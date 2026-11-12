//! `/torrents/{hash}`: one torrent's details and its file list. Display
//! data comes from SQLite, not the index (§8).

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::time::Instant;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lodestar_metainfo::{FileAttrs, InfoHash, Text};
use lodestar_search::flag_names;
use lodestar_search::query::Network;
use lodestar_store::claims::{AdultEnrichment as StoredMatch, Claims, MAX_FILES, MAX_NAME};
use lodestar_store::unverified::StoredClaims;
use lodestar_store::{FileTree, MAX_SOURCES, SourceId, Store, TorrentRow, flags as store_flags};
use tokio::sync::OnceCell;
use utoipa::OpenApi;

use crate::auth::Access;
use crate::dto::error::ErrorEnvelope;
use crate::dto::ops::ConsentClass;
use crate::dto::search::{HitSource, SearchHit};
use crate::dto::torrents::{
    AdultEnrichment, ClaimDifferences, ClaimedNumber, ClaimedText, EnrichmentSource, FileEntry,
    FilesPage, FilesQuery, Identification, IdentifiedAs, IdentifiedId, IdentifiedTitle, Provenance,
    ProvenanceSource, TorrentDetail,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{ClientIp, Path, Query};
use crate::ops::OpsApi;
use crate::swarm::{Detail, Summary};
use crate::{ApiState, auth, swarm};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(detail, files))]
pub(crate) struct ApiDoc;

/// Files in the detail response; more via `/files` (§9).
pub const DETAIL_FILES: usize = 500;
const MAX_FILES_PAGE: usize = 1000;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/torrents/{hash}", get(detail))
            .route("/torrents/{hash}/files", get(files)),
    )
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn parse_hash(s: &str) -> ApiResult<InfoHash> {
    let bad = || {
        ApiError::bad_request("bad_hash", "Expected a 40-character hex info hash.").field("hash")
    };
    if s.len() != 40 {
        return Err(bad());
    }
    let mut out = [0u8; 20];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2).ok_or_else(bad)?, 16).map_err(|_| bad())?;
    }
    Ok(InfoHash(out))
}

/// Who a provenance answer is for (§18.7). Private sources' names and
/// claims go to the admin session only: never to an API key or feed
/// token, and so never to Torznab, feeds, exports or any public route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Viewer {
    /// The signed-in browser, or the trusted proxy's user.
    Admin,
    /// Anyone else: an API key or a feed token.
    Public,
}

impl Viewer {
    /// From the request's [`Access`], which [`auth::protected`] and
    /// [`auth::key_or_session`] both put there. A request without one
    /// (a handler called as a function, or mounted without a guard)
    /// fails closed: it's public.
    pub(crate) fn of(access: Option<&Access>) -> Viewer {
        match access {
            Some(Access::Session) => Viewer::Admin,
            None | Some(Access::Key { .. } | Access::Feed { .. }) => Viewer::Public,
        }
    }
}

/// A source plugin of this build, as provenance shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginName {
    pub name: String,
    /// Its consent class isn't `account` (§18.7: Prowlarr, Jackett,
    /// private-tracker announces and your own clients are), and its
    /// manifest doesn't say its finds are private (Sonarr and Radarr
    /// webhooks). Its results may be public.
    pub public: bool,
    /// The hosts its manifest contacts, as `protocol://host`: the only
    /// places an adult-enrichment link of its may point.
    pub endpoints: Vec<String>,
    /// Everything it finds is Adult (its manifest's `always_adult`).
    pub always_adult: bool,
}

/// Display names and privacy by manifest id.
pub(crate) type PluginNames = HashMap<String, PluginName>;

/// How long a failed listing is kept before a request asks again, so a
/// pipeline whose `sources()` keeps failing takes the writer's lock at
/// most once per this, not once per request. Meanwhile every source
/// shows by plugin id and counts as private (a public caller sees none).
const NAMES_RETRY: Duration = Duration::from_secs(5);

/// How long a listing may take before it counts as failed, so a
/// `sources()` that hangs (the writer's lock held elsewhere) holds up
/// each search with a viewer for at most this, then backs off.
const NAMES_TIMEOUT: Duration = Duration::from_secs(10);

/// One pipeline's names: listed once, single-flight. Manifests are fixed
/// for the life of the process, so a listing that succeeds is kept for
/// good; one that fails, times out or panics is retried after `retry`.
#[derive(Debug)]
struct NamesSlot {
    names: OnceCell<Arc<PluginNames>>,
    /// When the last listing failed. The listing holds it until it has
    /// an answer, so the requests that arrive meanwhile wait for that
    /// answer rather than each listing again.
    failed: Arc<tokio::sync::Mutex<Option<Instant>>>,
    retry: Duration,
    timeout: Duration,
}

impl NamesSlot {
    fn new(retry: Duration, timeout: Duration) -> NamesSlot {
        NamesSlot {
            names: OnceCell::new(),
            failed: Arc::new(tokio::sync::Mutex::new(None)),
            retry,
            timeout,
        }
    }

    /// The names, from `list` at most once at a time; empty while the
    /// last listing failed less than `retry` ago.
    async fn get<F, Fut>(self: &Arc<Self>, list: F) -> Arc<PluginNames>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Option<PluginNames>> + Send + 'static,
    {
        if let Some(names) = self.names.get() {
            return names.clone();
        }
        let failed = self.failed.clone().lock_owned().await;
        // Listed while this request waited.
        if let Some(names) = self.names.get() {
            return names.clone();
        }
        if failed.is_some_and(|at| at.elapsed() < self.retry) {
            return Arc::default();
        }
        // Detached, with the lock: a request dropped mid-listing (its
        // client gone) leaves the listing running, `sources()`'s blocking
        // job with it, and the next request waits for its answer instead
        // of starting a second one beside it.
        let slot = self.clone();
        let listing = tokio::spawn(async move {
            let mut failed = failed;
            // Its own task, so a panic in it is a failure like any other.
            let job = tokio::spawn(list());
            let abort = job.abort_handle();
            let answer = match tokio::time::timeout(slot.timeout, job).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_panicked)) => None,
                Err(_elapsed) => {
                    abort.abort();
                    None
                }
            };
            match answer {
                Some(names) => {
                    let names = Arc::new(names);
                    // Only the holder of `failed` sets it, so this can't
                    // race another set.
                    let _ = slot.names.set(names.clone());
                    *failed = None;
                    names
                }
                None => {
                    *failed = Some(Instant::now());
                    Arc::default()
                }
            }
        });
        // Only the runtime shutting down fails this join.
        listing.await.unwrap_or_default()
    }
}

/// Each live pipeline's slot, by pointer.
type NamesCache = Vec<(Weak<dyn OpsApi>, Arc<NamesSlot>)>;
static PLUGIN_NAMES: Mutex<NamesCache> = Mutex::new(Vec::new());

/// The plugins of this build by manifest id; empty when no pipeline is
/// served or it couldn't list them (then every source shows by id and
/// counts as private, so a public caller sees none).
pub(crate) async fn plugin_names(s: &ApiState) -> Arc<PluginNames> {
    match s.ops.clone() {
        Some(ops) => names_for(ops).await,
        None => Arc::default(),
    }
}

/// [`plugin_names`] for the pipeline `ops`, from its slot.
async fn names_for(ops: Arc<dyn OpsApi>) -> Arc<PluginNames> {
    let key = Arc::downgrade(&ops);
    let slot = {
        let mut cache = PLUGIN_NAMES.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(w, _)| w.strong_count() > 0);
        match cache.iter().find(|(w, _)| Weak::ptr_eq(w, &key)) {
            Some((_, slot)) => slot.clone(),
            None => {
                let slot = Arc::new(NamesSlot::new(NAMES_RETRY, NAMES_TIMEOUT));
                cache.push((key, slot.clone()));
                slot
            }
        }
    };
    slot.get(move || async move {
        let list = ops.sources().await.ok()?;
        Some(list.into_iter().map(plugin_name).collect())
    })
    .await
}

/// A listed source's name, and whether a public viewer may see it: never
/// an account's, nor one whose finds reflect your own setup (§18.7: your
/// clients, your indexers, the releases your Sonarr or Radarr grabbed),
/// even on a torrent that's also stored publicly.
fn plugin_name(src: crate::ops::Source) -> (String, PluginName) {
    let public = src.consent != ConsentClass::Account && !src.finds_private;
    (
        src.plugin,
        PluginName {
            name: src.name,
            public,
            endpoints: src.endpoints,
            always_adult: src.always_adult,
        },
    )
}

/// One registered source, as provenance shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceEntry {
    pub id: u8,
    pub plugin: String,
    pub name: String,
    /// See [`PluginName::public`]; `None` for a plugin this build doesn't
    /// run (or no pipeline is served), whose class isn't known: a public
    /// caller sees none of those.
    pub public: Option<bool>,
    /// Everything it finds is Adult; `None` when this build doesn't run
    /// its plugin (or no pipeline is served), whose manifest isn't known.
    pub always_adult: Option<bool>,
}

/// The store's sources by id, named from [`plugin_names`].
#[derive(Debug, Clone, Default)]
pub(crate) struct Catalogue {
    by_id: Vec<Option<SourceEntry>>,
}

impl Catalogue {
    pub(crate) fn read(store: &Store, names: &PluginNames) -> lodestar_store::Result<Catalogue> {
        let mut by_id = vec![None; usize::from(MAX_SOURCES)];
        for row in store.sources()? {
            let known = names.get(&row.plugin);
            by_id[usize::from(row.id.get())] = Some(SourceEntry {
                id: row.id.get(),
                name: known.map_or_else(|| row.plugin.clone(), |n| n.name.clone()),
                public: known.map(|n| n.public),
                always_adult: known.map(|n| n.always_adult),
                plugin: row.plugin,
            });
        }
        Ok(Catalogue { by_id })
    }

    /// A catalogue of `entries`, for tests.
    #[cfg(test)]
    pub(crate) fn of(entries: Vec<SourceEntry>) -> Catalogue {
        let mut by_id = vec![None; usize::from(MAX_SOURCES)];
        for e in entries {
            let i = usize::from(e.id);
            by_id[i] = Some(e);
        }
        Catalogue { by_id }
    }

    /// The registered sources whose every find is Adult, as a bitset over
    /// source ids: the same rule `AdultSources::of` applies to the
    /// pipeline's manifests (X-always-adult-detail-mask), read from the
    /// listing the host always serves. Only the known ones: a source
    /// whose manifest isn't known isn't in it ([`Catalogue::adult_or_unknown`]).
    pub(crate) fn always_adult_bits(&self) -> u64 {
        self.by_id
            .iter()
            .flatten()
            .filter(|e| e.always_adult == Some(true))
            .fold(0, |bits, e| {
                bits | 1u64.checked_shl(u32::from(e.id)).unwrap_or(0)
            })
    }

    /// Of `bits` (a torrent's finders), the sources that are always adult
    /// **or can't be confirmed not to be**: a bit with no registered
    /// source, or a source whose plugin this build doesn't run. For a
    /// check that must fail closed.
    pub(crate) fn adult_or_unknown(&self, bits: u64) -> u64 {
        (0..MAX_SOURCES)
            .filter(|&id| bits & (1u64 << id) != 0)
            .filter(|&id| self.get(id).is_none_or(|e| e.always_adult != Some(false)))
            .fold(0, |out, id| out | (1u64 << id))
    }

    pub(crate) fn get(&self, id: u8) -> Option<&SourceEntry> {
        self.by_id.get(usize::from(id)).and_then(Option::as_ref)
    }

    /// The source with manifest id `plugin`.
    pub(crate) fn by_plugin(&self, plugin: &str) -> Option<&SourceEntry> {
        self.by_id.iter().flatten().find(|e| e.plugin == plugin)
    }

    /// Whether `viewer` may see the name `plugin` (a `source` facet
    /// value): always the admin; anyone else only a known public source.
    pub(crate) fn shows_plugin(&self, plugin: &str, viewer: Viewer) -> bool {
        viewer == Viewer::Admin
            || self
                .by_plugin(plugin)
                .is_some_and(|e| e.public == Some(true))
    }

    /// The ids in `bits`, the first source first, then by id.
    fn ordered(bits: u64, first: Option<SourceId>) -> impl Iterator<Item = u8> {
        let first = first.map(SourceId::get).filter(|f| bits & (1u64 << f) != 0);
        first
            .into_iter()
            .chain((0..MAX_SOURCES).filter(move |i| bits & (1u64 << i) != 0 && Some(*i) != first))
    }

    /// `row`'s sources `viewer` may see, the first first, and whether
    /// any was left out. A `private_source` torrent's are all private.
    ///
    /// Whether a source's name is shown depends on the torrent and the
    /// source's class only, so a hit and the detail view always agree; a
    /// private claim from a public source is withheld on its own
    /// ([`Catalogue::provenance`]).
    fn visible<'a>(
        &'a self,
        row: &TorrentRow,
        viewer: Viewer,
    ) -> (Vec<(&'a SourceEntry, bool)>, bool) {
        let private_item = row.flags & store_flags::PRIVATE_SOURCE != 0;
        let mut shown = Vec::new();
        let mut withheld = false;
        for id in Self::ordered(row.sources, row.first_source) {
            // A bit with no source row isn't one the store wrote.
            let Some(e) = self.get(id) else { continue };
            // Known to be private, which the admin is told.
            let private = private_item || e.public == Some(false);
            if (private || e.public.is_none()) && viewer == Viewer::Public {
                withheld = true;
            } else {
                shown.push((e, private));
            }
        }
        (shown, withheld)
    }

    /// A hit's source: the first one `viewer` may see.
    pub(crate) fn hit_source(&self, row: &TorrentRow, viewer: Viewer) -> Option<HitSource> {
        let (shown, _) = self.visible(row, viewer);
        shown.first().map(|(e, _)| HitSource {
            name: e.name.clone(),
            plugin: e.plugin.clone(),
        })
    }

    /// The detail view's provenance of `row`, from its stored claims.
    pub(crate) fn provenance(
        &self,
        row: &TorrentRow,
        claims: &[StoredClaims],
        viewer: Viewer,
    ) -> Provenance {
        let claim_of = |id: u8| claims.iter().find(|c| c.source.get() == id);
        let (shown, mut withheld) = self.visible(row, viewer);
        let first = row.first_source.map(SourceId::get);
        let mut sources = Vec::with_capacity(shown.len());
        for (e, private) in shown {
            let stored = claim_of(e.id);
            let private_claim = stored.is_some_and(|c| c.private_source);
            // A public source's name stays; its private claim doesn't.
            let claim = if private_claim && viewer == Viewer::Public {
                withheld = true;
                None
            } else {
                stored
                    .and_then(|c| c.claims.as_ref())
                    .map(|c| differences(row, c))
            };
            sources.push(ProvenanceSource {
                claim,
                first: first == Some(e.id),
                first_seen_at: (first == Some(e.id)).then_some(row.discovered_at),
                id: e.id,
                name: e.name.clone(),
                plugin: e.plugin.clone(),
                // Only the admin is told a claim was private.
                private: private || (private_claim && viewer == Viewer::Admin),
            });
        }
        // Claims from a source that's no longer in the bitset (purged
        // since) aren't shown; to a public caller, nor is their existence.
        if viewer == Viewer::Public && claims.iter().any(|c| c.private_source) {
            withheld = true;
        }
        Provenance {
            network: Network::Bt.as_str().to_owned(),
            sources,
            verified: true,
            withheld,
        }
    }
}

/// `s` cut to [`MAX_NAME`] bytes on a character boundary.
fn bounded(s: &str) -> String {
    let mut end = s.len().min(MAX_NAME);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// What `c` claims that `row`'s verified metadata contradicts (§18.5).
pub(crate) fn differences(row: &TorrentRow, c: &Claims) -> ClaimDifferences {
    let name = c
        .name
        .as_deref()
        .filter(|n| *n != row.name)
        .map(|n| ClaimedText {
            claimed: bounded(n),
            verified: bounded(&row.name),
        });
    let size = c.size.filter(|s| *s != row.size).map(|s| ClaimedNumber {
        claimed: s,
        verified: row.size,
    });
    let claimed = c.files.len() as u64;
    // A list cut at the limit says "at least this many".
    let cut = claimed >= MAX_FILES as u64 && row.file_count >= claimed;
    let file_count = (claimed > 0 && claimed != row.file_count && !cut).then_some(ClaimedNumber {
        claimed,
        verified: row.file_count,
    });
    ClaimDifferences {
        matches: name.is_none() && size.is_none() && file_count.is_none(),
        file_count,
        name,
        size,
    }
}

fn flags(row: &TorrentRow) -> Vec<String> {
    flag_names(row.flags).map(str::to_string).collect()
}

/// A search result row. The classification fields are filled from the
/// stored enrichment by `search::enrich`.
pub(crate) fn hit(
    row: &TorrentRow,
    score: f32,
    matched_files: Option<Vec<u64>>,
    swarm: Summary,
) -> SearchHit {
    SearchHit {
        adult_kind: None,
        completed: swarm.completed,
        composition: Vec::new(),
        content_type: None,
        discovered_at: row.discovered_at,
        files: row.file_count,
        flags: flags(row),
        hash: hex(&row.info_hash.0),
        hash_v2: row.info_hash_v2.map(|h| hex(&h.0)),
        id: row.id,
        junk_reasons: Vec::new(),
        junk_score: None,
        last_scrape_at: swarm.last_scrape_at,
        last_seen_at: swarm.last_seen_at,
        leechers: swarm.leechers,
        matched_files,
        metadata_at: row.metadata_at,
        name: row.name.clone(),
        piece_len: row.piece_len,
        resolved_via: None,
        // As `json!` widened it.
        score: f64::from(score),
        seeders: swarm.seeders,
        seeders_approx: swarm.seeders_approx,
        size: row.size,
        source: None,
        swarm_suspect: swarm.swarm_suspect,
        title: None,
        trackers_checked: swarm.trackers_checked,
        trackers_reporting: swarm.trackers_reporting,
        verified: None,
    }
}

fn file_entry(tree: &FileTree, f: &lodestar_metainfo::File) -> FileEntry {
    let path: Vec<String> = f
        .path
        .iter()
        .map(|seg| {
            Text {
                raw: seg.clone(),
                encoding: tree.encoding,
            }
            .decode()
            .into_owned()
        })
        .collect();
    let mut attrs = Vec::new();
    for (bit, name) in [
        (FileAttrs::PADDING, "padding"),
        (FileAttrs::HIDDEN, "hidden"),
        (FileAttrs::EXECUTABLE, "executable"),
        (FileAttrs::SYMLINK, "symlink"),
    ] {
        if f.attrs.contains(bit) {
            attrs.push(name.to_string());
        }
    }
    FileEntry {
        attrs,
        path: path.join("/"),
        size: f.size,
    }
}

/// Reads a torrent and its files by any swarm hash. An Adult torrent
/// while "Show adult content" is off (`adult_shown`) is a 404
/// `adult_hidden`, so a pasted hash doesn't get round the preference
/// (§20.9).
fn load(s: &ApiState, hash: &str, adult_shown: bool) -> ApiResult<(TorrentRow, FileTree)> {
    let hash = parse_hash(hash)?;
    let mut store = s.reader.lock().expect("reader lock");
    let not_found = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "torrent_not_found",
            "There's no torrent with that hash.",
        )
    };
    let id = store.find(&hash).map_err(internal)?.ok_or_else(not_found)?;
    if !adult_shown
        && store
            .enrichment(id)
            .map_err(internal)?
            .is_some_and(|e| crate::search::is_adult(&e))
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "adult_hidden",
            "This torrent is Adult, which is hidden: turn on Show adult content in Settings to see it.",
        ));
    }
    let row = store.torrent(id).map_err(internal)?.ok_or_else(not_found)?;
    let tree = store.files(id).map_err(internal)?.ok_or_else(not_found)?;
    Ok((row, tree))
}

/// The detail view: the summary, the swarm section and the first files.
fn detail_of(
    row: &TorrentRow,
    tree: &FileTree,
    swarm: Summary,
    more: Detail,
    provenance: Provenance,
    adult_enrichment: Option<AdultEnrichment>,
    identified_as: Option<IdentifiedAs>,
) -> TorrentDetail {
    TorrentDetail {
        adult_enrichment,
        identified_as,
        completed: swarm.completed,
        dht_peers: more.dht_peers,
        dht_seeds: more.dht_seeds,
        discovered_at: row.discovered_at,
        file_list: tree
            .files
            .iter()
            .take(DETAIL_FILES)
            .map(|f| file_entry(tree, f))
            .collect(),
        file_list_total: tree.files.len(),
        files: row.file_count,
        flags: flags(row),
        hash: hex(&row.info_hash.0),
        hash_v2: row.info_hash_v2.map(|h| hex(&h.0)),
        id: row.id,
        last_scrape_at: swarm.last_scrape_at,
        last_seen_at: swarm.last_seen_at,
        leechers: swarm.leechers,
        magnet: format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            hex(&row.info_hash.0),
            percent_encode(&row.name)
        ),
        metadata_at: row.metadata_at,
        name: row.name.clone(),
        piece_len: row.piece_len,
        provenance,
        seeders: swarm.seeders,
        seeders_approx: swarm.seeders_approx,
        size: row.size,
        swarm_history: more.swarm_history,
        swarm_suspect: swarm.swarm_suspect,
        swarm_trackers: more.swarm_trackers,
        swarm_trend: more.swarm_trend,
        trackers_checked: swarm.trackers_checked,
        trackers_reporting: swarm.trackers_reporting,
    }
}

/// One torrent by any of its swarm hashes.
#[utoipa::path(
    operation_id = "getTorrent",
    get,
    path = "/torrents/{hash}",
    tag = "torrents",
    params(("hash" = String, Path, description = "A 40-character hex info hash.")),
    responses(
        (status = 200, body = TorrentDetail),
        (status = 404, body = ErrorEnvelope, description = "`torrent_not_found`, or `adult_hidden`: an Adult torrent while \"Show adult content\" is off."),
    )
)]
pub(crate) async fn detail(
    State(s): State<ApiState>,
    access: Option<Extension<Access>>,
    interest: Option<Extension<DrawerInterest>>,
    ClientIp(client): ClientIp,
    Path(hash): Path<String>,
) -> ApiResult<Json<TorrentDetail>> {
    let viewer = Viewer::of(access.as_ref().map(|a| &a.0));
    // Read once, so the 404, the section and the lookup agree.
    let adult_shown = s.adult.shown();
    let names = plugin_names(&s).await;
    // The drawer's mask, when the host installs one; the catalogue's,
    // from the manifests, always (X-always-adult-detail-mask). Both come
    // from the same manifests, so they agree; the union is used anyway.
    let drawer_adult = interest.as_ref().map_or(0, |i| i.0.0.always_adult);
    let (detail, wanted) = tokio::task::spawn_blocking(move || {
        let (row, tree) = load(&s, &hash, adult_shown)?;
        // Opening a torrent is interest: scrape it soon (§15).
        if let Some(e) = &s.events {
            e.interest(row.id);
        }
        let store = s.reader.lock().expect("reader lock");
        let summary = swarm::summary(&store, s.hot.as_deref(), &row).map_err(internal)?;
        let more = swarm::detail(&store, &summary, row.id).map_err(internal)?;
        let catalogue = Catalogue::read(&store, &names).map_err(internal)?;
        let always_adult = catalogue.always_adult_bits() | drawer_adult;
        let claims = store.torrent_claims(row.id).map_err(internal)?;
        let provenance = catalogue.provenance(&row, &claims, viewer);
        // Only for the admin with "Show adult content" on (§20.9), only
        // for a torrent that could be adult, and never for one a filter or
        // the core holds back.
        let (section, wanted) = if adult_asked(adult_shown, viewer) {
            // As ENR-1's gate reads it: stored as Adult, or found by an
            // always-adult source unless you said Not adult.
            let stored_adult = store
                .enrichment(row.id)
                .map_err(internal)?
                .is_some_and(|e| crate::search::is_adult(&e));
            let adult = stored_adult
                || (always_adult & row.sources != 0
                    && !store
                        .correction(row.id)
                        .map_err(internal)?
                        .is_some_and(|c| c.adult == Some(false)));
            if adult && !held_back(&store, &row).map_err(internal)? {
                (
                    adult_section(&claims, &catalogue, &names),
                    lookup_wanted(&row),
                )
            } else {
                (None, false)
            }
        } else {
            (None, false)
        };
        // The finders that are always adult, or can't be confirmed not to
        // be (fail closed). `None` only when the plugins couldn't be listed
        // at all: nothing is known, so no section shows.
        let finders_adult = (!names.is_empty())
            .then(|| catalogue.adult_or_unknown(row.sources) | (drawer_adult & row.sources));
        let identified =
            identified_section(&store, &row, &claims, &catalogue, viewer, finders_adult)
                .map_err(internal)?;
        drop(store);
        let id = row.id;
        let detail = detail_of(&row, &tree, summary, more, provenance, section, identified);
        Ok::<_, ApiError>((detail, wanted.then_some(id)))
    })
    .await
    .map_err(internal)??;
    // After the answer is built, and the same whether it's taken or
    // refused, so neither the body nor the timing says which.
    if let (Some(id), Some(Extension(interest))) = (wanted, interest) {
        interest.offer(client, id);
    }
    Ok(Json(detail))
}

/// A page of a torrent's file list.
#[utoipa::path(
    operation_id = "listTorrentFiles",
    get,
    path = "/torrents/{hash}/files",
    tag = "torrents",
    params(
        ("hash" = String, Path, description = "A 40-character hex info hash."),
        FilesQuery,
    ),
    responses(
        (status = 200, body = FilesPage),
        (status = 404, body = ErrorEnvelope, description = "`torrent_not_found`, or `adult_hidden`: an Adult torrent while \"Show adult content\" is off."),
    )
)]
pub(crate) async fn files(
    State(s): State<ApiState>,
    Path(hash): Path<String>,
    Query(p): Query<FilesQuery>,
) -> ApiResult<Json<FilesPage>> {
    tokio::task::spawn_blocking(move || {
        let (_, tree) = load(&s, &hash, s.adult.shown())?;
        let limit = p.limit.unwrap_or(MAX_FILES_PAGE).clamp(1, MAX_FILES_PAGE);
        let files = tree
            .files
            .iter()
            .skip(p.offset)
            .take(limit)
            .map(|f| file_entry(&tree, f))
            .collect();
        Ok(Json(FilesPage {
            files,
            offset: p.offset,
            total: tree.files.len(),
        }))
    })
    .await
    .map_err(internal)?
}

// ── Identified as (ENR-12, §6, §10.5) ───────────────────────────────────

/// The "Identified as" section of `row`'s detail: each general
/// enrichment plugin's stored answer (ENR-10), for `viewer`. `None` when
/// there's nothing to show.
///
/// - **Fails closed on always-adult finders.** `always_adult` is the
///   torrent's finders that are always adult or can't be confirmed not to
///   be ([`Catalogue::adult_or_unknown`]); any one hides the section.
///   `None` when nothing is known (the plugins couldn't be listed): no
///   section at all.
/// - **Never wider than the detail view, and stricter where unsure.**
///   None for a torrent that's hidden, held (a tombstone or quarantine on
///   a swarm hash), private in any way (`PRIVACY`: a private source, BEP
///   27, or a remote worker's unconfirmed bit), or Adult by its stored
///   type or an always-adult finder. ENR-10 never looks those up, so an
///   answer on one is stale, and Adult goes only through ENR-1's section.
/// - **An answer** is a claims row from a source that didn't find the
///   torrent (its bit isn't in `row.sources`), not through a private
///   source, with a details section holding a match confidence: ENR-10's
///   own rule (`identify::stored_title`), so a feed's claims never read as
///   one.
/// - **Sources:** the admin sees every answer with its source. Any other
///   caller sees only answers from a source known to be public; the rest
///   are left out, unannounced, as provenance leaves out private sources.
/// - **Stored text only, bounded again:** each list cut to its stored cap,
///   ids kept only in the closed schemes, each text cut to its limit.
fn identified_section(
    store: &Store,
    row: &TorrentRow,
    claims: &[StoredClaims],
    catalogue: &Catalogue,
    viewer: Viewer,
    always_adult: Option<u64>,
) -> lodestar_store::Result<Option<IdentifiedAs>> {
    // Unknown always-adult sources: nothing can be confirmed, so nothing
    // shows.
    let Some(always_adult) = always_adult else {
        return Ok(None);
    };
    if row.flags & (store_flags::HIDDEN | store_flags::PRIVACY) != 0
        || always_adult & row.sources != 0
    {
        return Ok(None);
    }
    // Every stored answer, surest first, ties by source id: the order
    // ENR-10's `identify::stored_title` reads them in, so the first with a
    // canonical title is the one a resolved record shows. Chosen over all
    // of them, before any is left out for this caller.
    let mut answers: Vec<(u8, &Claims, &lodestar_store::claims::Details, u8)> = Vec::new();
    for c in claims {
        let id = c.source.get();
        let bit = 1u64.checked_shl(u32::from(id)).unwrap_or(0);
        if row.sources & bit != 0 || c.private_source {
            continue;
        }
        let Some(stored) = c.claims.as_ref().filter(|s| s.adult.is_none()) else {
            continue;
        };
        let Some(d) = stored.details.as_deref() else {
            continue;
        };
        let Some(confidence) = d.match_confidence else {
            continue;
        };
        answers.push((id, stored, d, confidence));
    }
    // Most torrents have none: the store reads below are only for those
    // that do.
    if answers.is_empty() {
        return Ok(None);
    }
    let enrichment = store.enrichment(row.id)?;
    if enrichment.as_ref().is_some_and(crate::search::is_adult) {
        return Ok(None);
    }
    let hashes = store.torrent_swarm_hashes(row.id)?.unwrap_or_default();
    if store.held_among(&hashes)?.into_iter().any(|h| h) {
        return Ok(None);
    }
    let resolved = enrichment
        .and_then(|e| lodestar_classify::Record::decode(&e.data).ok())
        .and_then(|r| r.resolved);
    answers.sort_by_key(|&(id, _, _, conf)| (std::cmp::Reverse(conf), id));
    // Only when the record really shows a plugin's title: then it's this
    // answer's. Otherwise none is chosen, so no answer claims a title the
    // record doesn't show (today, always: ENR-10's lookups never resolve).
    let chosen = answers
        .iter()
        .find(|(_, _, d, _)| d.canonical_title.is_some())
        .map(|&(id, ..)| id)
        .filter(|_| resolved == Some(lodestar_classify::ResolvedVia::Plugin));
    let alternatives: Vec<Identification> = answers
        .into_iter()
        .filter_map(|(id, stored, d, confidence)| {
            let source = catalogue.get(id)?;
            if viewer == Viewer::Public && source.public != Some(true) {
                return None;
            }
            let mut a = identification(stored, d, confidence, source);
            a.chosen = chosen == Some(id);
            Some(a)
        })
        .collect();
    if alternatives.is_empty() {
        return Ok(None);
    }
    let resolved_via = resolved
        // Only the listed methods; `plugin` isn't one yet (ENR-10).
        .filter(|v| lodestar_classify::ResolvedVia::ALL.contains(v))
        .map(|v| v.as_str().to_owned());
    Ok(Some(IdentifiedAs {
        alternatives,
        resolved_via,
    }))
}

/// One stored answer as the section shows it, bounded again.
fn identification(
    c: &Claims,
    d: &lodestar_store::claims::Details,
    confidence: u8,
    source: &SourceEntry,
) -> Identification {
    use lodestar_store::claims::{
        MAX_ALT_TITLE, MAX_ALT_TITLES, MAX_CANONICAL_TITLE, MAX_GENRE, MAX_GENRES, MAX_IDS,
        MAX_MATCH_CONFIDENCE, id_ok, language_ok,
    };
    Identification {
        alt_titles: d
            .alt_titles
            .iter()
            .filter(|a| language_ok(&a.language) && !a.title.is_empty())
            .take(MAX_ALT_TITLES)
            .map(|a| IdentifiedTitle {
                language: a.language.clone(),
                title: cut(&a.title, MAX_ALT_TITLE),
            })
            .collect(),
        canonical_title: d
            .canonical_title
            .as_deref()
            .map(|t| cut(t, MAX_CANONICAL_TITLE)),
        chosen: false,
        confidence: confidence.min(MAX_MATCH_CONFIDENCE),
        genres: d
            .genres
            .iter()
            .take(MAX_GENRES)
            .map(|g| cut(g, MAX_GENRE))
            .collect(),
        ids: c
            .ids
            .iter()
            .filter(|i| id_ok(&i.scheme, &i.value))
            .take(MAX_IDS)
            .map(|i| IdentifiedId {
                scheme: i.scheme.clone(),
                value: i.value.clone(),
            })
            .collect(),
        source: EnrichmentSource {
            id: source.id,
            name: source.name.clone(),
            plugin: source.plugin.clone(),
        },
        year: d.year.filter(|y| (1..=9999).contains(y)),
    }
}

/// `s` cut to `max` bytes on a character boundary.
fn cut(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

// ── Adult enrichment (ENR-7, §20.9) ─────────────────────────────────────

/// A torrent the detail view shows but whose adult-enrichment section it
/// never does, and which it never offers for a lookup: hidden by a
/// filter rule, from a private source, or with a swarm hash the core or a
/// filter holds (a tombstone or quarantine row).
fn held_back(store: &Store, row: &TorrentRow) -> lodestar_store::Result<bool> {
    if row.flags & (store_flags::HIDDEN | store_flags::PRIVATE_SOURCE) != 0 {
        return Ok(true);
    }
    let hashes = store.torrent_swarm_hashes(row.id)?.unwrap_or_default();
    Ok(store.held_among(&hashes)?.into_iter().any(|h| h))
}

/// Whether this request may see adult enrichment at all: "Show adult
/// content" on and the admin session asking. A public caller (an API key
/// or feed token) never does: the sources behind a match are Account
/// class, which §18.7 withholds from it.
fn adult_asked(adult_shown: bool, viewer: Viewer) -> bool {
    adult_shown && viewer == Viewer::Admin
}

/// Whether an adult torrent that isn't held back could pass ENR-1's gate
/// (`adult_enrich::check`) at all: not a BEP 27 private torrent or a
/// partial import, which the gate never looks up. The gate still decides;
/// this only keeps items it would surely refuse out of the queue.
fn lookup_wanted(row: &TorrentRow) -> bool {
    row.flags & (store_flags::PRIVACY | store_flags::FILES_PARTIAL) == 0
}

/// The stored match the detail view shows, by ENR-1's rule so the drawer
/// and the kind pin agree (`pipeline::enrich::stored_match`): the first,
/// by source id, of the sections that name a kind; failing that, the
/// first section.
fn chosen_match(claims: &[StoredClaims]) -> Option<(&StoredClaims, &StoredMatch)> {
    let mut found: Vec<(&StoredClaims, &StoredMatch)> = claims
        .iter()
        .filter_map(|c| Some((c, c.claims.as_ref()?.adult.as_ref()?)))
        .collect();
    found.sort_by_key(|(c, _)| c.source.get());
    found
        .iter()
        .find(|(_, m)| m.kind.is_some())
        .or(found.first())
        .copied()
}

/// The adult-enrichment section of a torrent's detail, from its stored
/// claims. `None` when there's no match, when the chosen match came
/// through a private source (it isn't replaced by another), or when its
/// source isn't registered.
fn adult_section(
    claims: &[StoredClaims],
    catalogue: &Catalogue,
    names: &PluginNames,
) -> Option<AdultEnrichment> {
    let (stored, m) = chosen_match(claims)?;
    if stored.private_source {
        return None;
    }
    let source = catalogue.get(stored.source.get())?;
    let endpoints = names
        .get(&source.plugin)
        .map_or(&[][..], |n| &n.endpoints[..]);
    let link = stored
        .claims
        .as_ref()
        .and_then(|c| c.provenance.external_ref.as_deref())
        .and_then(|url| scene_link(url, endpoints));
    Some(AdultEnrichment {
        link,
        performers: m.performers.clone(),
        performers_truncated: m.performers_truncated,
        released: m.released.map(|d| d.to_string()),
        scene_title: m.title.clone(),
        source: EnrichmentSource {
            id: source.id,
            name: source.name.clone(),
            plugin: source.plugin.clone(),
        },
        studio: m.studio.clone(),
    })
}

/// The longest scene link shown.
const MAX_LINK: usize = 2048;

/// `url` when it's a plain `https` URL on one of `endpoints`' hosts
/// (`https://host`, as a source's manifest lists them), exactly: no
/// other scheme, no user info, no other host or port, nothing but
/// printable ASCII that needs no quoting in HTML. `None` otherwise, and
/// the section keeps the rest.
fn scene_link(url: &str, endpoints: &[String]) -> Option<String> {
    let safe = |b: u8| b.is_ascii_graphic() && !b"\"'<>\\^`{|}".contains(&b);
    if url.len() > MAX_LINK || !url.bytes().all(safe) {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, tail) = rest.split_at(end);
    if host.is_empty() || !(tail.is_empty() || tail.starts_with('/')) {
        return None;
    }
    endpoints
        .iter()
        .filter_map(|e| e.strip_prefix("https://"))
        .map(|h| h.trim_end_matches('/'))
        .any(|h| !h.is_empty() && h.eq_ignore_ascii_case(host))
        .then(|| url.to_owned())
}

/// A token bucket's size and refill: `burst` at once, then one every
/// `every`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub burst: u32,
    pub every: Duration,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: u32,
    /// When the tokens were last counted.
    at: std::time::Instant,
}

impl Bucket {
    fn full(rate: Rate, now: std::time::Instant) -> Bucket {
        Bucket {
            tokens: rate.burst,
            at: now,
        }
    }

    /// Adds the whole tokens earned since `at`, keeping the remainder.
    fn refill(&mut self, rate: Rate, now: std::time::Instant) {
        if self.tokens >= rate.burst {
            self.tokens = rate.burst;
            self.at = now;
            return;
        }
        let every = rate.every.as_nanos().max(1);
        let earned = now.saturating_duration_since(self.at).as_nanos() / every;
        if earned == 0 {
            return;
        }
        let tokens = u128::from(self.tokens) + earned;
        if tokens >= u128::from(rate.burst) {
            *self = Bucket::full(rate, now);
        } else {
            // `earned` < `burst` here, so it fits.
            self.tokens = tokens as u32;
            self.at += rate.every * earned as u32;
        }
    }

    /// Full again: nothing to remember about its client.
    fn is_full(&mut self, rate: Rate, now: std::time::Instant) -> bool {
        self.refill(rate, now);
        self.tokens >= rate.burst
    }
}

#[derive(Debug)]
struct Buckets {
    per_client: Rate,
    overall_rate: Rate,
    /// Clients tracked at most.
    max: usize,
    clients: HashMap<IpAddr, Bucket>,
    /// Shared by every newcomer while `clients` is full of clients still
    /// spending.
    overflow: Bucket,
    overall: Bucket,
}

impl Buckets {
    fn new(per_client: Rate, overall_rate: Rate, max: usize, now: std::time::Instant) -> Buckets {
        Buckets {
            per_client,
            overall_rate,
            max,
            clients: HashMap::new(),
            overflow: Bucket::full(per_client, now),
            overall: Bucket::full(overall_rate, now),
        }
    }

    /// Takes a token from `client`'s bucket and the overall one, or from
    /// neither: false when either is empty.
    fn take(&mut self, client: IpAddr, now: std::time::Instant) -> bool {
        let Buckets {
            per_client: per,
            overall_rate,
            max,
            clients,
            overflow,
            overall,
        } = self;
        let (per, max) = (*per, *max);
        overall.refill(*overall_rate, now);
        if !clients.contains_key(&client) && clients.len() >= max {
            clients.retain(|_, b| !b.is_full(per, now));
        }
        let bucket = if clients.contains_key(&client) || clients.len() < max {
            clients.entry(client).or_insert(Bucket::full(per, now))
        } else {
            overflow
        };
        bucket.refill(per, now);
        if bucket.tokens == 0 || overall.tokens == 0 {
            return false;
        }
        bucket.tokens -= 1;
        overall.tokens -= 1;
        true
    }
}

/// One client, as the limit counts it: an IPv4 address, or an IPv6
/// address's /64 (one subscriber's usual allocation), so rotating
/// addresses inside one network doesn't buy more lookups.
fn client_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6((u128::from(v6) & !((1u128 << 64) - 1)).into()),
        },
    }
}

/// Where the detail view reports an opened drawer to the adult-enrichment
/// runtime (ENR-1's `Interest::opened`), behind a rate limit per client
/// and overall, so opening drawers in a loop can't keep a third-party
/// service busy: user interest skips the runtime's daily cap.
///
/// The API offers an item only when "Show adult content" is on, the
/// admin is asking, and the item isn't held back and could be adult; the
/// runtime's gate still decides. A refused offer is dropped without a
/// trace: the response is the same either way.
///
/// None is served until the host wires a plugin. The host mounts it on
/// the router: `router(state).layer(Extension(interest))`.
#[derive(Clone)]
pub struct DrawerInterest(Arc<InterestInner>);

struct InterestInner {
    opened: Box<dyn Fn(i64) -> bool + Send + Sync>,
    always_adult: u64,
    clock: Box<dyn Fn() -> std::time::Instant + Send + Sync>,
    buckets: Mutex<Buckets>,
}

impl std::fmt::Debug for DrawerInterest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DrawerInterest")
            .field("always_adult", &self.0.always_adult)
            .finish_non_exhaustive()
    }
}

impl DrawerInterest {
    /// Per client: 10 at once, then one every 30 s (runner's choice,
    /// `docs/decisions/2026-11-05-ENR-7.md`).
    pub const PER_CLIENT: Rate = Rate {
        burst: 10,
        every: Duration::from_secs(30),
    };
    /// Every client together: 30 at once, then one every 20 s: at most
    /// 4,350 a day. A second bound on how fast drawers can spend the
    /// runtime's daily cap (Q15), which interest is to count against.
    pub const OVERALL: Rate = Rate {
        burst: 30,
        every: Duration::from_secs(20),
    };
    /// Clients tracked at once; past this, newcomers share one bucket.
    pub const CLIENTS: usize = 1024;

    /// `opened` hands an id to the runtime (`Interest::opened`);
    /// `always_adult` is the registered always-adult sources' bitset
    /// (`AdultSources`), so their finds count as possibly adult unless
    /// you said Not adult. The host must always pass it: without it, a
    /// torrent adult only through its source shows no section and isn't
    /// offered (fails closed).
    pub fn new(always_adult: u64, opened: impl Fn(i64) -> bool + Send + Sync + 'static) -> Self {
        Self::with_clock(always_adult, opened, std::time::Instant::now)
    }

    /// [`DrawerInterest::new`] on a clock of the caller's (tests).
    pub fn with_clock(
        always_adult: u64,
        opened: impl Fn(i64) -> bool + Send + Sync + 'static,
        clock: impl Fn() -> std::time::Instant + Send + Sync + 'static,
    ) -> Self {
        let now = clock();
        DrawerInterest(Arc::new(InterestInner {
            opened: Box::new(opened),
            always_adult,
            clock: Box::new(clock),
            buckets: Mutex::new(Buckets::new(
                Self::PER_CLIENT,
                Self::OVERALL,
                Self::CLIENTS,
                now,
            )),
        }))
    }

    /// Hands `id` on if `client` and everyone together are within their
    /// limits; drops it otherwise.
    fn offer(&self, client: IpAddr, id: i64) {
        let taken = {
            let mut buckets = self.0.buckets.lock().unwrap_or_else(|e| e.into_inner());
            // Read under the lock, so the buckets see time in order.
            let now = (self.0.clock)();
            buckets.take(client_key(client), now)
        };
        if taken {
            // A full queue drops it too; the background gets to it.
            let _ = (self.0.opened)(id);
        }
    }
}

pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use lodestar_store::claims::ClaimedFile;

    use super::*;

    fn row(sources: u64, first: Option<u8>, flags: i64) -> TorrentRow {
        TorrentRow {
            id: 7,
            info_hash: InfoHash([1; 20]),
            info_hash_v2: None,
            name: "Some.Show.S01.1080p".into(),
            name_raw: None,
            size: 4_000,
            piece_len: 1 << 14,
            piece_count: 1,
            file_count: 3,
            info_size: 100,
            flags,
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            last_seen_at: 1_700_000_002,
            sources,
            first_source: first.and_then(SourceId::new),
        }
    }

    /// `dht` (0, public), `prowlarr` (1, account class: private), `nyaa`
    /// (2, public) and `gone` (3, a plugin this build doesn't run).
    pub(crate) fn catalogue() -> Catalogue {
        Catalogue::of(
            [
                (0u8, "dht", "Mainline DHT", Some(true)),
                (1, "prowlarr", "Prowlarr", Some(false)),
                (2, "nyaa", "Nyaa", Some(true)),
                (3, "gone", "gone", None),
            ]
            .into_iter()
            .map(|(id, plugin, name, public)| SourceEntry {
                id,
                plugin: plugin.into(),
                name: name.into(),
                public,
                always_adult: Some(false),
            })
            .collect(),
        )
    }

    fn claims(source: u8, private: bool, c: Claims) -> StoredClaims {
        StoredClaims {
            torrent_id: 7,
            source: SourceId::new(source).unwrap(),
            private_source: private,
            claims: Some(c),
        }
    }

    fn plugins(p: &Provenance) -> Vec<&str> {
        p.sources.iter().map(|s| s.plugin.as_str()).collect()
    }

    #[test]
    fn the_viewer_is_the_admin_only_with_a_session() {
        // No guard said who it is: fail closed.
        assert_eq!(Viewer::of(None), Viewer::Public);
        assert_eq!(Viewer::of(Some(&Access::Session)), Viewer::Admin);
        assert_eq!(
            Viewer::of(Some(&Access::Key {
                id: 1,
                scopes: crate::keys::scope::ALL
            })),
            Viewer::Public
        );
        assert_eq!(
            Viewer::of(Some(&Access::Feed {
                id: 1,
                saved_search_id: None
            })),
            Viewer::Public
        );
    }

    #[test]
    fn sources_are_listed_first_found_first_then_by_id() {
        let c = catalogue();
        // dht, nyaa and an unregistered bit 9; nyaa found it first.
        let r = row(0b101 | 1 << 9, Some(2), 0);
        let p = c.provenance(&r, &[], Viewer::Admin);
        assert_eq!(plugins(&p), ["nyaa", "dht"]);
        assert!(p.sources[0].first && !p.sources[1].first);
        assert_eq!(p.sources[0].first_seen_at, Some(r.discovered_at));
        assert_eq!(p.sources[1].first_seen_at, None);
        assert_eq!(p.sources[0].name, "Nyaa");
        assert!(p.verified && !p.withheld && p.network == "bt");
        // A torrent stored before provenance has none.
        assert!(
            c.provenance(&row(0, None, 0), &[], Viewer::Admin)
                .sources
                .is_empty()
        );
    }

    #[test]
    fn private_sources_and_claims_reach_the_admin_only() {
        let c = catalogue();
        // dht, prowlarr, nyaa (with private claims) and gone.
        let r = row(0b1111, Some(1), 0);
        let stored = [
            claims(1, true, Claims::default()),
            claims(
                2,
                true,
                Claims {
                    name: Some("leaked".into()),
                    ..Claims::default()
                },
            ),
        ];
        let admin = c.provenance(&r, &stored, Viewer::Admin);
        assert_eq!(plugins(&admin), ["prowlarr", "dht", "nyaa", "gone"]);
        let private: Vec<bool> = admin.sources.iter().map(|s| s.private).collect();
        // Unknown isn't known to be private, but stays from the public.
        assert_eq!(private, [true, false, true, false]);
        assert!(!admin.withheld);
        assert!(admin.sources[2].claim.as_ref().unwrap().name.is_some());

        // Nyaa is a public source: its name stays, its private claim
        // doesn't.
        let public = c.provenance(&r, &stored, Viewer::Public);
        assert_eq!(plugins(&public), ["dht", "nyaa"]);
        assert!(public.withheld);
        assert!(
            public
                .sources
                .iter()
                .all(|s| s.claim.is_none() && !s.private)
        );
        let text = serde_json::to_string(&public).unwrap();
        for hidden in ["prowlarr", "Prowlarr", "leaked", "gone"] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
        // Its hit shows the first source the caller may see.
        assert_eq!(c.hit_source(&r, Viewer::Admin).unwrap().plugin, "prowlarr");
        assert_eq!(c.hit_source(&r, Viewer::Public).unwrap().plugin, "dht");
    }

    #[test]
    fn a_hit_and_the_detail_agree_on_a_public_source_with_a_private_claim() {
        let c = catalogue();
        // Prowlarr found it first; Nyaa's claims came through a private
        // source. No DHT bit.
        let r = row(0b110, Some(1), 0);
        let stored = [claims(
            2,
            true,
            Claims {
                name: Some("leaked".into()),
                ..Claims::default()
            },
        )];
        let hit = c.hit_source(&r, Viewer::Public).unwrap();
        let detail = c.provenance(&r, &stored, Viewer::Public);
        assert_eq!(plugins(&detail), [hit.plugin.as_str()]);
        assert_eq!(hit.plugin, "nyaa");
        assert!(detail.withheld && detail.sources[0].claim.is_none());
        let admin = c.provenance(&r, &stored, Viewer::Admin);
        assert!(admin.sources[1].private && admin.sources[1].claim.is_some());
    }

    #[test]
    fn a_private_source_torrent_shows_no_source_to_the_public() {
        let c = catalogue();
        // Even public sources' bits: the flag says it was found only
        // through private ones (§18.7).
        let r = row(0b101, Some(0), store_flags::PRIVATE_SOURCE);
        let public = c.provenance(&r, &[], Viewer::Public);
        assert!(public.sources.is_empty() && public.withheld);
        assert_eq!(c.hit_source(&r, Viewer::Public), None);
        let admin = c.provenance(&r, &[], Viewer::Admin);
        assert!(admin.sources.iter().all(|s| s.private));
        assert_eq!(c.hit_source(&r, Viewer::Admin).unwrap().plugin, "dht");
    }

    /// ENR-1's queue skips what its gate refuses: a BEP 27 private row, a
    /// remote worker's row whose BEP 27 bit isn't verified yet (WK-9), and
    /// a partial import.
    #[test]
    fn enrichment_lookups_skip_private_and_unverified_remote_rows() {
        assert!(lookup_wanted(&row(0b1, Some(0), 0)));
        for f in [
            store_flags::PRIVATE,
            store_flags::REMOTE_UNVERIFIED,
            store_flags::PRIVATE_SOURCE,
            store_flags::FILES_PARTIAL,
        ] {
            assert!(!lookup_wanted(&row(0b1, Some(0), f)), "{f:#x}");
        }
    }

    #[test]
    fn a_private_claim_from_a_purged_source_is_still_withheld() {
        let c = catalogue();
        let r = row(0b1, Some(0), 0);
        let stored = [claims(2, true, Claims::default())];
        assert!(c.provenance(&r, &stored, Viewer::Public).withheld);
        let admin = c.provenance(&r, &stored, Viewer::Admin);
        assert_eq!(plugins(&admin), ["dht"]);
    }

    /// A store with one verified film found by the DHT, and two answers:
    /// Wikidata's (public) and an account-class enricher's (private).
    fn identified_store() -> (Store, TorrentRow) {
        use lodestar_bencode::{OwnedValue, to_vec};
        let mut store = Store::open_in_memory(lodestar_store::StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 0).unwrap();
        let wikidata = store.register_source("wikidata", "1", 0).unwrap();
        let acct = store.register_source("acctenr", "1", 0).unwrap();
        let d = |e: Vec<(&str, OwnedValue)>| {
            OwnedValue::Dict(
                e.into_iter()
                    .map(|(k, v)| (k.as_bytes().to_vec(), v))
                    .collect(),
            )
        };
        let raw = to_vec(&d(vec![
            ("length", OwnedValue::Int(1 << 30)),
            (
                "name",
                OwnedValue::Bytes(b"Some.Film.1994.1080p.BluRay".to_vec()),
            ),
            ("piece length", OwnedValue::Int(1 << 16)),
            ("pieces", OwnedValue::Bytes(vec![0; 20])),
        ]));
        let info = lodestar_metainfo::Info::parse(&raw).unwrap();
        store
            .insert(&[lodestar_store::NewTorrent {
                info: &info,
                raw_info: None,
                discovered_at: 1,
                metadata_at: 1,
                extra_flags: 0,
                source: dht,
            }])
            .unwrap();
        let id = store.find(&info.primary_hash()).unwrap().unwrap();
        let answer = |title: &str, conf: u8| Claims {
            details: Some(Box::new(lodestar_store::claims::Details {
                canonical_title: Some(title.into()),
                match_confidence: Some(conf),
                ..Default::default()
            })),
            ..Claims::default()
        };
        assert!(
            store
                .put_claims(id, wikidata, &answer("Public Answer", 70), false)
                .unwrap()
        );
        assert!(
            store
                .put_claims(id, acct, &answer("Account Answer", 95), false)
                .unwrap()
        );
        let row = store.torrent(id).unwrap().unwrap();
        (store, row)
    }

    /// A caller other than the admin sees only answers from a known public
    /// source.
    #[test]
    fn only_public_enrichers_reach_the_public() {
        let (store, row) = identified_store();
        let entry = |id: u8, plugin: &str, public: Option<bool>| SourceEntry {
            id,
            plugin: plugin.into(),
            name: plugin.into(),
            public,
            always_adult: Some(false),
        };
        let c = Catalogue::of(vec![
            entry(0, "dht", Some(true)),
            entry(1, "wikidata", Some(true)),
            entry(2, "acctenr", Some(false)),
        ]);
        let claims = store.torrent_claims(row.id).unwrap();
        let admin = identified_section(&store, &row, &claims, &c, Viewer::Admin, Some(0))
            .unwrap()
            .unwrap();
        let plugins: Vec<&str> = admin
            .alternatives
            .iter()
            .map(|a| a.source.plugin.as_str())
            .collect();
        assert_eq!(plugins, ["acctenr", "wikidata"]);
        // The record isn't resolved via a plugin: nothing is chosen.
        assert!(admin.alternatives.iter().all(|a| !a.chosen));
        let public = identified_section(&store, &row, &claims, &c, Viewer::Public, Some(0))
            .unwrap()
            .unwrap();
        assert_eq!(public.alternatives.len(), 1);
        assert_eq!(public.alternatives[0].source.plugin, "wikidata");
        let text = format!("{public:?}");
        assert!(
            !text.contains("acctenr") && !text.contains("Account Answer"),
            "{text}"
        );
        // An unknown class is private to the public: nothing left.
        let unknown = Catalogue::of(vec![entry(1, "wikidata", None), entry(2, "acctenr", None)]);
        assert_eq!(
            identified_section(&store, &row, &claims, &unknown, Viewer::Public, Some(0)).unwrap(),
            None
        );
        // The always-adult sources unknown: none, for anyone.
        assert_eq!(
            identified_section(&store, &row, &claims, &c, Viewer::Admin, None).unwrap(),
            None
        );
        // Found by an always-adult source: none, for anyone.
        assert_eq!(
            identified_section(&store, &row, &claims, &c, Viewer::Admin, Some(1 << 0)).unwrap(),
            None
        );
    }

    /// When the chosen answer's source is withheld from this caller, no
    /// other answer is marked in its place (rv-760).
    #[test]
    fn a_withheld_chosen_answer_marks_nothing() {
        let (mut store, row) = identified_store();
        // Resolve the record via a plugin, so the account enricher's
        // surer answer is the chosen one.
        let mut e = lodestar_filter::reclassify::enrich_corrected(
            &row.name,
            &[],
            lodestar_classify::Correction::default(),
        )
        .0;
        let mut rec = lodestar_classify::Record::decode(&e.data).unwrap();
        rec.resolved = Some(lodestar_classify::ResolvedVia::Plugin);
        e.data = rec.encode();
        store.set_enrichments(&[(row.id, e)]).unwrap();
        let entry = |id: u8, plugin: &str, public: Option<bool>| SourceEntry {
            id,
            plugin: plugin.into(),
            name: plugin.into(),
            public,
            always_adult: Some(false),
        };
        let c = Catalogue::of(vec![
            entry(0, "dht", Some(true)),
            entry(1, "wikidata", Some(true)),
            entry(2, "acctenr", Some(false)),
        ]);
        let claims = store.torrent_claims(row.id).unwrap();
        let admin = identified_section(&store, &row, &claims, &c, Viewer::Admin, Some(0))
            .unwrap()
            .unwrap();
        assert_eq!(admin.alternatives[0].source.plugin, "acctenr");
        assert!(admin.alternatives[0].chosen && !admin.alternatives[1].chosen);
        let public = identified_section(&store, &row, &claims, &c, Viewer::Public, Some(0))
            .unwrap()
            .unwrap();
        assert_eq!(public.alternatives.len(), 1);
        assert_eq!(public.alternatives[0].source.plugin, "wikidata");
        assert!(
            !public.alternatives[0].chosen,
            "nothing promoted in its place"
        );
    }

    /// The catalogue's always-adult bits are the listed sources whose
    /// manifest says so, by source id (`AdultSources::of`'s rule), and
    /// every finder it can't vouch for counts as always adult where that
    /// fails closed (X-always-adult-detail-mask).
    #[test]
    fn the_always_adult_mask_follows_the_manifests_and_fails_closed() {
        let manifests = [
            (0u8, "dht", false),
            (3, "adultsite", true),
            (7, "other", true),
        ];
        let entry = |id: u8, plugin: &str, always_adult: Option<bool>| SourceEntry {
            id,
            plugin: plugin.into(),
            name: plugin.into(),
            public: Some(true),
            always_adult,
        };
        let c = Catalogue::of(
            manifests
                .iter()
                .map(|&(id, plugin, adult)| entry(id, plugin, Some(adult)))
                .chain([entry(9, "unknown", None)])
                .collect(),
        );
        assert_eq!(c.always_adult_bits(), (1 << 3) | (1 << 7));
        // Of a torrent's finders: the always-adult one, the one whose
        // manifest isn't known (9), and a bit with no source at all (12).
        let finders = 1 | (1 << 3) | (1 << 9) | (1 << 12);
        assert_eq!(c.adult_or_unknown(finders), (1 << 3) | (1 << 9) | (1 << 12));
        assert_eq!(c.adult_or_unknown(1), 0, "the DHT is vouched for");
        // An unknown source isn't counted as always adult where that would
        // widen something: only the known ones are in the bits.
        assert_eq!(c.always_adult_bits() & (1 << 9), 0);
    }

    /// Every text is cut to its limit on a character boundary, however
    /// the row was written.
    #[test]
    fn texts_are_cut_to_their_limits() {
        use lodestar_store::claims::{AltTitle, Details, MAX_ALT_TITLE, MAX_CANONICAL_TITLE};
        let d = Details {
            canonical_title: Some("é".repeat(MAX_CANONICAL_TITLE)),
            alt_titles: vec![AltTitle {
                language: "en".into(),
                title: "ü".repeat(MAX_ALT_TITLE),
            }],
            year: Some(0),
            match_confidence: Some(250),
            ..Details::default()
        };
        let src = SourceEntry {
            id: 1,
            plugin: "w".into(),
            name: "W".into(),
            public: Some(true),
            always_adult: Some(false),
        };
        let a = identification(&Claims::default(), &d, 250, &src);
        let t = a.canonical_title.unwrap();
        assert!(t.len() <= MAX_CANONICAL_TITLE && t.chars().all(|c| c == 'é'));
        assert!(a.alt_titles[0].title.len() <= MAX_ALT_TITLE);
        assert_eq!((a.confidence, a.year), (100, None));
    }

    #[test]
    fn a_scene_link_must_be_https_on_the_sources_own_host() {
        let hosts = [
            "https://stashdb.org".to_string(),
            "http://plain.example".into(),
        ];
        let ok = |u: &str| scene_link(u, &hosts).is_some();
        assert!(ok("https://stashdb.org/scenes/abc-1"));
        assert!(ok("https://StashDB.org/scenes/abc-1?x=1#y"));
        assert!(ok("https://stashdb.org"));
        for bad in [
            "http://stashdb.org/scenes/a",
            "https://plain.example/a",
            "http://plain.example/a",
            "https://stashdb.org.evil.example/a",
            "https://evil.example/stashdb.org",
            "https://user@stashdb.org/a",
            "https://stashdb.org@evil.example/a",
            "https://stashdb.org:8443/a",
            "https://stashdb.org\\@evil.example/a",
            "https://stashdb.org/a b",
            "https://stashdb.org/a\"onclick",
            "https://stashdb.org/<script>",
            "javascript:alert(1)",
            "//stashdb.org/a",
            "https:///a",
            "",
        ] {
            assert!(!ok(bad), "{bad}");
        }
        let long = format!("https://stashdb.org/{}", "a".repeat(MAX_LINK));
        assert!(!ok(&long));
        assert!(scene_link("https://stashdb.org/a", &[]).is_none());
    }

    #[test]
    fn only_the_admin_with_the_preference_on_asks_for_adult_enrichment() {
        assert!(adult_asked(true, Viewer::Admin));
        assert!(!adult_asked(false, Viewer::Admin));
        assert!(!adult_asked(true, Viewer::Public));
        assert!(!adult_asked(false, Viewer::Public));
        // A key or feed token is public, whatever the preference.
        for a in [
            Access::Key {
                id: 1,
                scopes: u32::MAX,
            },
            Access::Feed {
                id: 1,
                saved_search_id: None,
            },
        ] {
            assert!(!adult_asked(true, Viewer::of(Some(&a))));
        }
    }

    #[test]
    fn an_ipv6_client_is_its_64() {
        let a: IpAddr = "2001:db8:0:1::5".parse().unwrap();
        let b: IpAddr = "2001:db8:0:1:ffff::9".parse().unwrap();
        let c: IpAddr = "2001:db8:0:2::5".parse().unwrap();
        assert_eq!(client_key(a), client_key(b));
        assert_ne!(client_key(a), client_key(c));
        let mapped: IpAddr = "::ffff:10.1.2.3".parse().unwrap();
        assert_eq!(client_key(mapped), IpAddr::from([10, 1, 2, 3]));
    }

    #[test]
    fn a_bucket_never_holds_more_than_its_burst() {
        let now = std::time::Instant::now();
        let rate = DrawerInterest::PER_CLIENT;
        let mut b = Bucket { tokens: 0, at: now };
        b.refill(rate, now + rate.every * 1000);
        assert_eq!(b.tokens, rate.burst);
        // Part of a refill period is kept for the next token.
        let mut b = Bucket { tokens: 0, at: now };
        b.refill(rate, now + rate.every + rate.every / 2);
        assert_eq!(b.tokens, 1);
        b.refill(rate, now + rate.every * 2);
        assert_eq!(b.tokens, 2);
        // A clock that goes back earns nothing.
        let mut b = Bucket {
            tokens: 1,
            at: now + rate.every,
        };
        b.refill(rate, now);
        assert_eq!(b.tokens, 1);
    }

    #[test]
    fn the_client_map_is_bounded_and_newcomers_share_the_overflow() {
        let now = std::time::Instant::now();
        let per = DrawerInterest::PER_CLIENT;
        let unlimited = Rate {
            burst: u32::MAX,
            every: Duration::from_nanos(1),
        };
        const MAX: usize = 64;
        let mut b = Buckets::new(per, unlimited, MAX, now);
        let addr = |n: u32| IpAddr::from(n.to_be_bytes());
        for n in 0..MAX as u32 {
            assert!(b.take(addr(n), now));
        }
        assert_eq!(b.clients.len(), MAX);
        // Every tracked client is still spending: newcomers share one
        // bucket, so all of them together get one burst.
        let extra = (0..per.burst * 3)
            .filter(|i| b.take(addr(1 << 30 | i), now))
            .count();
        assert_eq!(extra, per.burst as usize);
        assert_eq!(b.clients.len(), MAX);
        // Once the tracked ones have recovered they're forgotten.
        let later = now + per.every * per.burst;
        assert!(b.take(addr(1 << 31), later));
        assert_eq!(b.clients.len(), 1);
    }

    /// `n` public plugins.
    fn names_of(n: usize) -> PluginNames {
        (0..n)
            .map(|i| {
                (
                    format!("p{i}"),
                    PluginName {
                        name: format!("P{i}"),
                        public: true,
                        endpoints: Vec::new(),
                        always_adult: false,
                    },
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn concurrent_first_requests_list_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const N: usize = 16;
        let slot = Arc::new(NamesSlot::new(NAMES_RETRY, NAMES_TIMEOUT));
        let calls = Arc::new(AtomicUsize::new(0));
        // Every request starts before the listing may answer.
        let gate = Arc::new(tokio::sync::Barrier::new(N));
        let tasks: Vec<_> = (0..N)
            .map(|_| {
                let (slot, calls, gate) = (slot.clone(), calls.clone(), gate.clone());
                tokio::spawn(async move {
                    gate.wait().await;
                    slot.get(move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        // Let the other requests arrive while it lists.
                        for _ in 0..N {
                            tokio::task::yield_now().await;
                        }
                        Some(names_of(2))
                    })
                    .await
                })
            })
            .collect();
        for t in tasks {
            assert_eq!(t.await.unwrap().len(), 2);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Kept: later requests don't list again.
        let again = slot.get(|| async { panic!("listed twice") }).await;
        assert_eq!(again.len(), 2);
    }

    #[tokio::test]
    async fn a_failing_listing_is_not_retried_per_request() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const N: usize = 16;
        let slot = Arc::new(NamesSlot::new(Duration::from_secs(3600), NAMES_TIMEOUT));
        let calls = Arc::new(AtomicUsize::new(0));
        // Concurrent requests while it fails, then requests after.
        let tasks: Vec<_> = (0..N)
            .map(|_| {
                let (slot, calls) = (slot.clone(), calls.clone());
                tokio::spawn(async move {
                    slot.get(move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        for _ in 0..N {
                            tokio::task::yield_now().await;
                        }
                        None
                    })
                    .await
                })
            })
            .collect();
        for t in tasks {
            assert!(t.await.unwrap().is_empty());
        }
        for _ in 0..N {
            let calls = calls.clone();
            let names = slot
                .get(move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    None
                })
                .await;
            assert!(names.is_empty());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_listing_is_retried_after_the_back_off() {
        // No back-off: the next request asks again, and a success is kept.
        let slot = Arc::new(NamesSlot::new(Duration::ZERO, NAMES_TIMEOUT));
        assert!(slot.get(|| async { None }).await.is_empty());
        assert_eq!(slot.get(|| async { Some(names_of(1)) }).await.len(), 1);
        let kept = slot.get(|| async { panic!("listed after success") }).await;
        assert_eq!(kept.len(), 1);
    }

    #[tokio::test]
    async fn a_request_dropped_mid_listing_does_not_start_a_second() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let slot = Arc::new(NamesSlot::new(NAMES_RETRY, NAMES_TIMEOUT));
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started) = tokio::sync::oneshot::channel();
        let (finish, finish_rx) = tokio::sync::oneshot::channel::<()>();
        let first = {
            let (slot, calls) = (slot.clone(), calls.clone());
            tokio::spawn(async move {
                slot.get(move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let _ = started_tx.send(());
                    finish_rx.await.ok()?;
                    Some(names_of(3))
                })
                .await
            })
        };
        started.await.unwrap();
        // Its client goes away while it lists.
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let second = {
            let (slot, calls) = (slot.clone(), calls.clone());
            tokio::spawn(async move {
                slot.get(move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Some(names_of(1))
                })
                .await
            })
        };
        finish.send(()).unwrap();
        // The first listing's answer, not a second listing's.
        assert_eq!(second.await.unwrap().len(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_listing_times_out_and_backs_off() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let slot = Arc::new(NamesSlot::new(NAMES_RETRY, NAMES_TIMEOUT));
        let calls = Arc::new(AtomicUsize::new(0));
        let hang = |calls: Arc<AtomicUsize>| {
            move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                std::future::pending::<Option<PluginNames>>().await
            }
        };
        let started = Instant::now();
        assert!(slot.get(hang(calls.clone())).await.is_empty());
        assert!(started.elapsed() >= NAMES_TIMEOUT);
        // Backing off: no second listing.
        assert!(slot.get(hang(calls.clone())).await.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // After the back-off it asks again.
        tokio::time::advance(NAMES_RETRY).await;
        let names = slot.get(|| async { Some(names_of(2)) }).await;
        assert_eq!(names.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_panicked_listing_backs_off_like_a_failure() {
        let slot = Arc::new(NamesSlot::new(NAMES_RETRY, NAMES_TIMEOUT));
        assert!(
            slot.get(|| async { panic!("listing panicked") })
                .await
                .is_empty()
        );
        let again = slot.get(|| async { panic!("retried at once") }).await;
        assert!(again.is_empty());
        tokio::time::advance(NAMES_RETRY).await;
        assert_eq!(slot.get(|| async { Some(names_of(1)) }).await.len(), 1);
    }

    /// A pipeline that counts its `sources()` calls; everything else is
    /// unused.
    #[derive(Debug, Default)]
    pub(crate) struct CountingOps {
        pub(crate) calls: std::sync::atomic::AtomicUsize,
        pub(crate) fail: bool,
    }

    impl OpsApi for CountingOps {
        fn crawler(&self) -> crate::ops::CrawlerRuntime {
            unimplemented!()
        }
        fn sync_crawler(&self) -> crate::ops::BoxFuture<'_, ()> {
            unimplemented!()
        }
        fn stats(&self) -> crate::ops::BoxFuture<'_, crate::ops::StatsResponse> {
            unimplemented!()
        }
        fn sources(
            &self,
        ) -> crate::ops::BoxFuture<'_, Result<Vec<crate::ops::Source>, crate::ops::OpsError>>
        {
            Box::pin(async {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Let the other requests arrive while it lists.
                for _ in 0..16 {
                    tokio::task::yield_now().await;
                }
                if self.fail {
                    Err(crate::ops::OpsError::Internal("store busy".into()))
                } else {
                    Ok(Vec::new())
                }
            })
        }
        fn patch_source(
            &self,
            _: u8,
            _: crate::ops::SourcePatch,
            _: bool,
        ) -> crate::ops::BoxFuture<'_, Result<crate::ops::Source, crate::ops::OpsError>> {
            unimplemented!()
        }
        fn patch_source_credentials(
            &self,
            _: u8,
            _: Vec<(String, Option<crate::ops::Secret>)>,
        ) -> crate::ops::BoxFuture<'_, Result<crate::ops::Source, crate::ops::OpsError>> {
            unimplemented!()
        }
        fn network(&self) -> crate::ops::NetworkCheck {
            unimplemented!()
        }
        fn restart_network_check(&self) -> Result<crate::ops::NetworkCheck, crate::ops::OpsError> {
            unimplemented!()
        }
    }

    /// Through the per-pipeline cache: concurrent first requests, and
    /// requests while the listing fails, each pipeline listing once.
    /// Paused time: the back-off can't run out mid-test however slow
    /// the runner.
    #[tokio::test(start_paused = true)]
    async fn each_pipeline_lists_once_through_the_cache() {
        use std::sync::atomic::Ordering;
        const N: usize = 16;
        for fail in [false, true] {
            let ops = Arc::new(CountingOps {
                fail,
                ..CountingOps::default()
            });
            let dyn_ops: Arc<dyn OpsApi> = ops.clone();
            let gate = Arc::new(tokio::sync::Barrier::new(N));
            let tasks: Vec<_> = (0..N)
                .map(|_| {
                    let (ops, gate) = (dyn_ops.clone(), gate.clone());
                    tokio::spawn(async move {
                        gate.wait().await;
                        names_for(ops).await
                    })
                })
                .collect();
            for t in tasks {
                assert!(t.await.unwrap().is_empty());
            }
            for _ in 0..N {
                names_for(dyn_ops.clone()).await;
            }
            assert_eq!(ops.calls.load(Ordering::SeqCst), 1, "fail: {fail}");
        }
    }

    /// A listed source as `sources()` gives it, with only what
    /// [`plugin_name`] reads set.
    fn listed(plugin: &str, name: &str, consent: &str, finds_private: bool) -> crate::ops::Source {
        serde_json::from_value(serde_json::json!({
            "agents": { "count": 1, "enforced": false, "max": 1 },
            "consent": consent, "consent_required": false, "consent_version": "",
            "credentials": [], "editable": [],
            "egress": { "live": null, "setting": null, "warnings": [] },
            "enabled": true, "endpoints": [], "finds_private": finds_private,
            "health": { "harvested_24h": 0, "last_error": null, "last_run": null,
                "running": true, "stored_24h": 0 },
            "id": 0, "kinds": ["discovery"], "name": name, "needs": null, "networks": ["bt"],
            "not_yet_editable": [], "password_required": false, "plugin": plugin,
            "priority": "normal", "purge_pending": false, "restart_required": false,
            "risk": "low", "schedule": { "default": [], "enforced": false, "windows": [] },
            "state": "enabled", "version": "1"
        }))
        .unwrap()
    }

    /// SRC-11c: a Sonarr or Radarr grab of a torrent that's also stored
    /// publicly adds the webhooks' bit to that row. The webhooks source is
    /// consent None, yet a public viewer never sees it named, nor can pick
    /// it as a facet; the signed-in admin sees it, marked private.
    #[test]
    fn a_source_whose_finds_are_private_is_never_named_to_the_public() {
        let names: PluginNames = [
            listed("dht", "Mainline DHT", "none", false),
            listed("arr_webhooks", "Sonarr and Radarr webhooks", "none", true),
        ]
        .into_iter()
        .map(plugin_name)
        .collect();
        assert!(names["dht"].public);
        assert!(!names["arr_webhooks"].public);
        let c = Catalogue::of(
            [(0u8, "dht"), (1, "arr_webhooks")]
                .into_iter()
                .map(|(id, plugin)| SourceEntry {
                    id,
                    plugin: plugin.into(),
                    name: names[plugin].name.clone(),
                    public: Some(names[plugin].public),
                    always_adult: Some(names[plugin].always_adult),
                })
                .collect(),
        );
        // Stored publicly: found by the DHT and by the webhook, no
        // private-source flag.
        let r = row(0b11, Some(0), 0);
        let public = c.provenance(&r, &[], Viewer::Public);
        assert_eq!(plugins(&public), ["dht"]);
        assert_eq!(c.hit_source(&r, Viewer::Public).unwrap().plugin, "dht");
        assert!(!c.shows_plugin("arr_webhooks", Viewer::Public));
        let admin = c.provenance(&r, &[], Viewer::Admin);
        let mut seen = plugins(&admin);
        seen.sort_unstable();
        assert_eq!(seen, ["arr_webhooks", "dht"]);
        assert!(
            admin
                .sources
                .iter()
                .any(|s| s.plugin == "arr_webhooks" && s.private)
        );
        assert!(c.shows_plugin("arr_webhooks", Viewer::Admin));
    }

    #[test]
    fn plugin_names_follow_the_class() {
        let c = catalogue();
        for p in ["dht", "prowlarr", "nyaa", "gone", "never-registered"] {
            assert!(c.shows_plugin(p, Viewer::Admin));
        }
        assert!(c.shows_plugin("dht", Viewer::Public));
        assert!(c.shows_plugin("nyaa", Viewer::Public));
        for p in ["prowlarr", "gone", "never-registered"] {
            assert!(!c.shows_plugin(p, Viewer::Public), "{p}");
        }
    }

    #[test]
    fn differences_name_only_what_disagrees() {
        let r = row(0b1, Some(0), 0);
        let same = Claims {
            name: Some(r.name.clone()),
            size: Some(r.size),
            files: (0..3)
                .map(|i| ClaimedFile {
                    path: format!("f{i}"),
                    size: None,
                })
                .collect(),
            ..Claims::default()
        };
        let d = differences(&r, &same);
        assert!(d.matches && d.name.is_none() && d.size.is_none() && d.file_count.is_none());
        // Nothing stated is nothing to disagree with.
        assert!(differences(&r, &Claims::default()).matches);

        let long = "é".repeat(MAX_NAME);
        let other = Claims {
            name: Some(long.clone()),
            size: Some(5),
            files: vec![ClaimedFile {
                path: "x".into(),
                size: None,
            }],
            ..Claims::default()
        };
        let d = differences(&r, &other);
        assert!(!d.matches);
        let name = d.name.unwrap();
        assert!(name.claimed.len() <= MAX_NAME && long.starts_with(&name.claimed));
        assert_eq!(name.verified, r.name);
        assert_eq!(
            d.size,
            Some(ClaimedNumber {
                claimed: 5,
                verified: 4_000
            })
        );
        assert_eq!(
            d.file_count,
            Some(ClaimedNumber {
                claimed: 1,
                verified: 3
            })
        );
    }

    #[test]
    fn a_claimed_list_cut_at_the_limit_is_at_least_that_many() {
        let mut r = row(0b1, Some(0), 0);
        r.file_count = MAX_FILES as u64 + 50;
        let cut = Claims {
            files: vec![
                ClaimedFile {
                    path: "x".into(),
                    size: None,
                };
                MAX_FILES
            ],
            ..Claims::default()
        };
        assert!(differences(&r, &cut).file_count.is_none());
        r.file_count = 10;
        assert!(differences(&r, &cut).file_count.is_some());
    }
}
