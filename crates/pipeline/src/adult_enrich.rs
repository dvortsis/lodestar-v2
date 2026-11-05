//! The adult-enrichment runtime (architecture §18.1, §18.7, §20.5, §20.9;
//! plan `docs/plan/2026-11-04-v06-rulepacks.md` ENR-1 and Q2, Q3, Q11,
//! Q15; `docs/decisions/2026-11-05-ENR-1.md`): the part that decides what
//! leaves the install when ThePornDB or StashDB is asked about an item,
//! and what becomes of the answer.
//!
//! - **One gate** ([`check`], over [`eligible`]). Every lookup, user
//!   interest or background, goes through it right before the plugin is
//!   called. An item is eligible only when it's a stored, verified torrent,
//!   classified Adult (or found by an always-adult source, through
//!   [`AdultSources`] only), the plugin's source is enabled and its consent
//!   condition holds, and "Show adult content" is on (Q11). Never: a hash
//!   the floor, the core or a filter holds (tombstone, quarantine, a drop
//!   or quarantine verdict now, a hide), a `private_source` key, an
//!   unverified item, or anything while the plugin's egress kill switch is
//!   tripped (SRC-5).
//! - **What is sent** is a [`Query`]: the display name, or the release
//!   parser's site, date and title, whichever the plugin asks for. The info
//!   hash, files and trackers stay here: a [`Query`] has no field for them.
//! - **Order** (Q15): user interest first ([`Interest::opened`], the detail
//!   drawer), then a background backfill over stored torrents, at most
//!   [`BACKGROUND_PER_DAY`] a day, every lookup at least [`MIN_INTERVAL`]
//!   apart.
//! - **Results** are emitted as claims under the plugin's own source id, so
//!   they go through SRC-2's filter-before-storage path in the writer
//!   (`unverified::ingest`): the child-safety core judges the enrichment
//!   text, stricter only, and a match pins the adult kind
//!   (`enrich::apply_matches`).
//! - **Caches** in the plugin's `source_state` (Q15): a hit for
//!   [`HIT_TTL_SECS`], a miss for [`MISS_TTL_SECS`].
//!
//! Nothing here logs a name, a query or a hash.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_classify::{ContentType, Correction, ENRICHMENT_VERSION, Record, classify_corrected};
use lodestar_filter::{Action, Engine, Facts, Handle, Item};
use lodestar_metainfo::InfoHash;
use lodestar_sources::claims::ReleaseDate;
use lodestar_sources::enrich::{
    AdultEnricher, BACKGROUND_PER_DAY, HIT_TTL_SECS, Lookup, MIN_INTERVAL, MISS_TTL_SECS, Query,
    QueryForm, RUNTIME_PREFIX,
};
use lodestar_sources::state::MAX_VALUE;
use lodestar_sources::{
    BoxFuture, Claims, Found, Kit, Manifest, PluginError, PluginState, Policy, SourceCtx,
    SourcePlugin, StateError,
};
use lodestar_store::{SourceId, SourceState, Store, flags};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::source_adult::AdultSources;

/// The pipeline's own strategy code for an enrichment match, beside the
/// DHT's and verification's (`unverified::VERIFY_STRATEGY`).
pub const STRATEGY: u8 = 202;

/// Whether `d` is an enrichment match: an adult-enrichment section from an
/// enrichment source. It's the writer's gate exactly
/// (`unverified::ingest` keeps a section only from such a source), so the
/// strategy code isn't part of it. Intake doesn't count a match as
/// harvested (ENR-1b); another source's discovery always is.
pub fn is_match(d: &lodestar_sources::Discovery, enrichers: EnrichSources) -> bool {
    enrichers.has(d.source) && d.found.claims.as_deref().is_some_and(|c| c.adult.is_some())
}

/// User-interest lookups waiting at most; more are dropped. Each takes a
/// [`MIN_INTERVAL`], so this is about a minute of opened drawers.
pub const INTEREST_QUEUE: usize = 64;

/// Stored torrents one background page reads at most. Only the adult ones
/// among them are offered to the gate.
pub const PAGE: usize = 1024;

/// After the background cursor reaches the last torrent, it starts over
/// this much later (seconds).
pub const ROUND_REST_SECS: i64 = 86_400;

/// How long the runtime waits for user interest when it has nothing else
/// to do, before looking at the background again.
const IDLE: Duration = Duration::from_secs(60);

/// The pause between two background pages that held nothing adult.
const SCAN_PAUSE: Duration = Duration::from_millis(50);

/// The registered adult-enrichment sources, as bitsets over [`SourceId`]s
/// (the same layout as a torrent's `sources`).
///
/// - **Listed** ([`has`](Self::has)): the [`AdultEnrichSource`] plugins the
///   host runs (`PipelineConfig::adult_enrichers`, ENR-1c). Only these may
///   attach an adult-enrichment section to a claim
///   (`unverified::Ctx::enrichers`). Empty: none may.
/// - **Of the kind** ([`of_kind`](Self::of_kind)): every registered
///   `Kind::Enrichment` source, listed or not (the torrent cache is one). A
///   section-only claim from one that isn't listed is refused, never stored
///   as a find, since its answers aren't finds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnrichSources {
    pub listed: u64,
    pub kind: u64,
}

impl EnrichSources {
    /// The adult-enrichment sources among registered `(id, manifest)`
    /// pairs: those whose manifest id the host listed as an
    /// [`AdultEnrichSource`] (`PipelineConfig::adult_enrichers`), of kind
    /// `Enrichment`. The kind alone isn't enough: the torrent cache is that
    /// kind too, and it must not attach adult sections (ENR-1c).
    ///
    /// **An adult-enrichment source is never always adult.** Its matches
    /// are answers about stored torrents, not finds, so they never set its
    /// bit on a torrent and its mask would never apply.
    /// [`AdultEnrichSource::new`] refuses such a plugin, so none can run; a
    /// listed manifest that claims both is refused here too, and is a bug in
    /// debug builds.
    pub fn of<'a>(
        registered: impl IntoIterator<Item = (SourceId, &'a Manifest)>,
        listed: &[&str],
    ) -> EnrichSources {
        registered
            .into_iter()
            .filter(|(_, m)| {
                debug_assert!(
                    !(listed.contains(&m.id) && m.always_adult),
                    "an adult-enrichment source can't be always adult: {}",
                    m.id
                );
                m.is(lodestar_sources::Kind::Enrichment)
            })
            .fold(EnrichSources::default(), |set, (id, m)| EnrichSources {
                listed: set.listed
                    | if listed.contains(&m.id) && admitted(m) {
                        source_bit(id)
                    } else {
                        0
                    },
                kind: set.kind | source_bit(id),
            })
    }

    /// `bits` listed (and so of the kind).
    pub fn listed(bits: u64) -> EnrichSources {
        EnrichSources {
            listed: bits,
            kind: bits,
        }
    }

    /// A listed adult-enrichment source: it may attach sections.
    pub fn has(self, source: SourceId) -> bool {
        self.listed & source_bit(source) != 0
    }

    /// A registered `Kind::Enrichment` source, listed or not.
    pub fn of_kind(self, source: SourceId) -> bool {
        self.kind & source_bit(source) != 0
    }

    /// No adult-enrichment source is listed.
    pub fn is_empty(self) -> bool {
        self.listed == 0
    }
}

/// A manifest whose source may attach adult sections: an enrichment
/// source that isn't always adult.
pub(crate) fn admitted(m: &Manifest) -> bool {
    m.is(lodestar_sources::Kind::Enrichment) && !m.always_adult
}

fn source_bit(source: SourceId) -> u64 {
    1u64.checked_shl(u32::from(source.get())).unwrap_or(0)
}

/// What the gate needs from outside the store. The host implements it.
pub trait Switches: Send + Sync {
    /// "Show adult content" (§20.9). Nothing is looked up while it's off
    /// (Q11).
    fn show_adult(&self) -> bool;
    /// The plugin's egress kill switch has tripped (SRC-5): its route is
    /// down, so nothing may be sent through it.
    fn kill_switch_tripped(&self) -> bool;
}

/// Everything the gate looks at for one item. [`eligible`] decides from
/// this alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gate {
    /// "Show adult content" is on.
    pub show_adult: bool,
    /// The plugin's egress kill switch has tripped.
    pub kill_switch_tripped: bool,
    /// The plugin's source is enabled.
    pub source_enabled: bool,
    /// Its consent condition holds (Account: credentials stored, SRC-4).
    pub consent_met: bool,
    /// A stored torrent with verified metadata: not an unverified item,
    /// and not a partial import (`FILES_PARTIAL`).
    pub stored: bool,
    /// One of its swarm hashes is tombstoned, quarantined, or on the
    /// floor's or a dropping list.
    pub held: bool,
    /// A rule hides it (stored), or would drop, quarantine or hide it now.
    pub filtered: bool,
    /// Only private sources reported it (§18.7), or it's a private torrent
    /// (BEP 27).
    pub private_source: bool,
    /// Classified Adult, or found by an always-adult source (and you
    /// haven't said Not adult).
    pub adult: bool,
    /// Its stored classification is marked to be redone (a rules change,
    /// or a newer parser): until the backfill reaches it, its type may be
    /// stale, so it waits.
    pub pending: bool,
}

/// Why an item may not be looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ineligible {
    AdultHidden,
    KillSwitch,
    SourceOff,
    NoConsent,
    NotStored,
    Held,
    Filtered,
    PrivateSource,
    NotAdult,
    Pending,
    /// Eligible, but there's nothing to send in the plugin's form (no
    /// name, or no parsed title).
    NoQuery,
}

/// The eligibility rule (plan Q2): `Ok` only when every condition holds.
/// The never-rules come first, so a reason is always the strictest.
pub fn eligible(g: &Gate) -> Result<(), Ineligible> {
    let checks = [
        (!g.show_adult, Ineligible::AdultHidden),
        (g.kill_switch_tripped, Ineligible::KillSwitch),
        (!g.source_enabled, Ineligible::SourceOff),
        (!g.consent_met, Ineligible::NoConsent),
        (!g.stored, Ineligible::NotStored),
        (g.held, Ineligible::Held),
        (g.filtered, Ineligible::Filtered),
        (g.private_source, Ineligible::PrivateSource),
        (!g.adult, Ineligible::NotAdult),
        (g.pending, Ineligible::Pending),
    ];
    match checks.into_iter().find(|(fails, _)| *fails) {
        Some((_, why)) => Err(why),
        None => Ok(()),
    }
}

/// An item the gate passed: what to send, and the key the answer is
/// stored under. The key never leaves the install, and `Debug` prints
/// neither it nor the query's text.
#[derive(Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: i64,
    pub key: InfoHash,
    pub query: Query,
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate")
            .field("id", &self.id)
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

/// What the plugin-wide part of the gate reads, besides the store.
#[derive(Clone, Copy)]
pub struct GateCtx<'a> {
    pub manifest: &'a Manifest,
    pub source: SourceId,
    pub form: QueryForm,
    pub adult: AdultSources,
    pub switches: &'a dyn Switches,
}

impl std::fmt::Debug for GateCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateCtx")
            .field("plugin", &self.manifest.id)
            .field("source", &self.source)
            .field("form", &self.form)
            .field("adult", &self.adult)
            .finish_non_exhaustive()
    }
}

/// The gate, for torrent `id`: every fact [`eligible`] needs, read now,
/// then the query in the plugin's form. Every lookup goes through it. The
/// plugin-wide conditions are read first; while one fails, nothing is
/// read about the item.
pub fn check(
    store: &mut Store,
    engine: &Engine,
    ctx: GateCtx<'_>,
    id: i64,
    now: i64,
) -> Result<Result<Candidate, Ineligible>, lodestar_store::Error> {
    let mut g = Gate {
        show_adult: ctx.switches.show_adult(),
        kill_switch_tripped: ctx.switches.kill_switch_tripped(),
        ..Gate::default()
    };
    if g.show_adult && !g.kill_switch_tripped {
        g.source_enabled = store
            .sources()?
            .iter()
            .any(|s| s.id == ctx.source && s.state == SourceState::Enabled);
        g.consent_met = lodestar_sources::consent::need_in(store, ctx.manifest)?.is_none();
    }
    let item_wide = Gate {
        stored: true,
        adult: true,
        ..g
    };
    if let Err(why) = eligible(&item_wide) {
        return Ok(Err(why));
    }
    // An unverified item's id isn't a torrent's: not stored.
    let (Some(row), Some(hashes)) = (store.torrent(id)?, store.torrent_swarm_hashes(id)?) else {
        return Ok(Err(Ineligible::NotStored));
    };
    // A partial import's file list came from another indexer's database,
    // not from metadata that hashed to the key: not verified (stricter).
    g.stored = row.flags & flags::FILES_PARTIAL == 0;
    g.private_source = row.flags & (flags::PRIVATE_SOURCE | flags::PRIVATE) != 0;
    g.held =
        store.held_among(&hashes)?.into_iter().any(|h| h) || engine.blocked_hash(&hashes).is_some();
    let own = store.correction(id)?;
    let stored_adult = store.content_type(id)? == Some(ContentType::Adult.code());
    let not_adult = own.is_some_and(|c| c.adult == Some(false));
    g.adult = stored_adult || (ctx.adult.any(row.sources) && !not_adult);
    g.filtered = row.flags & flags::HIDDEN != 0;
    g.pending = store
        .enrichment(id)?
        .is_none_or(|e| e.version != ENRICHMENT_VERSION);
    // Cheap reasons first: the judgement below is only for an item that
    // could otherwise go.
    if let Err(why) = eligible(&g) {
        return Ok(Err(why));
    }
    let Some(tree) = store.files(id)? else {
        return Ok(Err(Ineligible::NotStored));
    };
    let files = crate::enrich::file_infos(&tree.files, tree.encoding);
    let class = classify_corrected(&row.name, &files, Correction::default());
    let facts = Facts {
        class: &class,
        size: row.size,
        piece_len: row.piece_len,
        flags: row.flags,
        discovered_at: row.discovered_at,
        now,
    };
    let item = Item {
        hashes: &hashes,
        name: &row.name,
        files: &files,
        content_type: class.content_type,
    };
    // As it would be judged now, as adult too (it's adult, or it isn't
    // eligible): a rule changed since it was stored can't be outrun.
    let (verdict, _) = crate::source_adult::judge(engine, &item, &facts, now);
    g.filtered |= verdict
        .decision
        .as_ref()
        .is_some_and(|d| matches!(d.action, Action::Drop | Action::Quarantine | Action::Hide));
    if let Err(why) = eligible(&g) {
        return Ok(Err(why));
    }
    // The torrent's name alone, never with its files: with no files, the
    // parse can't take a title from a file or folder name (Q2: never the
    // files). A name that says nothing useful ("New folder", ambiguous, or
    // resolved from the files) sends nothing in either form: it can't
    // match, costs budget, and tells the service an adult item was viewed
    // (ENR-1b, runner §2.33).
    let named = classify_corrected(&row.name, &[], Correction::default());
    if named.title.ambiguous || named.title.resolved.is_some() {
        return Ok(Err(Ineligible::NoQuery));
    }
    let query = match ctx.form {
        // The torrent's own name. A single-file torrent's name is its file
        // name, which is allowed: it's the display name.
        QueryForm::Name => Query::name(&row.name),
        QueryForm::Parsed => {
            let r = Record::new(&named);
            r.release("title").and_then(|title| {
                Query::parsed(
                    r.release("site"),
                    r.release("date").and_then(ReleaseDate::parse),
                    title,
                )
            })
        }
    };
    Ok(query
        .map(|query| Candidate {
            id,
            key: row.info_hash,
            query,
        })
        .ok_or(Ineligible::NoQuery))
}

/// The runtime's limits (plan Q15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Background lookups a day.
    pub per_day: u32,
    /// The least time between two lookups.
    pub interval: Duration,
    /// Stored torrents one background page reads.
    pub page: usize,
    /// The rest after a full background round, in seconds.
    pub rest_secs: i64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            per_day: BACKGROUND_PER_DAY,
            interval: MIN_INTERVAL,
            page: PAGE,
            rest_secs: ROUND_REST_SECS,
        }
    }
}

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct EnrichStats {
    /// Items handed to the plugin.
    pub looked_up: AtomicU64,
    pub matched: AtomicU64,
    pub missed: AtomicU64,
    /// Lookups that failed (the plugin's error), or a match the claims
    /// limits couldn't keep.
    pub failed: AtomicU64,
    /// Items the gate refused.
    pub ineligible: AtomicU64,
    /// Items passed over because a fresh hit or miss is cached.
    pub cached: AtomicU64,
    /// Items the runtime finished with, whatever the outcome.
    pub done: AtomicU64,
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

/// Where the detail drawer reports an opened item. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Interest(mpsc::Sender<i64>);

impl Interest {
    /// Asks for torrent `id` to be looked up next. False when the queue is
    /// full or the runtime has gone; the item waits for the background.
    pub fn opened(&self, id: i64) -> bool {
        self.0.try_send(id).is_ok()
    }
}

/// [`AdultEnrichSource::new`] refused a plugin: its manifest isn't an
/// enrichment source, or says it's always adult.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAdmitted;

/// An adult-enrichment plugin, run as a source of its own: the pipeline
/// registers, enables and stops it like any other, gives it its kit (the
/// client behind its egress and kill switch, and its `source_state`) and
/// its emitter, under its own source id.
pub struct AdultEnrichSource {
    plugin: Arc<dyn AdultEnricher>,
    store: Arc<Mutex<Store>>,
    filter: Arc<Handle>,
    adult: AdultSources,
    switches: Arc<dyn Switches>,
    interest: tokio::sync::Mutex<mpsc::Receiver<i64>>,
    config: Config,
    /// The plugin's policy with Q15's per-request cap applied.
    policy: Policy,
    stats: Arc<EnrichStats>,
}

impl std::fmt::Debug for AdultEnrichSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdultEnrichSource")
            .field("plugin", &self.plugin.manifest().id)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl AdultEnrichSource {
    /// `store` is a connection of the runtime's own (it only reads, apart
    /// from its `source_state`); `filter` is the live filter; `adult` the
    /// registered always-adult sources.
    pub fn new(
        plugin: Arc<dyn AdultEnricher>,
        store: Arc<Mutex<Store>>,
        filter: Arc<Handle>,
        adult: AdultSources,
        switches: Arc<dyn Switches>,
        config: Config,
    ) -> Result<(AdultEnrichSource, Interest), NotAdmitted> {
        // Its answers aren't finds, so an always-adult mask on it would
        // never apply (ENR-1c).
        if !admitted(plugin.manifest()) {
            return Err(NotAdmitted);
        }
        let (tx, rx) = mpsc::channel(INTEREST_QUEUE);
        let policy = capped(*plugin.policy());
        let source = AdultEnrichSource {
            policy,
            plugin,
            store,
            filter,
            adult,
            switches,
            interest: tokio::sync::Mutex::new(rx),
            config,
            stats: Arc::default(),
        };
        Ok((source, Interest(tx)))
    }

    pub fn stats(&self) -> Arc<EnrichStats> {
        self.stats.clone()
    }

    async fn run_inner(&self, mut ctx: SourceCtx) -> Result<(), PluginError> {
        // One run at a time holds the interest queue.
        let mut interest = self.interest.lock().await;
        let state = ctx.kit.state.clone();
        // The plugin gets the same client, and its state without the
        // runtime's keys.
        let kit = Kit {
            state: Arc::new(Guarded(state.clone())),
            http: ctx.kit.http.clone(),
        };
        let mut background = Background::load(&*state).await;
        let mut last: Option<Instant> = None;
        // An opened item that arrived while the runtime waited.
        let mut waiting: Option<i64> = None;
        loop {
            if ctx.shutdown.is_requested() {
                return Ok(());
            }
            if let Some(at) = last {
                tokio::select! {
                    biased;
                    _ = ctx.shutdown.requested() => return Ok(()),
                    _ = tokio::time::sleep_until(at + self.config.interval) => {}
                }
            }
            let now = crate::unix_now();
            let next = waiting
                .take()
                .map(Ok)
                .unwrap_or_else(|| interest.try_recv());
            let (id, user) = match next {
                Ok(id) => (id, true),
                Err(_) => match self.next_background(&*state, &mut background, now).await {
                    Next::Item(id) => (id, false),
                    // A page with nothing adult: on to the next, paced so
                    // a round over a large store is a light, steady load.
                    Next::Again => {
                        tokio::select! {
                            biased;
                            _ = ctx.shutdown.requested() => return Ok(()),
                            _ = tokio::time::sleep(SCAN_PAUSE) => {}
                        }
                        continue;
                    }
                    Next::Idle => tokio::select! {
                        biased;
                        _ = ctx.shutdown.requested() => return Ok(()),
                        Some(id) = interest.recv() => (id, true),
                        _ = tokio::time::sleep(IDLE) => continue,
                    },
                },
            };
            match self.one(&mut ctx, &kit, &*state, id, user).await {
                Step::Asked => last = Some(Instant::now()),
                Step::Skipped => {}
                Step::Wait => {
                    // Nothing can go now (a plugin-wide condition, or the
                    // day's budget): a background item goes back to the
                    // front of the queue, not past the cursor.
                    if !user {
                        background.queue.push_front(id);
                    }
                    bump(&self.stats.done);
                    tokio::select! {
                        biased;
                        _ = ctx.shutdown.requested() => return Ok(()),
                        Some(id) = interest.recv(), if waiting.is_none() => waiting = Some(id),
                        _ = tokio::time::sleep(IDLE) => {}
                    }
                    continue;
                }
                Step::Shutdown => return Ok(()),
            }
            if !user {
                background.done(&*state, id).await;
            }
            bump(&self.stats.done);
        }
    }

    /// The next background id to try. [`Next::Idle`] while there's none:
    /// the day's budget is spent, the round is resting, "Show adult
    /// content" is off, or the store can't be read. A page keeps only the
    /// torrents stored as adult or found by an always-adult source, so the
    /// gate isn't asked about the rest.
    async fn next_background(
        &self,
        state: &dyn PluginState,
        bg: &mut Background,
        now: i64,
    ) -> Next {
        if let Some(id) = bg.queue.pop_front() {
            return Next::Item(id);
        }
        if now < bg.resume_at || !self.switches.show_adult() {
            return Next::Idle;
        }
        if Budget::load(state).await.left(now, self.config.per_day) == 0 {
            return Next::Idle;
        }
        let store = self.store.clone();
        let (after, page, adult) = (bg.after, self.config.page.clamp(1, PAGE), self.adult);
        let read = tokio::task::spawn_blocking(move || {
            let store = store.lock().unwrap_or_else(|e| e.into_inner());
            let ids = store.ids_after(after, page)?;
            let mut keep = Vec::new();
            for &id in &ids {
                let stored_adult = store.content_type(id)? == Some(ContentType::Adult.code());
                if stored_adult
                    || (!adult.is_empty()
                        && store.torrent(id)?.is_some_and(|r| adult.any(r.sources)))
                {
                    keep.push(id);
                }
            }
            Ok::<_, lodestar_store::Error>((ids.last().copied(), keep))
        })
        .await;
        let Ok(Ok((last, keep))) = read else {
            return Next::Idle;
        };
        match last {
            Some(last) => bg.after = last,
            None => {
                bg.after = 0;
                bg.resume_at = now.saturating_add(self.config.rest_secs);
            }
        }
        bg.queue.extend(keep);
        if bg.queue.is_empty() {
            // Nothing on the page to try: the cursor moves past it.
            bg.save(state, bg.after).await;
        }
        match bg.queue.pop_front() {
            Some(id) => Next::Item(id),
            None if last.is_some() => Next::Again,
            None => Next::Idle,
        }
    }

    /// One item: the cache, the gate, the budget, then the plugin.
    async fn one(
        &self,
        ctx: &mut SourceCtx,
        kit: &Kit,
        state: &dyn PluginState,
        id: i64,
        user: bool,
    ) -> Step {
        let now = crate::unix_now();
        if Cache::fresh(state, id, now).await {
            bump(&self.stats.cached);
            return Step::Skipped;
        }
        let candidate = {
            let store = self.store.clone();
            let engine = self.filter.current();
            let manifest = self.plugin.manifest().clone();
            let switches = self.switches.clone();
            let gate = (ctx.source, self.plugin.query_form(), self.adult);
            tokio::task::spawn_blocking(move || {
                let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
                let ctx = GateCtx {
                    manifest: &manifest,
                    source: gate.0,
                    form: gate.1,
                    adult: gate.2,
                    switches: &*switches,
                };
                check(&mut store, &engine, ctx, id, now)
            })
            .await
        };
        let candidate = match candidate {
            Ok(Ok(Ok(c))) => c,
            Ok(Ok(Err(why))) => {
                bump(&self.stats.ineligible);
                return match why {
                    Ineligible::AdultHidden
                    | Ineligible::KillSwitch
                    | Ineligible::SourceOff
                    | Ineligible::NoConsent => Step::Wait,
                    _ => Step::Skipped,
                };
            }
            // The store couldn't say: never asked, and tried again later.
            _ => {
                bump(&self.stats.ineligible);
                return Step::Wait;
            }
        };
        if !user && !Budget::take(state, now, self.config.per_day).await {
            return Step::Wait;
        }
        bump(&self.stats.looked_up);
        let lookup = tokio::select! {
            biased;
            _ = ctx.shutdown.requested() => return Step::Shutdown,
            r = self.plugin.lookup(kit, &candidate.query) => r,
        };
        match lookup {
            Ok(Lookup::Match(section)) => {
                let mut claims = Claims {
                    adult: Some(section),
                    ..Claims::default()
                };
                claims.clamp();
                if claims.adult.is_none() {
                    // A section the limits can't keep is no match: and a
                    // discovery without claims would read as a sighting.
                    bump(&self.stats.failed);
                    return Step::Asked;
                }
                let found = Found::key(candidate.key, STRATEGY).with_claims(claims);
                tokio::select! {
                    biased;
                    _ = ctx.shutdown.requested() => return Step::Shutdown,
                    sent = ctx.emitter.emit(found) => if sent.is_err() { return Step::Shutdown },
                }
                bump(&self.stats.matched);
                Cache::remember(state, id, now.saturating_add(HIT_TTL_SECS), now).await;
            }
            Ok(Lookup::NoMatch) => {
                bump(&self.stats.missed);
                Cache::remember(state, id, now.saturating_add(MISS_TTL_SECS), now).await;
            }
            Err(_) => bump(&self.stats.failed),
        }
        Step::Asked
    }
}

enum Next {
    Item(i64),
    /// A page was read but held nothing to try: read the next.
    Again,
    Idle,
}

enum Step {
    /// The plugin was called: the next lookup waits the interval.
    Asked,
    /// This item isn't looked up.
    Skipped,
    /// Nothing may be looked up now: wait, keeping the item's place.
    Wait,
    Shutdown,
}

impl SourcePlugin for AdultEnrichSource {
    fn manifest(&self) -> &Manifest {
        self.plugin.manifest()
    }

    fn policy(&self) -> &Policy {
        &self.policy
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(self.run_inner(ctx))
    }
}

/// `p` with its per-host rate held to Q15's one request a second and no
/// burst, so the host's limiter enforces the cap per request, however
/// many requests one lookup makes.
pub fn capped(mut p: Policy) -> Policy {
    p.caps.per_host.interval = p.caps.per_host.interval.max(MIN_INTERVAL);
    p.caps.per_host.burst = 1;
    p
}

/// The plugin's view of its `source_state`: everything but the runtime's
/// own keys under [`RUNTIME_PREFIX`], which it can neither read nor
/// write, so a plugin can't reset the caps or caches.
#[derive(Debug)]
struct Guarded(Arc<dyn PluginState>);

impl Guarded {
    fn refuse(key: &str) -> Result<(), StateError> {
        if key.starts_with(RUNTIME_PREFIX) {
            Err(StateError::BadKey)
        } else {
            Ok(())
        }
    }
}

impl PluginState for Guarded {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, StateError>> {
        Box::pin(async move {
            Guarded::refuse(key)?;
            self.0.get(key).await
        })
    }

    fn set<'a>(&'a self, key: &'a str, value: Vec<u8>) -> BoxFuture<'a, Result<(), StateError>> {
        Box::pin(async move {
            Guarded::refuse(key)?;
            self.0.set(key, value).await
        })
    }

    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), StateError>> {
        Box::pin(async move {
            Guarded::refuse(key)?;
            self.0.remove(key).await
        })
    }

    fn compare_and_set<'a>(
        &'a self,
        key: &'a str,
        expected: Option<&'a [u8]>,
        new: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<bool, StateError>> {
        Box::pin(async move {
            Guarded::refuse(key)?;
            self.0.compare_and_set(key, expected, new).await
        })
    }
}

/// The background cursor, kept in `source_state` so a restart goes on
/// where it stopped.
#[derive(Debug, Default)]
struct Background {
    after: i64,
    resume_at: i64,
    queue: VecDeque<i64>,
}

const CURSOR_KEY: &str = "enr1/cursor";
const BUDGET_KEY: &str = "enr1/budget";

fn i64_at(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

impl Background {
    async fn load(state: &dyn PluginState) -> Background {
        let v = state
            .get(CURSOR_KEY)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        Background {
            after: i64_at(&v, 0).unwrap_or(0),
            resume_at: i64_at(&v, 8).unwrap_or(0),
            queue: VecDeque::new(),
        }
    }

    /// Saves the cursor at `through`, the last id dealt with, so a restart
    /// goes on after it and never past an item still queued. Best effort:
    /// a lost cursor only starts the round over.
    async fn save(&self, state: &dyn PluginState, through: i64) {
        let mut v = through.to_be_bytes().to_vec();
        v.extend(self.resume_at.to_be_bytes());
        let _ = state.set(CURSOR_KEY, v).await;
    }

    /// `id` is dealt with.
    async fn done(&self, state: &dyn PluginState, id: i64) {
        let through = if self.queue.is_empty() {
            self.after
        } else {
            id
        };
        self.save(state, through).await;
    }
}

/// The day's background lookups (Q15), counted per UTC day.
struct Budget {
    day: i64,
    used: u32,
}

impl Budget {
    async fn load(state: &dyn PluginState) -> Budget {
        let v = state
            .get(BUDGET_KEY)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        Budget {
            day: i64_at(&v, 0).unwrap_or(i64::MIN),
            used: v
                .get(8..12)
                .and_then(|b| b.try_into().ok())
                .map_or(0, u32::from_be_bytes),
        }
    }

    fn left(&self, now: i64, per_day: u32) -> u32 {
        if self.day == now.div_euclid(86_400) {
            per_day.saturating_sub(self.used)
        } else {
            per_day
        }
    }

    /// Takes one lookup from today's budget. False when it's spent, or the
    /// count can't be written: a budget that can't be kept isn't spent.
    async fn take(state: &dyn PluginState, now: i64, per_day: u32) -> bool {
        let b = Budget::load(state).await;
        if b.left(now, per_day) == 0 {
            return false;
        }
        let day = now.div_euclid(86_400);
        let used = if b.day == day { b.used + 1 } else { 1 };
        let mut v = day.to_be_bytes().to_vec();
        v.extend(used.to_be_bytes());
        state.set(BUDGET_KEY, v).await.is_ok()
    }
}

/// Hits and misses (Q15), in [`BUCKETS`] `source_state` values keyed by
/// torrent id: each entry is the id and when it expires, so a bucket holds
/// [`PER_BUCKET`] entries and the whole cache about half a million, well
/// past what the daily caps can fill within the hit lifetime.
struct Cache;

/// Cache buckets: an eighth of a plugin's 1,024 state keys.
pub const BUCKETS: i64 = 128;
const ENTRY: usize = 16;
/// Entries one bucket holds at most.
pub const PER_BUCKET: usize = MAX_VALUE / ENTRY;

impl Cache {
    fn key(id: i64) -> String {
        format!("{RUNTIME_PREFIX}cache/{:03}", id.rem_euclid(BUCKETS))
    }

    fn decode(v: &[u8]) -> Vec<(i64, i64)> {
        v.chunks_exact(ENTRY)
            .filter_map(|c| Some((i64_at(c, 0)?, i64_at(c, 8)?)))
            .collect()
    }

    /// A fresh entry is cached for `id`. A bucket that can't be read
    /// counts as empty: the item is asked again, which the caps bound.
    async fn fresh(state: &dyn PluginState, id: i64, now: i64) -> bool {
        let v = state.get(&Cache::key(id)).await.ok().flatten();
        Cache::decode(&v.unwrap_or_default())
            .iter()
            .any(|&(i, expires)| i == id && expires > now)
    }

    /// Caches `id` until `expires`, dropping expired entries and, in a full
    /// bucket, the ones that expire soonest. Best effort.
    async fn remember(state: &dyn PluginState, id: i64, expires: i64, now: i64) {
        let key = Cache::key(id);
        let v = state.get(&key).await.ok().flatten().unwrap_or_default();
        let mut entries = Cache::decode(&v);
        entries.retain(|&(i, e)| i != id && e > now);
        entries.push((id, expires));
        if entries.len() > PER_BUCKET {
            entries.sort_by_key(|&(_, e)| std::cmp::Reverse(e));
            entries.truncate(PER_BUCKET);
        }
        let mut out = Vec::with_capacity(entries.len() * ENTRY);
        for (i, e) in entries {
            out.extend(i.to_be_bytes());
            out.extend(e.to_be_bytes());
        }
        let _ = state.set(&key, out).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_sources::MemoryState;
    use proptest::prelude::*;

    fn gate() -> impl Strategy<Value = Gate> {
        proptest::collection::vec(any::<bool>(), 10).prop_map(|b| Gate {
            show_adult: b[0],
            kill_switch_tripped: b[1],
            source_enabled: b[2],
            consent_met: b[3],
            stored: b[4],
            held: b[5],
            filtered: b[6],
            private_source: b[7],
            adult: b[8],
            pending: b[9],
        })
    }

    proptest! {
        /// Eligible exactly when every condition holds: no never-rule can
        /// be outweighed by the others.
        #[test]
        fn eligible_only_when_every_condition_holds(g in gate()) {
            let all = g.show_adult
                && !g.kill_switch_tripped
                && g.source_enabled
                && g.consent_met
                && g.stored
                && !g.held
                && !g.filtered
                && !g.private_source
                && g.adult
                && !g.pending;
            prop_assert_eq!(eligible(&g).is_ok(), all);
        }
    }

    #[test]
    fn only_a_match_skips_the_harvest() {
        let key = InfoHash([1; 20]);
        let source = SourceId::new(3).unwrap();
        let d = |found: Found| lodestar_sources::Discovery {
            source,
            found,
            at: 0,
            reply: false,
        };
        let section = Claims {
            adult: Some(lodestar_sources::claims::AdultEnrichment::new("1")),
            ..Claims::default()
        };
        let named = Claims {
            name: Some("n".into()),
            ..Claims::default()
        };
        let enr = EnrichSources::listed(1 << source.get());
        let m = d(Found::key(key, STRATEGY).with_claims(section.clone()));
        assert!(is_match(&m, enr));
        // Another source sending a section is still harvested.
        assert!(!is_match(&m, EnrichSources::default()));
        assert!(!is_match(
            &d(Found::key(key, STRATEGY).with_claims(named)),
            enr
        ));
        assert!(!is_match(&d(Found::key(key, STRATEGY)), enr));
        // The writer's gate doesn't read the strategy code, so neither does
        // this: a section from a listed adult-enrichment source is a match.
        assert!(is_match(&d(Found::key(key, 0).with_claims(section)), enr));
    }

    /// Only listed adult-enrichment plugins are enrichment sources: an
    /// unlisted source of the same kind (the torrent cache) isn't.
    #[test]
    fn enrichment_sources_are_the_listed_adult_enrichers() {
        let mut m = lodestar_sources::Manifest {
            id: "x",
            name: "x",
            version: "1",
            kinds: &[lodestar_sources::Kind::Enrichment],
            networks: &[],
            endpoints: &[],
            risk: lodestar_sources::Risk::Low,
            consent: lodestar_sources::Consent::Account,
            default_priority: lodestar_sources::Priority::Normal,
            enabled_by_default: false,
            always_adult: false,
        };
        let a = SourceId::new(2).unwrap();
        let b = SourceId::new(5).unwrap();
        let feed = lodestar_sources::Manifest {
            kinds: &[lodestar_sources::Kind::Discovery],
            ..m.clone()
        };
        m.id = "y";
        let c = SourceId::new(7).unwrap();
        let cache = lodestar_sources::Manifest {
            id: "torrent-cache",
            ..m.clone()
        };
        let set = EnrichSources::of([(a, &m), (b, &feed), (c, &cache)], &["y", "x"]);
        assert!(set.has(a) && !set.has(b), "listed and of the kind");
        assert!(!set.has(c), "the kind alone isn't enough");
        assert!(set.of_kind(a) && set.of_kind(c) && !set.of_kind(b));
        // Listing a feed doesn't make it one either.
        assert!(!EnrichSources::of([(b, &feed)], &["y"]).has(b));
        assert!(!EnrichSources::of([(a, &m)], &[]).has(a), "none listed");
        assert!(!EnrichSources::default().has(a));
        // Enrichment and always adult together is refused.
        let both = lodestar_sources::Manifest {
            always_adult: true,
            ..m.clone()
        };
        assert!(admitted(&m) && !admitted(&both) && !admitted(&feed));
    }

    /// Registering an enrichment source that's always adult is a bug.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "can't be always adult")]
    fn an_always_adult_enrichment_source_is_a_bug() {
        let m = lodestar_sources::Manifest {
            id: "both",
            name: "both",
            version: "1",
            kinds: &[lodestar_sources::Kind::Enrichment],
            networks: &[],
            endpoints: &[],
            risk: lodestar_sources::Risk::Low,
            consent: lodestar_sources::Consent::Account,
            default_priority: lodestar_sources::Priority::Normal,
            enabled_by_default: false,
            always_adult: true,
        };
        let _ = EnrichSources::of([(SourceId::new(2).unwrap(), &m)], &["both"]);
    }

    #[test]
    fn the_strictest_reason_comes_first() {
        let g = Gate {
            held: true,
            private_source: true,
            ..Gate::default()
        };
        assert_eq!(eligible(&g), Err(Ineligible::AdultHidden));
        let g = Gate {
            show_adult: true,
            source_enabled: true,
            consent_met: true,
            stored: true,
            held: true,
            adult: true,
            ..Gate::default()
        };
        assert_eq!(eligible(&g), Err(Ineligible::Held));
    }

    #[tokio::test]
    async fn the_cache_keeps_hits_and_misses_until_they_expire() {
        let s = MemoryState::new();
        assert!(!Cache::fresh(&s, 7, 100).await);
        Cache::remember(&s, 7, 200, 100).await;
        Cache::remember(&s, 7 + BUCKETS, 150, 100).await;
        assert!(Cache::fresh(&s, 7, 199).await);
        assert!(!Cache::fresh(&s, 7, 200).await);
        assert!(Cache::fresh(&s, 7 + BUCKETS, 149).await);
        // Expired entries make room when the bucket is written again.
        Cache::remember(&s, 7 + 2 * BUCKETS, 400, 300).await;
        let v = s.get(&Cache::key(7)).await.unwrap().unwrap();
        assert_eq!(Cache::decode(&v), vec![(7 + 2 * BUCKETS, 400)]);
    }

    #[tokio::test]
    async fn a_full_bucket_drops_what_expires_soonest() {
        let s = MemoryState::new();
        let mut v = Vec::new();
        for n in 0..PER_BUCKET as i64 {
            v.extend((n * BUCKETS).to_be_bytes());
            v.extend((1_000 + n).to_be_bytes());
        }
        s.set(&Cache::key(0), v).await.unwrap();
        Cache::remember(&s, PER_BUCKET as i64 * BUCKETS, 5_000, 0).await;
        let v = s.get(&Cache::key(0)).await.unwrap().unwrap();
        assert!(v.len() <= MAX_VALUE);
        let entries = Cache::decode(&v);
        assert_eq!(entries.len(), PER_BUCKET);
        assert!(entries.iter().all(|&(_, e)| e != 1_000), "soonest dropped");
        assert!(Cache::fresh(&s, PER_BUCKET as i64 * BUCKETS, 1).await);
    }

    #[tokio::test]
    async fn the_daily_budget_resets_each_day() {
        let s = MemoryState::new();
        let day = 20_000 * 86_400;
        assert!(Budget::take(&s, day, 2).await);
        assert!(Budget::take(&s, day + 5, 2).await);
        assert!(!Budget::take(&s, day + 10, 2).await);
        assert_eq!(Budget::load(&s).await.left(day + 10, 2), 0);
        assert!(Budget::take(&s, day + 86_400, 2).await);
        assert_eq!(Budget::load(&s).await.left(day + 86_400, 2), 1);
    }
}
