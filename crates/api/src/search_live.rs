//! `POST /search/live` and `DELETE /search/live/{id}` (B-search-live): a
//! count of the new matches for the query a page shows, delivered as
//! `search_live` events on `/events` (decision B-search-recovery, part 4).
//!
//! - **In memory only.** A registration is the query as `/search` runs it
//!   ([`crate::search::base_query`]`(ast, visibility).query()`), with the
//!   visibility read at registration, a watermark and a counter. No store
//!   rows; a restart forgets them all.
//! - **Visibility can't be outlived.** Each registration remembers whether
//!   Adult was shown when it was made; every read of the registry first
//!   drops those made under another setting, so after "Show adult
//!   content" changes, every earlier registration is gone (the client
//!   registers again). Hidden, Adult and filter-hidden torrents never
//!   count: the stored query carries the clauses that leave them out, and
//!   quarantined or dropped torrents are never indexed (§20).
//! - **Matching** is the percolator's: after each commit it runs these as
//!   count-only entries ([`percolate_live`]) once the saved searches of
//!   that run are recorded and announced, on what's left of the same
//!   budget, with the same range cap. Live queries never delay an alert.
//! - **Delivery** is per session, never broadcast: each `/events`
//!   connection of the session that made a registration gets its count at
//!   most once per [`LiveConfig::every`], counts in between combined.
//! - **The same tier as the page:** the stem fallback (§8) is decided at
//!   registration as a search's lease decides it, so a query the page
//!   showed stemmed results for counts stem matches too.
//! - **Bounds:** [`LiveConfig::per_session`] per session (one more
//!   replaces the oldest), [`LiveConfig::total`] in all. A registration
//!   is released [`LiveConfig::grace`] after its session's last `/events`
//!   connection closes (so a reconnect keeps it, count and all), after
//!   [`LiveConfig::idle`] when no connection ever opened, or when a
//!   `content` settings save may have changed what's visible.
//! - **No `since` or `through` ids while Adult is hidden**, as with
//!   `torrents`' `max_id`: ids are sequential, so the gap between two
//!   would tell how many hidden torrents arrived.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use lodestar_search::{
    CancelToken, Due, Lease, Node, PercolateLimits, Result as SearchResult, SearchIndex, Sort,
    percolate,
};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::dto::auth::OkResponse;
use crate::dto::events::{SearchLiveEvent, SearchLiveRegistration, SearchLiveRequest};
use crate::error::{ApiError, ApiResult, internal};
use crate::events::{EventHub, session_of};
use crate::extract::{Json, Path, Peer, same_origin_json};
use crate::search::{AdultVisibility, Visibility, base_query, request_ast, tz_param};
use crate::{ApiState, unix_now};

/// How many live queries, for how long, and how often they're delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveConfig {
    /// Registrations per session; one more replaces its oldest.
    pub per_session: usize,
    /// Registrations in all; past it, a 429.
    pub total: usize,
    /// A registration made while its session has no `/events`
    /// connection is released after this long, unless one opens.
    pub idle: Duration,
    /// A registration is released this long after its session's last
    /// `/events` connection closes, unless one opens again: long enough
    /// for `EventSource`'s reconnect (5 s), short for a closed tab.
    pub grace: Duration,
    /// At most one `search_live` event per connection per registration
    /// this often (§10.13's throttle cadence).
    pub every: Duration,
}

impl Default for LiveConfig {
    fn default() -> Self {
        LiveConfig {
            per_session: 4,
            // Four for each of the hub's 64 streams.
            total: 256,
            idle: Duration::from_secs(600),
            grace: Duration::from_secs(30),
            every: Duration::from_secs(5),
        }
    }
}

/// The registry of live queries. The hub owns one ([`EventHub::live`]).
pub struct LiveQueries {
    config: LiveConfig,
    adult: AdultVisibility,
    state: Mutex<LiveState>,
}

impl std::fmt::Debug for LiveQueries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.lock();
        f.debug_struct("LiveQueries")
            .field("registrations", &st.regs.len())
            .field("sessions", &st.sessions.len())
            .finish()
    }
}

#[derive(Default)]
struct LiveState {
    /// The next internal key; keys are never reused.
    next_key: i64,
    regs: Vec<Reg>,
    sessions: HashMap<u64, Session>,
}

/// A session with `/events` connections open.
struct Session {
    streams: usize,
    /// Bumped when one of its registrations' counts grows.
    changed: watch::Sender<u64>,
}

struct Reg {
    /// The public id: random, so it names nothing about other sessions.
    id: String,
    /// The percolator's key for it.
    key: i64,
    session: u64,
    query: Arc<Node>,
    utc_offset: i32,
    /// Runs on the stem tier, as the page's search did.
    stems: bool,
    /// Whether Adult was shown when it was made.
    adult: bool,
    /// The highest id already counted.
    watermark: i64,
    matched: u64,
    /// When it's released, while its session has no `/events`
    /// connection; `None` while it has one.
    expires: Option<Instant>,
}

/// A query as a registration would store it: [`LiveQueries::prepare`].
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The query with its visibility clauses.
    pub query: Node,
    /// Whether Adult was shown when it was prepared.
    pub adult: bool,
}

/// One live query due to run, as [`LiveQueries::due`] hands it out.
#[derive(Debug, Clone)]
pub struct LiveDue {
    pub key: i64,
    pub watermark: i64,
    pub query: Arc<Node>,
    pub utc_offset: i32,
    pub stems: bool,
}

/// Why a registration was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveRefused {
    /// [`LiveConfig::total`] are registered.
    Full,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl LiveQueries {
    pub fn new(config: LiveConfig, adult: AdultVisibility) -> LiveQueries {
        LiveQueries {
            config,
            adult,
            state: Mutex::default(),
        }
    }

    pub fn config(&self) -> LiveConfig {
        self.config
    }

    fn lock(&self) -> MutexGuard<'_, LiveState> {
        lock(&self.state)
    }

    /// The registry under its lock, with what's no longer valid dropped:
    /// registrations made under another Adult setting than `shown`, and
    /// those past their expiry. Visibility is read by the caller, outside
    /// the lock (it may take the settings' own).
    fn screened(&self, shown: bool) -> MutexGuard<'_, LiveState> {
        let mut st = self.lock();
        let now = Instant::now();
        st.regs
            .retain(|r| r.adult == shown && r.expires.is_none_or(|t| now < t));
        st
    }

    /// Drops every registration: a `content` settings save may have
    /// changed what's visible (the client registers again). Eager, so an
    /// on-off-on change nobody saw in between still drops them.
    pub fn clear(&self) {
        self.lock().regs.clear();
    }

    /// Registrations held now.
    pub fn len(&self) -> usize {
        self.screened(self.adult.shown()).regs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `ast` as `/search` would run it now: with the visibility clauses
    /// of this moment, and the Adult setting they were made under.
    pub fn prepare(&self, ast: &Node, hidden: bool) -> Prepared {
        let adult = self.adult.shown();
        Prepared {
            query: base_query(ast, Visibility { hidden, adult }).query(),
            adult,
        }
    }

    /// [`LiveQueries::prepare`] and [`LiveQueries::insert`] on the exact
    /// tier.
    pub fn register(
        &self,
        session: u64,
        ast: &Node,
        hidden: bool,
        utc_offset: i32,
        since: i64,
    ) -> Result<SearchLiveRegistration, LiveRefused> {
        let p = self.prepare(ast, hidden);
        self.insert(session, p, utc_offset, false, since)
    }

    /// Registers `p` for `session`, counting matches after `since`. One
    /// prepared under a visibility that has changed since is dropped at
    /// the next read, before it can count.
    pub fn insert(
        &self,
        session: u64,
        p: Prepared,
        utc_offset: i32,
        stems: bool,
        since: i64,
    ) -> Result<SearchLiveRegistration, LiveRefused> {
        let Prepared { query, adult } = p;
        let shown = self.adult.shown();
        let mut st = self.screened(shown);
        let mine: Vec<usize> = (0..st.regs.len())
            .filter(|&i| st.regs[i].session == session)
            .collect();
        if mine.len() >= self.config.per_session.max(1) {
            // The oldest: registrations are kept in the order made.
            st.regs.remove(mine[0]);
        } else if st.regs.len() >= self.config.total {
            return Err(LiveRefused::Full);
        }
        let expires = match st.sessions.get(&session) {
            Some(s) if s.streams > 0 => None,
            _ => Some(Instant::now() + self.config.idle),
        };
        st.next_key += 1;
        let key = st.next_key;
        let id = format!("{:016x}", rand::random::<u64>());
        st.regs.push(Reg {
            id: id.clone(),
            key,
            session,
            query: Arc::new(query),
            utc_offset,
            stems,
            adult,
            watermark: since,
            matched: 0,
            expires,
        });
        Ok(SearchLiveRegistration {
            id,
            // Prepared while Adult was shown but hidden since: no id
            // (and the registration is dropped at the next read).
            since: (adult && shown).then_some(since),
        })
    }

    /// Releases `session`'s registration `id`; false when it has none by
    /// that id (another session's included).
    pub fn release(&self, session: u64, id: &str) -> bool {
        let mut st = self.screened(self.adult.shown());
        let before = st.regs.len();
        st.regs.retain(|r| !(r.session == session && r.id == id));
        st.regs.len() < before
    }

    /// An `/events` connection of `session` opened: its registrations no
    /// longer expire.
    pub(crate) fn opened(&self, session: u64) {
        let mut st = self.lock();
        st.sessions
            .entry(session)
            .or_insert_with(|| Session {
                streams: 0,
                changed: watch::channel(0).0,
            })
            .streams += 1;
        for r in st.regs.iter_mut().filter(|r| r.session == session) {
            r.expires = None;
        }
    }

    /// An `/events` connection of `session` closed: with its last one,
    /// its registrations are released after [`LiveConfig::grace`] unless
    /// one opens again.
    pub(crate) fn closed(&self, session: u64) {
        let mut st = self.lock();
        let Some(s) = st.sessions.get_mut(&session) else {
            return;
        };
        s.streams = s.streams.saturating_sub(1);
        if s.streams == 0 {
            st.sessions.remove(&session);
            let at = Instant::now() + self.config.grace;
            for r in st.regs.iter_mut().filter(|r| r.session == session) {
                r.expires = Some(at);
            }
        }
    }

    /// What tells `session`'s connections that a count grew; `None` when
    /// it has no connection open.
    pub(crate) fn watch(&self, session: u64) -> Option<watch::Receiver<u64>> {
        self.lock()
            .sessions
            .get(&session)
            .map(|s| s.changed.subscribe())
    }

    /// The registrations to run, most behind first.
    pub fn due(&self) -> Vec<LiveDue> {
        let st = self.screened(self.adult.shown());
        let mut due: Vec<LiveDue> = st
            .regs
            .iter()
            .map(|r| LiveDue {
                key: r.key,
                watermark: r.watermark,
                query: r.query.clone(),
                utc_offset: r.utc_offset,
                stems: r.stems,
            })
            .collect();
        due.sort_by_key(|d| (d.watermark, d.key));
        due
    }

    /// Adds a run's counts: `(key, from, through, count)` for each that
    /// ran from watermark `from`. A registration released, or dropped by
    /// a visibility change, since it was handed out is left alone, as is
    /// one whose watermark has moved (counted once, never twice).
    pub fn record(&self, ran: &[(i64, i64, i64, u64)]) {
        let mut st = self.screened(self.adult.shown());
        let st = &mut *st;
        let mut grew: Vec<u64> = Vec::new();
        for &(key, from, through, count) in ran {
            if let Some(r) = st
                .regs
                .iter_mut()
                .find(|r| r.key == key && r.watermark == from)
            {
                r.watermark = through;
                r.matched += count;
                if count > 0 && !grew.contains(&r.session) {
                    grew.push(r.session);
                }
            }
        }
        for session in grew {
            if let Some(s) = st.sessions.get(&session) {
                s.changed.send_modify(|v| *v = v.wrapping_add(1));
            }
        }
    }

    /// The events `session`'s connection has yet to see: each of its
    /// registrations whose count isn't what `sent` last told it.
    /// `sent` is that connection's own record.
    pub(crate) fn updates(
        &self,
        session: u64,
        sent: &mut HashMap<String, u64>,
    ) -> Vec<SearchLiveEvent> {
        let st = self.screened(self.adult.shown());
        let mut out = Vec::new();
        let mut held = Vec::new();
        for r in st.regs.iter().filter(|r| r.session == session) {
            held.push(r.id.as_str());
            if r.matched > 0 && sent.get(&r.id) != Some(&r.matched) {
                sent.insert(r.id.clone(), r.matched);
                out.push(SearchLiveEvent {
                    id: r.id.clone(),
                    matched: r.matched,
                    through: r.adult.then_some(r.watermark),
                });
            }
        }
        sent.retain(|id, _| held.contains(&id.as_str()));
        out
    }
}

/// What one run of the live queries did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveRun {
    /// Live queries that ran.
    pub ran: usize,
    /// Matches they counted.
    pub matched: u64,
    /// Another run straight away would find more to do.
    pub more: bool,
}

/// Runs `live`'s registrations against `index` up to `through`, as
/// count-only entries of [`percolate`], and adds their counts. The
/// percolator calls it after a run's saved searches, with the same token,
/// so they share one budget and live queries only get what's left. After
/// saved searches ran (`after_saved`), even the first can be stopped
/// inside its scan, so a nearly spent budget isn't overrun; otherwise the
/// first always completes, so a run that starts one finishes one.
pub fn percolate_live(
    index: &SearchIndex,
    live: &LiveQueries,
    through: i64,
    limits: PercolateLimits,
    now: i64,
    cancel: &CancelToken,
    after_saved: bool,
) -> SearchResult<LiveRun> {
    let due = live.due();
    if due.is_empty() {
        return Ok(LiveRun::default());
    }
    let entries: Vec<Due<'_>> = due
        .iter()
        .map(|d| Due {
            id: d.key,
            watermark: d.watermark,
            query: &d.query,
            utc_offset: d.utc_offset,
            count_only: true,
            stems: d.stems,
            stoppable: after_saved,
        })
        .collect();
    let p = percolate(index, &entries, through, limits, now, cancel)?;
    let from: HashMap<i64, i64> = due.iter().map(|d| (d.key, d.watermark)).collect();
    let ran: Vec<(i64, i64, i64, u64)> = p
        .matched
        .iter()
        .filter_map(|m| Some((m.id, *from.get(&m.id)?, m.through, m.count)))
        .collect();
    live.record(&ran);
    Ok(LiveRun {
        ran: ran.len(),
        matched: ran.iter().map(|r| r.3).sum(),
        more: p.more(),
    })
}

/// The highest torrent id the serving searcher can see now.
fn searchable(index: &SearchIndex) -> SearchResult<i64> {
    let limits = PercolateLimits {
        keep: 0,
        max_range: 1,
    };
    Ok(percolate(index, &[], i64::MAX, limits, 0, &CancelToken::new())?.snapshot_max)
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "events_unavailable",
        "Live updates aren't available right now.",
    )
}

fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "live_query_not_found",
        "There's no live query with that id.",
    )
}

/// Registers a live query: the count of new matches for `q` (or `ast`)
/// arrives as `search_live` events on `/events`, to this session only.
///
/// The query is validated and bounded as `/search` bounds it, and runs
/// with the same visibility: torrents a filter rule hid unless `hidden`,
/// and no Adult while "Show adult content" is off. Changing that setting
/// releases every live query: register again. A session holds at most 4
/// (a fifth replaces the oldest); one is released 30 s after the
/// session's last `/events` connection closes (a reconnect keeps it),
/// after 10 minutes if no connection opens, on a `content` settings save,
/// or with `DELETE /search/live/{id}`. Register again after a `settings`
/// event for `content`, or when a `search_live` id you hold is no longer
/// known (a 404 on `DELETE`). Swarm clauses (`seeders:`…) match nothing
/// at index time, so a query with one counts only what it can see then.
/// Where the percolator doesn't run (fixture mode), nothing is counted.
#[utoipa::path(
    operation_id = "registerSearchLive",
    post,
    path = "/search/live",
    tag = "search",
    request_body = SearchLiveRequest,
    responses(
        (status = 200, body = SearchLiveRegistration),
        (status = 400, body = crate::dto::error::ErrorEnvelope, description = "`query_syntax` (with `start` and `end`), `q_and_ast`, `invalid_ast` or `bad_tz`."),
        (status = 401, body = crate::dto::error::ErrorEnvelope, description = "No session (`login_required`)."),
        (status = 403, body = crate::dto::error::ErrorEnvelope, description = "`cross_origin`."),
        (status = 415, body = crate::dto::error::ErrorEnvelope, description = "`not_json`: send `Content-Type: application/json`."),
        (
            status = 429,
            body = crate::dto::error::ErrorEnvelope,
            description = "Too many live queries are registered (`too_many_live_queries`).",
            headers(("Retry-After" = u32, description = "Seconds to wait."))
        ),
        (status = 503, body = crate::dto::error::ErrorEnvelope, description = "Search is off (`search_disabled`), or live updates aren't served (`events_unavailable`)."),
    )
)]
pub(crate) async fn register(
    State(s): State<ApiState>,
    hub: Option<Extension<Arc<EventHub>>>,
    Peer(peer): Peer,
    headers: HeaderMap,
    Json(body): Json<SearchLiveRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let ast = request_ast(body.ast, body.q)?;
    let tz = tz_param(body.tz)?;
    let Some(Extension(hub)) = hub else {
        return Err(unavailable());
    };
    let Some(search) = s.index.clone() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "search_disabled",
            "Search is turned off.",
        ));
    };
    let session = session_of(&s.auth, &headers, peer);
    let prepared = hub.live().prepare(&ast, body.hidden.unwrap_or(false));
    let query = prepared.query.clone();
    // The tier the page's search took (§8's stem fallback, decided by the
    // count as a lease decides it), and where counting starts. A client
    // that goes away cancels it.
    let cancel = CancelToken::new();
    let _guard = cancel.cancel_on_drop();
    let (stems, since) = tokio::task::spawn_blocking(move || {
        let lease = Lease::new_at(
            search.index(),
            &query,
            unix_now(),
            tz * 60,
            Sort::Relevance,
            &cancel,
        )?;
        Ok::<_, lodestar_search::Error>((lease.stemmed(), searchable(lease.index())?))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    match hub.live().insert(session, prepared, tz * 60, stems, since) {
        Ok(reg) => Ok(axum::Json(reg).into_response()),
        Err(LiveRefused::Full) => {
            let mut res = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_live_queries",
                "Too many live queries are registered; try again shortly.",
            )
            .into_response();
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(60u32));
            Ok(res)
        }
    }
}

/// Releases one of this session's live queries.
#[utoipa::path(
    operation_id = "releaseSearchLive",
    delete,
    path = "/search/live/{id}",
    tag = "search",
    params(("id" = String, Path, description = "The registration's id.")),
    request_body(content = Object, description = "Any JSON, such as `{}`."),
    responses(
        (status = 200, body = OkResponse),
        (status = 403, body = crate::dto::error::ErrorEnvelope, description = "`cross_origin`."),
        (status = 415, body = crate::dto::error::ErrorEnvelope, description = "`not_json`: send `Content-Type: application/json`."),
        (status = 401, body = crate::dto::error::ErrorEnvelope, description = "No session (`login_required`)."),
        (status = 404, body = crate::dto::error::ErrorEnvelope, description = "`live_query_not_found`: none by that id for this session (it may have been released already)."),
        (status = 503, body = crate::dto::error::ErrorEnvelope, description = "Live updates aren't served (`events_unavailable`)."),
    )
)]
pub(crate) async fn release(
    State(s): State<ApiState>,
    hub: Option<Extension<Arc<EventHub>>>,
    Peer(peer): Peer,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<axum::Json<OkResponse>> {
    same_origin_json(&headers)?;
    let Some(Extension(hub)) = hub else {
        return Err(unavailable());
    };
    let session = session_of(&s.auth, &headers, peer);
    if id.len() > 64 || !hub.live().release(session, &id) {
        return Err(not_found());
    }
    Ok(axum::Json(OkResponse::OK))
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::Router;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use lodestar_bencode::{OwnedValue, to_vec};
    use lodestar_classify::Correction;
    use lodestar_metainfo::Info;
    use lodestar_search::parse;
    use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::events::{EventSources, EventsConfig};
    use crate::{Auth, router};

    /// "Show adult content", flipped by the test.
    fn adult() -> (Arc<AtomicBool>, AdultVisibility) {
        let shown = Arc::new(AtomicBool::new(false));
        let s = shown.clone();
        (
            shown,
            AdultVisibility::follow(move || s.load(Ordering::SeqCst)),
        )
    }

    fn live(config: LiveConfig) -> (Arc<AtomicBool>, LiveQueries) {
        let (shown, v) = adult();
        (shown, LiveQueries::new(config, v))
    }

    fn q(text: &str) -> Node {
        parse(text).unwrap()
    }

    fn ids(l: &LiveQueries, session: u64) -> Vec<String> {
        l.lock()
            .regs
            .iter()
            .filter(|r| r.session == session)
            .map(|r| r.id.clone())
            .collect()
    }

    #[test]
    fn a_fifth_replaces_the_oldest_and_the_total_is_capped() {
        let (_, l) = live(LiveConfig {
            total: 6,
            ..LiveConfig::default()
        });
        let made: Vec<String> = (0..5)
            .map(|i| l.register(1, &q(&format!("w{i}")), false, 0, 0).unwrap().id)
            .collect();
        assert_eq!(ids(&l, 1), made[1..]);
        l.register(2, &q("a"), false, 0, 0).unwrap();
        l.register(2, &q("b"), false, 0, 0).unwrap();
        // Six in all: a new session is refused, a full one still replaces.
        assert_eq!(l.register(3, &q("c"), false, 0, 0), Err(LiveRefused::Full));
        l.register(1, &q("d"), false, 0, 0).unwrap();
        assert_eq!(l.len(), 6);
        // Ids are random, not sequential.
        assert_ne!(made[0], made[1]);
        assert_eq!(made[0].len(), 16);
    }

    #[test]
    fn a_session_releases_only_its_own() {
        let (_, l) = live(LiveConfig::default());
        let a = l.register(1, &q("a"), false, 0, 0).unwrap().id;
        assert!(!l.release(2, &a));
        assert!(l.release(1, &a));
        assert!(!l.release(1, &a));
        assert!(l.is_empty());
    }

    #[test]
    fn the_stored_query_carries_the_visibility_clauses() {
        let (shown, l) = live(LiveConfig::default());
        let reg = l.register(1, &q("movie"), false, 0, 42).unwrap();
        assert_eq!(reg.since, None, "no ids while Adult is hidden");
        let query = l.due()[0].query.clone();
        assert_eq!(
            *query,
            base_query(
                &q("movie"),
                Visibility {
                    hidden: false,
                    adult: false
                }
            )
            .query()
        );
        assert!(
            query.to_query().contains("-type:adult"),
            "{}",
            query.to_query()
        );
        assert!(query.to_query().contains("hidden"), "{}", query.to_query());
        // Turning Adult on drops it; one made now carries no Adult clause
        // and says where it starts.
        shown.store(true, Ordering::SeqCst);
        assert!(l.is_empty());
        let reg = l.register(1, &q("movie"), true, 0, 42).unwrap();
        assert_eq!(reg.since, Some(42));
        assert_eq!(*l.due()[0].query, q("movie"));
        // And turning it off again drops that one.
        shown.store(false, Ordering::SeqCst);
        assert!(l.due().is_empty());
    }

    #[test]
    fn counts_are_recorded_once_and_delivered_when_they_change() {
        let (shown, l) = live(LiveConfig::default());
        l.opened(1);
        let rx = l.watch(1).unwrap();
        let a = l.register(1, &q("a"), false, 0, 10).unwrap().id;
        let key = l.due()[0].key;
        l.record(&[(key, 10, 20, 3)]);
        assert!(rx.has_changed().unwrap());
        // A stale run (from an old watermark) is ignored.
        l.record(&[(key, 10, 20, 3)]);
        // A run with nothing new moves the watermark but tells no one.
        let mut rx2 = l.watch(1).unwrap();
        rx2.mark_unchanged();
        l.record(&[(key, 20, 25, 0)]);
        assert!(!rx2.has_changed().unwrap());
        let mut sent = HashMap::new();
        let e = l.updates(1, &mut sent);
        assert_eq!(
            e,
            [SearchLiveEvent {
                id: a.clone(),
                matched: 3,
                through: None
            }]
        );
        assert!(l.updates(1, &mut sent).is_empty(), "unchanged: nothing");
        assert!(l.updates(2, &mut HashMap::new()).is_empty(), "not theirs");
        // A visibility change drops it, counts and all.
        shown.store(true, Ordering::SeqCst);
        l.record(&[(key, 25, 30, 1)]);
        assert!(l.updates(1, &mut sent).is_empty());
        assert!(sent.is_empty());
    }

    /// Waits, at most 10 s, until `l` holds `n`; a short sleep paces the
    /// polling.
    fn wait_for_len(l: &LiveQueries, n: usize) {
        let start = std::time::Instant::now();
        while l.len() != n {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "held {}",
                l.len()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The last connection closing releases a session's queries after the
    /// grace; a reconnect within it keeps them, counts and all.
    #[test]
    fn the_last_connection_closing_releases_a_sessions_queries() {
        let (_, l) = live(LiveConfig {
            grace: Duration::from_millis(200),
            ..LiveConfig::default()
        });
        l.opened(1);
        l.opened(1);
        l.opened(2);
        let a = l.register(1, &q("a"), false, 0, 0).unwrap().id;
        l.register(2, &q("b"), false, 0, 0).unwrap();
        l.closed(1);
        l.closed(2);
        assert!(
            l.lock()
                .regs
                .iter()
                .all(|r| (r.session == 1) == r.expires.is_none())
        );
        assert!(l.watch(2).is_none());
        // Session 1 reconnects: its count carries on.
        let key = l.lock().regs.iter().find(|r| r.id == a).unwrap().key;
        l.record(&[(key, 0, 9, 2)]);
        l.closed(1);
        l.opened(1);
        assert_eq!(matched(&l, &a), 2);
        // Session 2 never did: released after the grace.
        wait_for_len(&l, 1);
        assert_eq!(ids(&l, 1), [a]);
        // A close it never saw changes nothing.
        l.closed(9);
    }

    #[test]
    fn a_content_save_drops_every_registration() {
        let (_, l) = live(LiveConfig::default());
        l.register(1, &q("a"), false, 0, 0).unwrap();
        l.register(2, &q("b"), true, 0, 0).unwrap();
        l.clear();
        assert!(l.is_empty());
    }

    /// Without a connection, a registration lasts `idle`; with one, it
    /// doesn't expire.
    #[test]
    fn an_idle_registration_expires() {
        let (_, l) = live(LiveConfig {
            idle: Duration::from_millis(200),
            ..LiveConfig::default()
        });
        l.opened(2);
        let kept = l.register(2, &q("b"), false, 0, 0).unwrap().id;
        l.register(1, &q("a"), false, 0, 0).unwrap();
        wait_for_len(&l, 1);
        assert_eq!(ids(&l, 2), [kept]);
        // A connection opening stops the clock.
        l.register(3, &q("c"), false, 0, 0).unwrap();
        l.opened(3);
        assert!(l.lock().regs.iter().all(|r| r.expires.is_none()));
    }

    // The visibility tests: a corpus of torrents a filter rule hid, and
    // one of Adult torrents, give 0 while they're hidden.

    /// In id order: two films, one a filter rule hid, two detected Adult.
    const TORRENTS: [(&str, i64); 5] = [
        ("Big.Movie.2019.1080p.mkv", 0),
        ("Other.Movie.2018.720p.mkv", 0),
        ("Secret.Movie.2019.1080p.mkv", flags::HIDDEN),
        ("Studio.Scene.XXX.1080p.mp4", 0),
        ("Another.Scene.XXX.720p.mp4", 0),
    ];

    fn raw(name: &str) -> Vec<u8> {
        let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
        to_vec(&OwnedValue::Dict(
            [
                ("length", OwnedValue::Int(1 << 30)),
                ("name", b(name)),
                ("piece length", OwnedValue::Int(1 << 16)),
                ("pieces", OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
        ))
    }

    /// The torrents stored and enriched; indexed when the test commits.
    fn corpus(dir: &std::path::Path) -> Store {
        let mut store = Store::open(dir.join("t.db"), StoreConfig::default()).unwrap();
        let raws: Vec<Vec<u8>> = TORRENTS.iter().map(|(n, _)| raw(n)).collect();
        let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
        let batch: Vec<NewTorrent<'_>> = infos
            .iter()
            .zip(&raws)
            .zip(TORRENTS)
            .map(|((info, raw), (_, extra_flags))| NewTorrent {
                info,
                raw_info: Some(raw),
                discovered_at: 1_700_000_000,
                metadata_at: 1_700_000_100,
                extra_flags,
                source: SourceId::new(0).unwrap(),
            })
            .collect();
        store.insert(&batch).unwrap();
        for id in 1..=TORRENTS.len() as i64 {
            let row = store.torrent(id).unwrap().unwrap();
            let tree = store.files(id).unwrap().unwrap();
            let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
            let (e, _) = lodestar_filter::reclassify::enrich_corrected(
                &row.name,
                &files,
                Correction::default(),
            );
            store.set_enrichments(&[(id, e)]).unwrap();
        }
        store
    }

    const LIMITS: PercolateLimits = PercolateLimits {
        keep: 1000,
        max_range: 100_000,
    };

    fn matched(l: &LiveQueries, id: &str) -> u64 {
        l.lock().regs.iter().find(|r| r.id == id).unwrap().matched
    }

    #[test]
    fn a_commit_counts_only_what_search_would_show() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = corpus(dir.path());
        let index = SearchIndex::in_memory().unwrap();
        let (shown, l) = live(LiveConfig::default());
        // Registered before anything is searchable.
        assert_eq!(searchable(&index).unwrap(), 0);
        let reg = |text: &str, hidden: bool| l.register(1, &q(text), hidden, 0, 0).unwrap().id;
        let movie = reg("movie", false);
        let secret = reg("secret", false);
        let secret_asked = reg("secret", true);
        let scene = reg("scene", false);
        // A second session for the explicit `type:adult`.
        let adult_asked = l.register(2, &q("type:adult"), true, 0, 0).unwrap().id;
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 100).unwrap();
        w.commit().unwrap();
        let through = searchable(&index).unwrap();
        assert_eq!(through, 5);
        let run =
            percolate_live(&index, &l, through, LIMITS, 0, &CancelToken::new(), false).unwrap();
        assert_eq!((run.ran, run.matched, run.more), (5, 3, false));
        assert_eq!(matched(&l, &movie), 2, "the hidden one isn't counted");
        assert_eq!(matched(&l, &secret), 0, "hidden only: 0");
        assert_eq!(matched(&l, &secret_asked), 1, "asked for with hidden=true");
        assert_eq!(matched(&l, &scene), 0, "Adult only: 0");
        assert_eq!(matched(&l, &adult_asked), 0, "type:adult while hidden: 0");
        // Nothing new: nothing runs again.
        let again =
            percolate_live(&index, &l, through, LIMITS, 0, &CancelToken::new(), false).unwrap();
        assert_eq!(again.matched, 0);
        assert_eq!(matched(&l, &movie), 2);
        // Showing Adult drops every registration; one made now counts
        // Adult, from where it starts.
        shown.store(true, Ordering::SeqCst);
        assert!(l.is_empty());
        let scene = l.register(1, &q("scene"), false, 0, 3).unwrap().id;
        percolate_live(&index, &l, through, LIMITS, 0, &CancelToken::new(), false).unwrap();
        assert_eq!(matched(&l, &scene), 2);
    }

    /// A query the page's search served from the stem tier counts stem
    /// matches too; one it served exact doesn't.
    #[test]
    fn the_stem_tier_counts_as_the_page_did() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = corpus(dir.path());
        let index = SearchIndex::in_memory().unwrap();
        let (_, l) = live(LiveConfig::default());
        let reg = |stems| {
            let p = l.prepare(&q("movies"), false);
            l.insert(1, p, 0, stems, 0).unwrap().id
        };
        let (exact, stemmed) = (reg(false), reg(true));
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 100).unwrap();
        w.commit().unwrap();
        percolate_live(&index, &l, 5, LIMITS, 0, &CancelToken::new(), false).unwrap();
        assert_eq!((matched(&l, &exact), matched(&l, &stemmed)), (0, 2));
    }

    #[test]
    fn a_spent_budget_runs_nothing_and_leaves_it_for_later() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = corpus(dir.path());
        let index = SearchIndex::in_memory().unwrap();
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 100).unwrap();
        w.commit().unwrap();
        let (_, l) = live(LiveConfig::default());
        let movie = l.register(1, &q("movie"), false, 0, 0).unwrap().id;
        let spent = CancelToken::new().with_budget(Duration::ZERO);
        let run = percolate_live(&index, &l, 5, LIMITS, 0, &spent, false).unwrap();
        assert_eq!((run.ran, run.more), (0, true));
        let run = percolate_live(&index, &l, 5, LIMITS, 0, &CancelToken::new(), false).unwrap();
        assert_eq!((run.ran, run.more), (1, false));
        assert_eq!(matched(&l, &movie), 2);
    }

    // The endpoints.

    struct Http {
        app: Router,
        auth: Arc<Auth>,
        hub: Arc<EventHub>,
        _dir: tempfile::TempDir,
    }

    fn http() -> Http {
        let dir = tempfile::tempdir().unwrap();
        let mut store = corpus(dir.path());
        let index = SearchIndex::in_memory().unwrap();
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 100).unwrap();
        w.commit().unwrap();
        drop((w, store));
        let db = dir.path().join("t.db");
        let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
        let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
        let hub = EventHub::start(EventsConfig::default(), EventSources::default());
        let app = router(ApiState {
            auth: auth.clone(),
            index: Some(index.into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            user_data: None,
            hot: None,
            events: None,
            adult: AdultVisibility::default(),
            insights: None,
        })
        .layer(Extension(hub.clone()));
        Http {
            app,
            auth,
            hub,
            _dir: dir,
        }
    }

    fn login(auth: &Auth) -> String {
        let set = match auth.setup_token() {
            Some(token) => auth
                .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
                .unwrap(),
            None => auth
                .login([127, 0, 0, 1].into(), "correct horse battery")
                .unwrap(),
        };
        set.split(';').next().unwrap().to_string()
    }

    async fn call(h: &Http, method: &str, uri: &str, cookie: &str, body: Value) -> (u16, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if !cookie.is_empty() {
            req = req.header(header::COOKIE, cookie);
        }
        let mut req = req.body(Body::from(body.to_string())).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 0, 7, 1], 1))));
        let resp = h.app.clone().oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn register_and_release_over_http() {
        let h = http();
        let a = login(&h.auth);
        let b = login(&h.auth);
        let (status, body) = call(&h, "POST", "/search/live", "", json!({"q": "movie"})).await;
        assert_eq!(
            (status, &body["error"]["code"]),
            (401, &json!("login_required"))
        );
        let (status, body) = call(
            &h,
            "POST",
            "/search/live",
            &a,
            json!({"q": "movie", "tz": 60}),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["since"], Value::Null, "Adult hidden: no id");
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(h.hub.live().len(), 1);
        let due = h.hub.live().due();
        assert_eq!((due[0].utc_offset, due[0].stems), (3600, false));
        // "movies" finds nothing exact but "Movie" by its stem: the page
        // showed stemmed results, so the registration counts them.
        let (status, _) = call(&h, "POST", "/search/live", &a, json!({"q": "movies"})).await;
        assert_eq!(status, 200);
        assert!(h.hub.live().due().iter().any(|d| d.stems));
        // Validated as `/search` validates.
        for (bad, code) in [
            (json!({"q": "("}), "query_syntax"),
            (json!({"q": "x".repeat(5000)}), "query_syntax"),
            (
                json!({"q": "a", "ast": serde_json::to_value(q("a")).unwrap()}),
                "q_and_ast",
            ),
            (json!({"q": "a", "tz": 9999}), "bad_tz"),
        ] {
            let (status, body) = call(&h, "POST", "/search/live", &a, bad).await;
            assert_eq!(
                (status, body["error"]["code"].as_str()),
                (400, Some(code)),
                "{body}"
            );
        }
        // Another session can't release it.
        let uri = format!("/search/live/{id}");
        let (status, body) = call(&h, "DELETE", &uri, &b, Value::Null).await;
        assert_eq!(
            (status, &body["error"]["code"]),
            (404, &json!("live_query_not_found"))
        );
        let (status, body) = call(&h, "DELETE", &uri, &a, Value::Null).await;
        assert_eq!((status, body), (200, json!({"ok": true})));
        assert_eq!(h.hub.live().len(), 1);
        let (status, _) = call(&h, "DELETE", &uri, &a, Value::Null).await;
        assert_eq!(status, 404);
        h.hub.close();
    }
}
