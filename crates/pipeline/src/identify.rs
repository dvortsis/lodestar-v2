//! The general enrichment runtime (architecture §6, §18.1, §18.2, §18.7,
//! §20.9; plan `docs/plan/2026-11-05-v07-m7.md` ENR-10, Q5 and Q12;
//! `docs/decisions/2026-11-12-ENR-10.md`): what leaves the install when a
//! service such as Wikidata or TMDB is asked about an item, and what
//! becomes of the answer. ENR-1's [`adult_enrich`](crate::adult_enrich)
//! is the adult path; nothing here touches it.
//!
//! - **One gate** ([`check`], over [`eligible`]). Every lookup, user
//!   interest or background, goes through it right before the plugin is
//!   called. An item is eligible only when it's a stored torrent with
//!   verified metadata (not a partial import), the plugin's source is
//!   enabled and its consent condition holds, and its egress kill switch
//!   hasn't tripped (SRC-5). Never (Q5): a hash the floor, the core or a
//!   filter holds (tombstone, quarantine, a drop, quarantine or hide
//!   verdict now, a stored hide), a `private_source` or BEP 27 private
//!   torrent, an unverified item, or anything Adult: stored as adult,
//!   found by an always-adult source, made adult by your correction or
//!   rule, or detected as adult now. Adult goes only to ENR-1's services.
//! - **What is sent** is a [`Query`]: the title and year the release
//!   parser reads from the torrent's name alone, and its kind of work.
//!   Never the hash, the raw name, files or trackers: a [`Query`] has no
//!   field for them. A name that says nothing (ambiguous, or resolved from
//!   the files) sends nothing, and neither does an item of a kind the
//!   plugin doesn't serve.
//! - **Order** (Q12): user interest first ([`Interest::opened`], the detail
//!   drawer), then a background backfill over stored torrents, within one
//!   cap for both: at most [`PER_DAY`] lookups a UTC day. Every lookup is
//!   at least the plugin's interval apart.
//! - **Results** are emitted as claims under the plugin's own source id
//!   ([`Identification::into_claims`]: ids and a details section that
//!   always holds the match confidence). They go through the writer's
//!   filter-before-storage path (`unverified::ingest`), which judges every
//!   text with the core, stricter only, and never stores a held one. This
//!   module doesn't judge the answer itself (ENR-9b's rule), but never
//!   skips that path either.
//! - **Caches** in the plugin's `source_state`: a hit for
//!   [`HIT_TTL_SECS`], a miss for [`MISS_TTL_SECS`]. The cursor and the
//!   day's counts live there too, so a restart goes on where it stopped.
//! - **Off by default** (§18.7): nothing runs until a host registers a
//!   plugin, and nothing is sent until you enable it and its consent holds.
//!
//! Nothing here logs a name, a query or a hash.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_classify::{
    Classification, ContentType, Correction, ENRICHMENT_VERSION, PluginTitle, Record,
    classify_corrected, via_plugin,
};
use lodestar_filter::{Action, Engine, Facts, Handle, Item};
use lodestar_metainfo::InfoHash;
use lodestar_sources::identify::{
    HIT_TTL_SECS, Identifier, Lookup, MIN_INTERVAL_FLOOR, MISS_TTL_SECS, MediaKind, PER_DAY, Query,
    RUNTIME_PREFIX,
};
use lodestar_sources::state::MAX_VALUE;
use lodestar_sources::{
    BoxFuture, Found, Kit, Manifest, PluginError, PluginState, Policy, SourceCtx, SourcePlugin,
    StateError,
};
use lodestar_store::{Enriched, SourceId, SourceState, Store, flags};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::source_adult::AdultSources;

/// The pipeline's strategy code for an identification, beside the DHT's,
/// verification's (201) and adult enrichment's (202).
pub const STRATEGY: u8 = 203;

/// User-interest lookups waiting at most; more are dropped and wait for
/// the background.
pub const INTEREST_QUEUE: usize = 64;

/// Stored torrents one background page reads at most.
pub const PAGE: usize = 1024;

/// After the background cursor reaches the last torrent, it starts over
/// this much later (seconds).
pub const ROUND_REST_SECS: i64 = 86_400;

/// How long the runtime waits for user interest when it has nothing else
/// to do, or nothing may be looked up, before trying again.
pub const IDLE: Duration = Duration::from_secs(60);

/// The shortest [`Config::idle`] a host can set.
pub const MIN_IDLE: Duration = Duration::from_millis(10);

/// The pause after a background item the gate refused, so a round over a
/// large store is a light, steady load (about 200 items a second).
const SKIP_PAUSE: Duration = Duration::from_millis(5);

/// What the gate needs from outside the store. The host implements it.
pub trait Switches: Send + Sync {
    /// The plugin's egress kill switch has tripped (SRC-5): its route is
    /// down, so nothing may be sent through it.
    fn kill_switch_tripped(&self) -> bool;
}

/// Everything the gate looks at for one item. [`eligible`] decides from
/// this alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gate {
    pub kill_switch_tripped: bool,
    pub source_enabled: bool,
    /// Its consent condition holds (SRC-4).
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
    /// (BEP 27, verified or not).
    pub private_source: bool,
    /// Adult by any reading: stored, an always-adult source, your
    /// correction or rule, or detection now.
    pub adult: bool,
    /// Its stored classification is marked to be redone: until the
    /// backfill reaches it, its type may be stale, so it waits.
    pub pending: bool,
}

/// Why an item may not be looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ineligible {
    KillSwitch,
    SourceOff,
    NoConsent,
    NotStored,
    Held,
    Filtered,
    PrivateSource,
    Adult,
    Pending,
    /// Eligible, but there's nothing to send: the name says nothing, or
    /// the item is a kind of work the plugin doesn't serve.
    NoQuery,
}

impl Ineligible {
    /// A plugin-wide condition: nothing may be looked up until it changes.
    fn plugin_wide(self) -> bool {
        matches!(
            self,
            Ineligible::KillSwitch | Ineligible::SourceOff | Ineligible::NoConsent
        )
    }
}

/// The eligibility rule (plan Q5): `Ok` only when every condition holds.
/// The plugin-wide conditions come first, then the never-rules.
pub fn eligible(g: &Gate) -> Result<(), Ineligible> {
    let checks = [
        (g.kill_switch_tripped, Ineligible::KillSwitch),
        (!g.source_enabled, Ineligible::SourceOff),
        (!g.consent_met, Ineligible::NoConsent),
        (!g.stored, Ineligible::NotStored),
        (g.held, Ineligible::Held),
        (g.filtered, Ineligible::Filtered),
        (g.private_source, Ineligible::PrivateSource),
        (g.adult, Ineligible::Adult),
        (g.pending, Ineligible::Pending),
    ];
    match checks.into_iter().find(|(fails, _)| *fails) {
        Some((_, why)) => Err(why),
        None => Ok(()),
    }
}

/// The kind of work a classification names, if a plugin could know it.
pub fn media_kind(t: ContentType) -> Option<MediaKind> {
    Some(match t {
        ContentType::Movie => MediaKind::Movie,
        ContentType::Tv => MediaKind::Tv,
        ContentType::Music => MediaKind::Music,
        ContentType::Audiobook => MediaKind::Audiobook,
        ContentType::Ebook => MediaKind::Ebook,
        ContentType::Comic => MediaKind::Comic,
        ContentType::Software => MediaKind::Software,
        ContentType::Game => MediaKind::Game,
        ContentType::Images | ContentType::Video | ContentType::Other | ContentType::Adult => {
            return None;
        }
    })
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
    /// The kinds of work the plugin serves.
    pub kinds: &'a [MediaKind],
    pub adult: AdultSources,
    pub switches: &'a dyn Switches,
}

impl std::fmt::Debug for GateCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateCtx")
            .field("plugin", &self.manifest.id)
            .field("source", &self.source)
            .field("kinds", &self.kinds)
            .field("adult", &self.adult)
            .finish_non_exhaustive()
    }
}

/// The gate, for torrent `id`: every fact [`eligible`] needs, read now,
/// then the query. Every lookup goes through it. The plugin-wide
/// conditions are read first; while one fails, nothing is read about the
/// item.
pub fn check(
    store: &mut Store,
    engine: &Engine,
    ctx: GateCtx<'_>,
    id: i64,
    now: i64,
) -> Result<Result<Candidate, Ineligible>, lodestar_store::Error> {
    let mut g = Gate {
        kill_switch_tripped: ctx.switches.kill_switch_tripped(),
        ..Gate::default()
    };
    if !g.kill_switch_tripped {
        g.source_enabled = store
            .sources()?
            .iter()
            .any(|s| s.id == ctx.source && s.state == SourceState::Enabled);
        g.consent_met = lodestar_sources::consent::need_in(store, ctx.manifest)?.is_none();
    }
    let plugin_wide = Gate { stored: true, ..g };
    if let Err(why) = eligible(&plugin_wide) {
        return Ok(Err(why));
    }
    // An unverified item's id isn't a torrent's: not stored.
    let (Some(row), Some(hashes)) = (store.torrent(id)?, store.torrent_swarm_hashes(id)?) else {
        return Ok(Err(Ineligible::NotStored));
    };
    // A partial import's file list came from another indexer's database,
    // not from metadata that hashed to the key: not verified.
    g.stored = row.flags & flags::FILES_PARTIAL == 0;
    g.private_source = row.flags & flags::PRIVACY != 0;
    g.held =
        store.held_among(&hashes)?.into_iter().any(|h| h) || engine.blocked_hash(&hashes).is_some();
    g.filtered = row.flags & flags::HIDDEN != 0;
    let own = store
        .correction(id)?
        .map(crate::enrich::correction)
        .unwrap_or_default();
    // An always-adult source among its finders makes it Is adult, unless
    // your own correction says Not adult.
    let own = crate::source_adult::correction(own, row.sources, ctx.adult);
    let stored_type = store.content_type(id)?;
    g.adult = stored_type == Some(ContentType::Adult.code()) || own.adult == Some(true);
    g.pending = store
        .enrichment(id)?
        .is_none_or(|e| e.version != ENRICHMENT_VERSION);
    // Cheap reasons first: what follows is only for an item that could
    // otherwise go.
    if let Err(why) = eligible(&g) {
        return Ok(Err(why));
    }
    let Some(tree) = store.files(id)? else {
        return Ok(Err(Ineligible::NotStored));
    };
    let files = crate::enrich::file_infos(&tree.files, tree.encoding);
    let class = classify_corrected(&row.name, &files, own);
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
    // As it would be judged now: a rule changed since it was stored can't
    // be outrun, and an Is adult rule makes it adult.
    let (verdict, fix) = crate::source_adult::judge_maybe(engine, &item, &facts, false, now);
    g.filtered |= verdict
        .decision
        .as_ref()
        .is_some_and(|d| matches!(d.action, Action::Drop | Action::Quarantine | Action::Hide));
    g.adult |= class.content_type == ContentType::Adult || fix == Some(true);
    if let Err(why) = eligible(&g) {
        return Ok(Err(why));
    }
    let Some(kind) = media_kind(class.content_type).filter(|k| ctx.kinds.contains(k)) else {
        return Ok(Err(Ineligible::NoQuery));
    };
    // The torrent's name alone, never with its files: with no files, the
    // parse can't take a title from a file or folder name (Q5: never the
    // files). A name that says nothing useful sends nothing.
    let named = classify_corrected(&row.name, &[], Correction::default());
    if named.title.ambiguous || named.title.resolved.is_some() {
        return Ok(Err(Ineligible::NoQuery));
    }
    let release = &named.title.release;
    let year = release.year.map(|y| y.first);
    Ok(Query::new(&release.title, year, kind)
        .map(|query| Candidate {
            id,
            key: row.info_hash,
            query,
        })
        .ok_or(Ineligible::NoQuery))
}

/// The runtime's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Lookups a UTC day, opened and background together (Q12); never
    /// above [`PER_DAY`].
    pub per_day: u32,
    /// The least time between two lookups; never below the plugin's own
    /// interval.
    pub interval: Duration,
    /// Stored torrents one background page reads.
    pub page: usize,
    /// The rest after a full background round, in seconds.
    pub rest_secs: i64,
    /// Run the background backfill. Off: opened items only.
    pub background: bool,
    /// The wait before trying again when there's nothing to do, or nothing
    /// may be looked up.
    pub idle: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            per_day: PER_DAY,
            interval: Duration::ZERO,
            page: PAGE,
            rest_secs: ROUND_REST_SECS,
            background: true,
            idle: IDLE,
        }
    }
}

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct IdentifyStats {
    /// Items handed to the plugin.
    pub looked_up: AtomicU64,
    pub matched: AtomicU64,
    pub missed: AtomicU64,
    /// Lookups that failed (the plugin's error).
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

/// [`IdentifySource::new`] refused a plugin: its manifest isn't an
/// enrichment source, or says it's always adult.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAdmitted;

/// A manifest whose source may run here: an enrichment source that isn't
/// always adult (its answers aren't finds, so an adult mask on it would
/// never apply, and Adult never goes through this path).
pub fn admitted(m: &Manifest) -> bool {
    m.is(lodestar_sources::Kind::Enrichment) && !m.always_adult
}

/// A general enrichment plugin, run as a source of its own: the pipeline
/// registers, enables and stops it like any other, gives it its kit (the
/// client behind its egress and kill switch, and its `source_state`) and
/// its emitter, under its own source id.
pub struct IdentifySource {
    plugin: Arc<dyn Identifier>,
    store: Arc<Mutex<Store>>,
    filter: Arc<Handle>,
    adult: AdultSources,
    switches: Arc<dyn Switches>,
    interest: tokio::sync::Mutex<mpsc::Receiver<i64>>,
    config: Config,
    stats: Arc<IdentifyStats>,
}

impl std::fmt::Debug for IdentifySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentifySource")
            .field("plugin", &self.plugin.manifest().id)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl IdentifySource {
    /// `store` is a connection of the runtime's own (it only reads, apart
    /// from its `source_state`); `filter` is the live filter; `adult` the
    /// registered always-adult sources.
    pub fn new(
        plugin: Arc<dyn Identifier>,
        store: Arc<Mutex<Store>>,
        filter: Arc<Handle>,
        adult: AdultSources,
        switches: Arc<dyn Switches>,
        config: Config,
    ) -> Result<(IdentifySource, Interest), NotAdmitted> {
        if !admitted(plugin.manifest()) {
            return Err(NotAdmitted);
        }
        let (tx, rx) = mpsc::channel(INTEREST_QUEUE);
        let interval = config
            .interval
            .max(plugin.min_interval())
            .max(MIN_INTERVAL_FLOOR);
        let source = IdentifySource {
            plugin,
            store,
            filter,
            adult,
            switches,
            interest: tokio::sync::Mutex::new(rx),
            config: Config {
                // Never above the runner's cap, whatever the host sets.
                per_day: config.per_day.min(PER_DAY),
                interval,
                // No busy loop, whatever the host sets.
                idle: config.idle.max(MIN_IDLE),
                ..config
            },
            stats: Arc::default(),
        };
        Ok((source, Interest(tx)))
    }

    pub fn stats(&self) -> Arc<IdentifyStats> {
        self.stats.clone()
    }

    /// The pacing the runtime uses, after the plugin's own interval and
    /// the floor.
    pub fn config(&self) -> Config {
        self.config
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
                    Some(id) => (id, false),
                    None => tokio::select! {
                        biased;
                        _ = ctx.shutdown.requested() => return Ok(()),
                        Some(id) = interest.recv() => (id, true),
                        _ = tokio::time::sleep(self.config.idle) => continue,
                    },
                },
            };
            match self.one(&mut ctx, &kit, &*state, id).await {
                Step::Asked => last = Some(Instant::now()),
                Step::Skipped if !user => tokio::select! {
                    biased;
                    _ = ctx.shutdown.requested() => return Ok(()),
                    _ = tokio::time::sleep(SKIP_PAUSE) => {}
                },
                Step::Skipped => {}
                step @ (Step::Wait | Step::Spent) => {
                    // Nothing can go now. A background item goes back to
                    // the front of the queue, never past the cursor. An
                    // opened item waits first in line only for the day's
                    // count, and is checked against every condition again
                    // when it's tried; one opened while a plugin-wide
                    // condition fails, or while the store can't answer, is
                    // dropped, never sent later.
                    if !user {
                        background.queue.push_front(id);
                    } else if matches!(step, Step::Spent) {
                        waiting = Some(id);
                    }
                    bump(&self.stats.done);
                    tokio::select! {
                        biased;
                        _ = ctx.shutdown.requested() => return Ok(()),
                        Some(id) = interest.recv(), if waiting.is_none() => waiting = Some(id),
                        _ = tokio::time::sleep(self.config.idle) => {}
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

    /// The next background id to try, reading the next page when the
    /// queue is empty. `None` while there's none: the background is off,
    /// the day's count is spent, the round is resting, or the store can't
    /// be read.
    async fn next_background(
        &self,
        state: &dyn PluginState,
        bg: &mut Background,
        now: i64,
    ) -> Option<i64> {
        if let Some(id) = bg.queue.pop_front() {
            return Some(id);
        }
        if !self.config.background || now < bg.resume_at {
            return None;
        }
        if Budget::load(state, now)
            .await
            .is_none_or(|b| b.left(now, self.config.per_day) == 0)
        {
            return None;
        }
        let store = self.store.clone();
        let (after, page) = (bg.after, self.config.page.clamp(1, PAGE));
        let read = tokio::task::spawn_blocking(move || {
            let store = store.lock().unwrap_or_else(|e| e.into_inner());
            store.ids_after(after, page)
        })
        .await;
        let Ok(Ok(ids)) = read else {
            return None;
        };
        match ids.last() {
            Some(&last) => bg.after = last,
            None => {
                bg.after = 0;
                bg.resume_at = now.saturating_add(self.config.rest_secs);
                bg.save(state, 0).await;
            }
        }
        bg.queue.extend(ids);
        bg.queue.pop_front()
    }

    /// One item: the cache, the gate, the day's count, then the plugin.
    async fn one(&self, ctx: &mut SourceCtx, kit: &Kit, state: &dyn PluginState, id: i64) -> Step {
        let now = crate::unix_now();
        if Cache::fresh(state, id, now).await {
            bump(&self.stats.cached);
            return Step::Skipped;
        }
        let candidate = {
            let store = self.store.clone();
            let engine = self.filter.current();
            let manifest = self.plugin.manifest().clone();
            let kinds = self.plugin.kinds().to_vec();
            let switches = self.switches.clone();
            let (source, adult) = (ctx.source, self.adult);
            tokio::task::spawn_blocking(move || {
                let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
                let ctx = GateCtx {
                    manifest: &manifest,
                    source,
                    kinds: &kinds,
                    adult,
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
                return if why.plugin_wide() {
                    Step::Wait
                } else {
                    Step::Skipped
                };
            }
            // The store couldn't say: never asked, and tried again later.
            _ => {
                bump(&self.stats.ineligible);
                return Step::Wait;
            }
        };
        // One cap for both (Q12, stricter than "after user interest"): an
        // opened item jumps the queue, within the same day's count.
        if !Budget::take(state, now, self.config.per_day).await {
            return Step::Spent;
        }
        bump(&self.stats.looked_up);
        let lookup = tokio::select! {
            biased;
            _ = ctx.shutdown.requested() => return Step::Shutdown,
            r = self.plugin.lookup(kit, &candidate.query) => r,
        };
        match lookup {
            Ok(Lookup::Match(answer)) => {
                let found = Found::key(candidate.key, STRATEGY).with_claims(answer.into_claims());
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

enum Step {
    /// The plugin was called: the next lookup waits the interval.
    Asked,
    /// This item isn't looked up.
    Skipped,
    /// Nothing may be looked up now (a plugin-wide condition, or the store
    /// can't answer): wait. An opened item is dropped.
    Wait,
    /// The day's count is spent, or can't be written (fail closed): wait,
    /// keeping the item's place, an opened one included.
    Spent,
    Shutdown,
}

impl SourcePlugin for IdentifySource {
    fn manifest(&self) -> &Manifest {
        self.plugin.manifest()
    }

    fn policy(&self) -> &Policy {
        self.plugin.policy()
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(self.run_inner(ctx))
    }
}

/// The plugin's view of its `source_state`: everything but the runtime's
/// own keys under [`RUNTIME_PREFIX`], which it can neither read nor
/// write, so a plugin can't reset the counts or caches.
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

/// The key of the background cursor in the plugin's `source_state`.
pub const CURSOR_KEY: &str = "enr10/cursor";
/// The key of the day's counts.
pub const BUDGET_KEY: &str = "enr10/budget";

fn i64_at(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The background cursor, kept in `source_state` so a restart goes on
/// where it stopped.
#[derive(Debug, Default)]
struct Background {
    after: i64,
    resume_at: i64,
    queue: VecDeque<i64>,
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

/// The day's lookups (Q12), counted per UTC day: background and opened
/// items alike, against one cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Budget {
    day: i64,
    used: u32,
}

impl Budget {
    /// The stored count, normalised to `now`'s UTC day. `None` when it
    /// can't be read: a count that can't be read is spent (fail closed). A
    /// stored day later than today (the clock stepped back) becomes today
    /// with its count, and is written back at once, so a count spent on a
    /// future day is never held until that day (as ENR-1's, #502 review).
    async fn load(state: &dyn PluginState, now: i64) -> Option<Budget> {
        let v = state.get(BUDGET_KEY).await.ok()?.unwrap_or_default();
        let today = now.div_euclid(86_400);
        let b = Budget {
            day: i64_at(&v, 0).unwrap_or(i64::MIN),
            used: u32_at(&v, 8).unwrap_or(0),
        };
        if b.day <= today {
            return Some(b);
        }
        let b = Budget { day: today, ..b };
        let _ = state.set(BUDGET_KEY, b.encode()).await;
        Some(b)
    }

    fn encode(&self) -> Vec<u8> {
        let mut v = self.day.to_be_bytes().to_vec();
        v.extend(self.used.to_be_bytes());
        v
    }

    fn left(&self, now: i64, per_day: u32) -> u32 {
        if self.day == now.div_euclid(86_400) {
            per_day.saturating_sub(self.used)
        } else {
            per_day
        }
    }

    /// Takes one lookup from today's count. False when it's spent, or the
    /// count can't be read or written: then the lookup isn't made.
    async fn take(state: &dyn PluginState, now: i64, per_day: u32) -> bool {
        let Some(b) = Budget::load(state, now).await else {
            return false;
        };
        if b.left(now, per_day) == 0 {
            return false;
        }
        let day = now.div_euclid(86_400);
        let used = if b.day == day { b.used + 1 } else { 1 };
        state
            .set(BUDGET_KEY, Budget { day, used }.encode())
            .await
            .is_ok()
    }
}

/// Hits and misses, in [`BUCKETS`] `source_state` values keyed by torrent
/// id: each entry is the id and when it expires. A full bucket drops the
/// entries that expire soonest; an item dropped early is only asked again
/// sooner, within the day's cap.
struct Cache;

/// Cache buckets: a quarter of a plugin's 1,024 state keys, about a
/// million entries.
pub const BUCKETS: i64 = 256;
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

    /// Caches `id` until `expires`. Best effort.
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

/// The identification stored on a torrent that resolves its title, of
/// its claims (`torrent_claims`): the surest one with a canonical title
/// and a match confidence, ties to the lower source id.
///
/// **Only from a source that didn't find the torrent** (its bit isn't in
/// `finders`, the torrent's `sources`). `unverified::ingest` stores an
/// identification through the match path, which never sets the plugin's
/// bit, while every finder's claims come with its bit (`put_unverified`).
/// So a feed that claims a canonical title for a torrent it found can't
/// rename it. Private claims never count. Each plugin's answer is its own
/// row, so the others are kept as alternatives; only this one names the
/// title.
pub fn stored_title(
    store: &Store,
    id: i64,
    finders: u64,
) -> Result<Option<(String, Option<u16>, u8)>, lodestar_store::Error> {
    let mut best: Option<(String, Option<u16>, u8)> = None;
    for c in store.torrent_claims(id)? {
        let bit = 1u64.checked_shl(u32::from(c.source.get())).unwrap_or(0);
        if finders & bit != 0 || c.private_source {
            continue;
        }
        let Some(d) = c.claims.as_ref().and_then(|c| c.details.as_deref()) else {
            continue;
        };
        let (Some(title), Some(conf)) = (&d.canonical_title, d.match_confidence) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| conf > b.2) {
            best = Some((title.clone(), d.year, conf));
        }
    }
    Ok(best)
}

/// `enriched` (the record classification `class` gave the torrent `row`)
/// with its title resolved via a plugin ([`via_plugin`]), when a stored
/// identification ([`stored_title`]) resolves it. Only the title's part
/// of the record changes; the type, kind and junk score stay what
/// classification made them. `enriched` comes back unchanged otherwise,
/// and when its record doesn't decode.
pub fn with_identified(
    store: &Store,
    row: &lodestar_store::TorrentRow,
    class: &Classification,
    enriched: Enriched,
) -> Result<Enriched, lodestar_store::Error> {
    let Some((title, year, confidence)) = stored_title(store, row.id, row.sources)? else {
        return Ok(enriched);
    };
    Ok(resolved(class, enriched, &title, year, confidence))
}

/// [`with_identified`]'s rewrite, from the answer itself.
pub fn resolved(
    class: &Classification,
    enriched: Enriched,
    title: &str,
    year: Option<u16>,
    confidence: u8,
) -> Enriched {
    let p = PluginTitle {
        title,
        year,
        confidence,
    };
    let Some(effective) = via_plugin(&class.title, p) else {
        return enriched;
    };
    let Ok(mut record) = Record::decode(&enriched.data) else {
        return enriched;
    };
    let by_plugin = Record::new(&Classification {
        title: effective,
        ..class.clone()
    });
    record.release = by_plugin.release;
    record.ambiguous = by_plugin.ambiguous;
    record.resolved = by_plugin.resolved;
    record.title_confidence = by_plugin.title_confidence;
    Enriched {
        data: record.encode(),
        ..enriched
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_sources::MemoryState;
    use proptest::prelude::*;

    fn gate() -> impl Strategy<Value = Gate> {
        proptest::collection::vec(any::<bool>(), 9).prop_map(|b| Gate {
            kill_switch_tripped: b[0],
            source_enabled: b[1],
            consent_met: b[2],
            stored: b[3],
            held: b[4],
            filtered: b[5],
            private_source: b[6],
            adult: b[7],
            pending: b[8],
        })
    }

    proptest! {
        /// Eligible exactly when every condition holds.
        #[test]
        fn eligible_only_when_every_condition_holds(g in gate()) {
            let all = !g.kill_switch_tripped
                && g.source_enabled
                && g.consent_met
                && g.stored
                && !g.held
                && !g.filtered
                && !g.private_source
                && !g.adult
                && !g.pending;
            prop_assert_eq!(eligible(&g).is_ok(), all);
        }
    }

    #[test]
    fn adult_and_unnamed_kinds_are_never_looked_up() {
        assert_eq!(media_kind(ContentType::Adult), None);
        assert_eq!(media_kind(ContentType::Video), None);
        assert_eq!(media_kind(ContentType::Movie), Some(MediaKind::Movie));
        let served: Vec<_> = ContentType::ALL
            .iter()
            .filter_map(|&t| media_kind(t))
            .collect();
        assert_eq!(served, MediaKind::ALL);
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
    }

    #[tokio::test]
    async fn the_count_resets_each_day() {
        let s = MemoryState::new();
        let day = 20_000 * 86_400;
        assert!(Budget::take(&s, day, 2).await);
        assert!(Budget::take(&s, day + 5, 2).await);
        assert!(!Budget::take(&s, day + 10, 2).await);
        assert!(Budget::take(&s, day + 86_400, 2).await);
        assert_eq!(Budget::load(&s, day + 86_400).await.unwrap().used, 1);
    }

    #[tokio::test]
    async fn a_future_day_never_holds_the_count() {
        let s = MemoryState::new();
        let today = 20_000 * 86_400;
        let future = today + 3_000 * 86_400;
        assert!(Budget::take(&s, future, 1).await);
        assert!(!Budget::take(&s, today + 10, 1).await);
        let next = today + 86_400;
        assert_eq!(Budget::load(&s, next).await.unwrap().left(next, 1), 1);
    }

    #[tokio::test]
    async fn an_unreadable_count_is_spent() {
        #[derive(Debug)]
        struct Broken;
        impl PluginState for Broken {
            fn get<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, StateError>> {
                Box::pin(async { Err(StateError::Store("down".into())) })
            }
            fn set<'a>(&'a self, _: &'a str, _: Vec<u8>) -> BoxFuture<'a, Result<(), StateError>> {
                Box::pin(async { Ok(()) })
            }
            fn remove<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), StateError>> {
                Box::pin(async { Ok(()) })
            }
            fn compare_and_set<'a>(
                &'a self,
                _: &'a str,
                _: Option<&'a [u8]>,
                _: Option<Vec<u8>>,
            ) -> BoxFuture<'a, Result<bool, StateError>> {
                Box::pin(async { Ok(true) })
            }
        }
        assert!(Budget::load(&Broken, 0).await.is_none());
        assert!(!Budget::take(&Broken, 0, 5).await);
    }

    #[tokio::test]
    async fn the_plugin_cannot_touch_the_runtime_keys() {
        let inner = Arc::new(MemoryState::new());
        let g = Guarded(inner.clone());
        assert_eq!(g.get(BUDGET_KEY).await, Err(StateError::BadKey));
        assert_eq!(g.set(CURSOR_KEY, vec![]).await, Err(StateError::BadKey));
        assert_eq!(g.remove(&Cache::key(1)).await, Err(StateError::BadKey));
        assert_eq!(
            g.compare_and_set(BUDGET_KEY, None, None).await,
            Err(StateError::BadKey)
        );
        g.set("own", b"x".to_vec()).await.unwrap();
        assert_eq!(inner.get("own").await, Ok(Some(b"x".to_vec())));
    }
}
