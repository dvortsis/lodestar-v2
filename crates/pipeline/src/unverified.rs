//! The unverified pipeline (architecture §18.5, §20.1, §20.5; plan
//! `docs/plan/2026-11-01-v06-sources.md` SRC-2;
//! `docs/decisions/2026-11-02-SRC-2.md`): what the writer does with a
//! source's claims before and after they're stored.
//!
//! - **Filter before storage.** Every claimed name and file path passes
//!   the full filter pipeline ([`judge`]: the locked floor, the core, your
//!   rules, packs and lists, with your classification rules) before
//!   `put_unverified` or `put_claims`. The store's held-hash guard is the
//!   second line, not the only one.
//! - **A claim on the floor or a CS pattern always carries a hold**
//!   (tombstone or quarantine, as §20 says for that rule), even when the
//!   rule's own action is weaker: such a claim is never stored, promoted
//!   or left only flagged. Any other hold (your drop or quarantine rule, a
//!   pack, a list) holds the hash of a key with no verified metadata, but
//!   never a stored torrent: claims are hints, and they never override
//!   verified metadata (§18.5). That claim is dropped instead.
//! - **Re-judging after storage** ([`Sweep`], [`refile_step`]): stored
//!   unverified items and torrent claims are judged again at start and
//!   whenever the filter engine's version changes, one bounded page per
//!   step through SRC-1's keyset pages, each written in its own short
//!   transaction that the store refuses when the rules moved on.
//! - **Verification** ([`Verify`]): behind `sources.verify_unverified`
//!   (default off), a cursor over the stored items hands a few keys per
//!   flush to intake, which fetches them through the shared pool. The
//!   store is the queue, so it holds nothing in memory.
//! - **Promotion** runs in the writer's commit (`writer::transaction`):
//!   any verified torrent whose hash an unverified item holds promotes it,
//!   keeping its id, whichever path fetched it.
//!
//! Nothing here logs or returns a claim's text: holds carry rule ids,
//! evidence codes and sealed names only.

use std::sync::atomic::{AtomicU64, Ordering};

use lodestar_classify::{Correction, FileInfo, Record, classify_corrected};
use lodestar_filter::reclassify::{adult_hold, judge_with_rules};
use lodestar_filter::{Action, Engine, Facts, Handle, Item, RuleRef, Sealer};
use lodestar_sources::Discovery;
use lodestar_store::claims::{self, Claims};
use lodestar_store::unverified::{
    MAX_PAGE, Network, NewUnverified, Put, RefiledClaims, RefiledUnverified, UnverifiedItem,
};
use lodestar_store::{Filtered, Held, Prepared, SearchableCodes, SourceId, Store};

use crate::source_adult::AdultSources;
use crate::writer::Backoff;

/// The codes a promotion is counted under as newly searchable: Insights'
/// own (`lodestar::insights::CODES`, which this crate can't name).
pub const SEARCHABLE: SearchableCodes = SearchableCodes {
    stage: lodestar_sources::Stage::Searchable as u8,
    newly: crate::rollups::metric::NEWLY_SEARCHABLE,
    delay: crate::rollups::metric::TIME_TO_SEARCHABLE,
};

/// Rows one refile step judges: one store page at most.
pub const REFILE_PAGE: usize = MAX_PAGE;

/// Keys the verification cursor hands to intake per flush (every 5 s by
/// default, so about 13 a second). The crawler commits thousands a
/// minute, so verification takes a few percent of the writer at most
/// (§18.4: importers and the like stay well under half its share).
pub const VERIFY_PER_FLUSH: usize = 64;

/// After the cursor reaches the last item, it starts over this much later
/// (seconds): a dead swarm is tried again a few times a day, not in a loop.
pub const VERIFY_ROUND_SECS: i64 = 6 * 3600;

/// The pipeline's own strategy code for a verification fetch, beside the
/// DHT's (`dht::strategy`).
pub const VERIFY_STRATEGY: u8 = 201;

/// What intake may hold in flight to the writer, in KiB of claims
/// ([`claims_kib`]): a byte bound on the claims channel, since one
/// discovery's claims run to a megabyte and feeds are untrusted.
pub const CLAIMS_BUDGET_KIB: u32 = 64 * 1024;

/// Claims one writer step takes at most, in KiB: a step's judging and
/// storing stay short beside the crawler's commits however large the
/// claims.
pub const CLAIMS_STEP_KIB: u32 = 8 * 1024;

/// A discovery with claims on its way to the writer, holding its share
/// of [`CLAIMS_BUDGET_KIB`] until the writer has stored or dropped it.
pub type Claimed = (Discovery, tokio::sync::OwnedSemaphorePermit);

/// A discovery's claims in KiB, counting each file's entry as well as its
/// text (an empty path still takes memory), at least 1 and at most
/// [`CLAIMS_STEP_KIB`].
pub fn claims_kib(d: &Discovery) -> u32 {
    let Some(c) = d.found.claims.as_deref() else {
        return 1;
    };
    let bytes = c.text_len()
        + c.files.len() * std::mem::size_of::<lodestar_sources::ClaimedFile>()
        + c.trackers.len() * std::mem::size_of::<String>()
        + c.ids.len() * std::mem::size_of::<lodestar_sources::ExternalId>();
    u32::try_from(bytes / 1024 + 1)
        .unwrap_or(u32::MAX)
        .min(CLAIMS_STEP_KIB)
}

/// Where the writer publishes the ids it promoted under their own id,
/// after each commit (SRC-2b's percolator pickup hooks in here). The sink
/// runs on the writer's task and must not block: hand the ids off and
/// return. Nothing is published while no sink is set.
#[derive(Default)]
pub struct PromotedHook(std::sync::RwLock<Option<PromotedSink>>);

/// A [`PromotedHook`]'s sink.
type PromotedSink = Box<dyn Fn(&[i64]) + Send + Sync>;

impl std::fmt::Debug for PromotedHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = self.0.read().map(|s| s.is_some()).unwrap_or(false);
        f.debug_struct("PromotedHook").field("set", &set).finish()
    }
}

impl PromotedHook {
    /// Sets the sink, replacing any earlier one.
    pub fn set(&self, sink: impl Fn(&[i64]) + Send + Sync + 'static) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(sink));
    }

    pub(crate) fn publish(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        if let Some(sink) = self.0.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            sink(ids);
        }
    }
}

/// Counters for metrics and the status page.
#[derive(Debug, Default)]
pub struct UnverifiedStats {
    /// Discoveries with claims the writer judged.
    pub received: AtomicU64,
    /// New unverified items stored.
    pub stored: AtomicU64,
    /// Items that already existed and gained the source.
    pub existing: AtomicU64,
    /// Claims stored on a torrent that already held the key.
    pub on_torrent: AtomicU64,
    /// Keys whose claims a rule held: tombstoned or quarantined, never
    /// stored.
    pub held: AtomicU64,
    /// Claims a rule matched on a stored torrent, dropped without holding
    /// it (not a floor or core hit: claims never override verified
    /// metadata).
    pub dropped: AtomicU64,
    /// Discoveries the store refused: the key is held, or the source is
    /// disabled or purged.
    pub refused: AtomicU64,
    /// Claims outside the limits (the emitter clamps them, so none).
    pub invalid: AtomicU64,
    /// Unverified items promoted under their own id.
    pub promoted: AtomicU64,
    /// Unverified items merged into a torrent stored under the hash.
    pub merged: AtomicU64,
    /// Promotions whose claimed file list contradicts the verified one.
    pub mismatches: AtomicU64,
    /// Stored items and claims the refile sweep removed.
    pub refiled: AtomicU64,
    /// Refile sweeps finished.
    pub sweeps: AtomicU64,
    /// Keys the verification cursor handed to intake.
    pub verify_sent: AtomicU64,
    /// The promoted ids, published after each commit.
    pub promoted_hook: PromotedHook,
}

impl UnverifiedStats {
    fn add(counter: &AtomicU64, n: usize) {
        counter.fetch_add(n as u64, Ordering::Relaxed);
    }
}

/// Whether a claimed tracker is a plausible tracker URL: an `http`,
/// `https`, `udp` or `wss` scheme, a host, at most the claims' tracker
/// limit, and no whitespace or control characters. Tracker URLs aren't
/// names, so they aren't judged as text (runner, #383); anything else a
/// feed puts there is dropped before storage.
pub fn tracker_url(t: &str) -> bool {
    let Some((scheme, rest)) = t.split_once("://") else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "udp" | "wss"
    ) && t.len() <= claims::MAX_TRACKER
        && !rest.is_empty()
        && !rest.starts_with('/')
        && !t.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The store's claims for a discovery: the source's claims and
/// provenance, already clamped by the emitter. `None` when it has none.
pub fn store_claims(d: &Discovery) -> Option<Claims> {
    let c = d.found.claims.as_deref()?;
    let p = d.found.provenance.as_deref();
    Some(Claims {
        name: c.name.clone(),
        size: c.size,
        files: c
            .files
            .iter()
            .map(|f| claims::ClaimedFile {
                path: f.path.clone(),
                size: f.size,
            })
            .collect(),
        trackers: c
            .trackers
            .iter()
            .filter(|t| tracker_url(t))
            .cloned()
            .collect(),
        ids: c
            .ids
            .iter()
            .map(|i| claims::ExternalId {
                scheme: i.scheme.clone(),
                value: i.value.clone(),
            })
            .collect(),
        description: c.description.clone(),
        publisher: c.publisher.clone(),
        provenance: claims::Provenance {
            external_ref: p.and_then(|p| p.external_ref.clone()),
            published_at: p.and_then(|p| p.published_at),
            confidence: match p.map(|p| p.confidence) {
                Some(lodestar_sources::Confidence::High) => claims::Confidence::High,
                Some(lodestar_sources::Confidence::Medium) => claims::Confidence::Medium,
                _ => claims::Confidence::Low,
            },
        },
        // Not stored until ENR-1 judges the section's text first (plan Q3).
        adult: None,
    })
}

/// A claim a rule holds back. No name in the clear: only the sealed one,
/// for quarantine.
#[derive(Clone)]
pub struct Hold {
    pub hashes: Vec<[u8; 20]>,
    pub rule: String,
    pub action: u8,
    pub held: Held,
    pub sealed: Option<Vec<u8>>,
    pub evidence: Vec<u8>,
    pub adult_hold: bool,
    /// A floor or core (CS) rule decided: the hash is held even when a
    /// torrent is stored under it.
    pub core: bool,
}

// No name, sealed or not: it may be one a rule holds.
impl std::fmt::Debug for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hold")
            .field("rule", &self.rule)
            .field("held", &self.held)
            .field("core", &self.core)
            .finish_non_exhaustive()
    }
}

impl Hold {
    /// The decision as ingest writes one: a filter hit and the hold.
    pub fn filtered(&self, now: i64, quarantine_secs: i64) -> Filtered<'_> {
        Filtered {
            hashes: &self.hashes,
            rule: &self.rule,
            action: self.action,
            held: Some(self.held),
            sealed: self.sealed.as_deref(),
            evidence: &self.evidence,
            at: now,
            expires_at: now.saturating_add(quarantine_secs),
            adult_hold: self.adult_hold,
        }
    }
}

/// A claimed path as the classifier and filter take it: its segments,
/// split on either separator. Never empty.
fn segments(path: &str) -> Vec<String> {
    let mut out: Vec<String> = path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if out.is_empty() {
        out.push(path.to_owned());
    }
    out
}

/// The claimed files as the classifier and filter take them. A claimed
/// size the source left out counts as 0.
pub fn claimed_files(c: &Claims) -> Vec<FileInfo> {
    c.files
        .iter()
        .map(|f| FileInfo::new(segments(&f.path), f.size.unwrap_or(0)))
        .collect()
}

/// Judges one key's claims through the full filter pipeline, as ingest
/// judges a fetched torrent: its hash, then the claimed name and every
/// claimed file path together, classified as they claim, with your
/// classification rules; then the claimed description and the publisher,
/// each judged as a name the same way (runner, #383: every claim text that
/// is stored passes the filter). Tracker URLs aren't names: they're
/// validated as URLs instead ([`store_claims`]).
///
/// `Some` when the claims must not be stored:
/// - a floor or core (CS) rule matched any of the texts: [`Hold::core`],
///   a hold always, a quarantine when the rule's own action is weaker
///   than one (the rule for SRC-2 in `2026-11-02-SRC-1.md`). A core hit
///   on any text wins over another rule's hit on another;
/// - any other rule that drops or quarantines: not [`Hold::core`]. The
///   callers drop the source's claims and hold nothing (runner, #383): a
///   feed must not be able to get an unrelated torrent refused by
///   claiming its hash under a name your rule drops.
///
/// Weaker decisions (flag, hide, demote) and shadow-mode rules don't stop
/// storage: an unverified item isn't searchable, and its verified metadata
/// is judged again when it's promoted.
///
/// `adult`: an always-adult source made these claims, or found the key
/// ([`crate::source_adult`]). Each text is then judged as adult as well,
/// as an Is adult correction makes it, and the stricter verdict stands:
/// the Adult-only core (CS-5a and the rest) checks it whatever its name.
pub fn judge(
    engine: &Engine,
    key: [u8; 20],
    c: &Claims,
    sealer: Option<&Sealer>,
    adult: bool,
    now: i64,
) -> Option<Hold> {
    let hashes = vec![key];
    let files = claimed_files(c);
    let size = c
        .size
        .unwrap_or_else(|| files.iter().map(|f| f.size).fold(0, u64::saturating_add));
    let mut found: Option<Hold> = None;
    let texts = [
        (c.name.as_deref().unwrap_or(""), true),
        (c.description.as_deref().unwrap_or(""), false),
        (c.publisher.as_deref().unwrap_or(""), false),
    ];
    for (i, (text, with_files)) in texts.into_iter().enumerate() {
        // The name is judged even when empty: the files and the hash.
        if i > 0 && text.is_empty() {
            continue;
        }
        let files: &[FileInfo] = if with_files { &files } else { &[] };
        let Some(h) = judge_text(engine, &hashes, text, files, size, sealer, adult, now) else {
            continue;
        };
        if h.core {
            return Some(h);
        }
        found.get_or_insert(h);
    }
    found
}

/// [`judge`] for one text, as a torrent's name, with `files`.
#[allow(clippy::too_many_arguments)]
fn judge_text(
    engine: &Engine,
    hashes: &[[u8; 20]],
    name: &str,
    files: &[FileInfo],
    size: u64,
    sealer: Option<&Sealer>,
    adult: bool,
    now: i64,
) -> Option<Hold> {
    let class = classify_corrected(name, files, Correction::default());
    let facts = Facts {
        class: &class,
        size,
        piece_len: 0,
        flags: 0,
        discovered_at: now,
        now,
    };
    let item = Item {
        hashes,
        name,
        files,
        content_type: class.content_type,
    };
    let (verdict, fix) = if adult {
        // A hash on the floor's or a dropping list first, as
        // `judge_with_rules` does; then both readings.
        match engine.blocked_hash(hashes) {
            Some(d) => (
                lodestar_filter::Verdict {
                    decision: Some(d),
                    shadow: Vec::new(),
                },
                None,
            ),
            None => crate::source_adult::judge(engine, &item, &facts, now),
        }
    } else {
        judge_with_rules(engine, &item, &class, &facts, now)
    };
    let d = verdict.decision?;
    let core = matches!(d.rule, RuleRef::Core(_));
    let action = match d.action {
        Action::Drop => Action::Drop,
        Action::Quarantine => Action::Quarantine,
        _ if core => Action::Quarantine,
        _ => return None,
    };
    let held = if action == Action::Drop {
        Held::Tombstone
    } else {
        Held::Quarantine
    };
    let sealed = (core && held == Held::Quarantine)
        .then(|| sealer.and_then(|s| s.seal(name).ok()))
        .flatten();
    Some(Hold {
        hashes: hashes.to_vec(),
        rule: d.rule.id(),
        action: action.code(),
        held,
        sealed,
        evidence: d.evidence_codes(),
        // Held by the Adult-only core only because your rule (or its
        // source) made it adult: a tombstone when the quarantine ends
        // (§20.9).
        adult_hold: fix == Some(true) && adult_hold(&d),
        core,
    })
}

/// What the writer needs to judge and hold.
#[derive(Clone, Copy, Default)]
pub struct Ctx<'a> {
    pub sealer: Option<&'a Sealer>,
    pub quarantine_secs: i64,
    /// The sources whose every find is adult ([`crate::source_adult`]).
    pub adult: AdultSources,
}

impl std::fmt::Debug for Ctx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("sealer", &self.sealer.is_some())
            .field("quarantine_secs", &self.quarantine_secs)
            .field("adult", &self.adult)
            .finish()
    }
}

/// What one ingest batch did, for the writer to account.
#[derive(Debug, Default)]
pub struct Ingested {
    /// Torrents stored under a key the item's own claims now hold (crawled
    /// before the claims arrived): deleted, and logged for the index.
    pub deleted: Vec<i64>,
    /// Stored torrents that gained claims: `(id, at, source, strategy)`.
    /// The writer accounts each as a rediscovery, as a bare sighting of a
    /// stored key is.
    pub touched: Vec<(i64, i64, SourceId, u8)>,
    /// Stored torrents an always-adult source has just claimed: the writer
    /// classifies them as adult (`enrich::adult_by_source`).
    pub adult: Vec<i64>,
}

/// Judges a batch of discoveries with claims and stores what the rules
/// let through: holds first (calling `mark` with their hashes just before
/// the write, `2026-10-31-B-held-precommit.md`), then the items, one
/// `put_unverified` per source so a source disabled meanwhile is refused
/// alone.
pub fn ingest(
    store: &mut Store,
    engine: &Engine,
    ctx: Ctx<'_>,
    found: &[Discovery],
    stats: &UnverifiedStats,
    now: i64,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Ingested, lodestar_store::Error> {
    let mut out = Ingested::default();
    UnverifiedStats::add(&stats.received, found.len());
    let mut keep: Vec<(&Discovery, Claims)> = Vec::with_capacity(found.len());
    let mut holds: Vec<Hold> = Vec::new();
    for d in found {
        let Some(c) = store_claims(d) else { continue };
        if c.check().is_err() {
            UnverifiedStats::add(&stats.invalid, 1);
            continue;
        }
        let adult = ctx.adult.has(d.source);
        let Some(mut h) = judge(engine, d.found.key.0, &c, ctx.sealer, adult, now) else {
            keep.push((d, c));
            continue;
        };
        if !h.core {
            // Your rule, a pack or a list: this source's claims are
            // dropped and nothing is held (runner, #383). A torrent under
            // the hash is judged on its own verified metadata, whichever
            // arrives first.
            drop_source_claims(store, d.found.key.0, d.source)?;
            UnverifiedStats::add(&stats.dropped, 1);
            continue;
        }
        // The floor or the core on a stored torrent: every one of its
        // swarm hashes is held (a hybrid's other key too), as the refile
        // does.
        if let Some(id) = store.find(&d.found.key)?
            && let Some(all) = store.torrent_swarm_hashes(id)?
        {
            for x in all {
                if !h.hashes.contains(&x) {
                    h.hashes.push(x);
                }
            }
        }
        holds.push(h);
    }
    if !holds.is_empty() {
        let hashes: Vec<[u8; 20]> = holds.iter().flat_map(|h| h.hashes.clone()).collect();
        mark(store, &hashes);
        let filtered: Vec<Filtered<'_>> = holds
            .iter()
            .map(|h| h.filtered(now, ctx.quarantine_secs))
            .collect();
        // One transaction: the holds, and any torrent stored under their
        // hashes deleted, so a failure can't leave one stored under a held
        // hash. Only floor and core holds reach a stored torrent.
        out.deleted = store.hold_claims(&filtered)?;
        UnverifiedStats::add(&stats.held, holds.len());
    }
    let mut sources: Vec<SourceId> = keep.iter().map(|(d, _)| d.source).collect();
    sources.sort_unstable();
    sources.dedup();
    for source in sources {
        let group: Vec<&(&Discovery, Claims)> =
            keep.iter().filter(|(d, _)| d.source == source).collect();
        let items: Vec<NewUnverified<'_>> = group
            .iter()
            .map(|(d, c)| NewUnverified {
                network: Network::Bt,
                key: &d.found.key.0,
                source,
                private_source: d.found.private_source,
                claims: Some(c),
                at: d.at,
            })
            .collect();
        let put = match store.put_unverified(&items) {
            Ok(put) => put,
            // Disabled or purged meanwhile: what it found is dropped, so a
            // purge can't be undone by a poll in flight (SRC-1).
            Err(lodestar_store::Error::Invalid(_)) => {
                UnverifiedStats::add(&stats.refused, items.len());
                continue;
            }
            Err(e) => return Err(e),
        };
        for ((d, _), p) in group.iter().zip(put) {
            let counter = match p {
                Put::New(_) => &stats.stored,
                Put::Existing(_) => &stats.existing,
                Put::Torrent(id) => {
                    out.touched.push((id, d.at, d.source, d.found.strategy));
                    if ctx.adult.has(d.source) {
                        out.adult.push(id);
                    }
                    &stats.on_torrent
                }
                Put::Held | Put::Bare => &stats.refused,
            };
            UnverifiedStats::add(counter, 1);
        }
    }
    Ok(out)
}

/// Removes what `source` claimed about `key`, holding nothing: the
/// unverified item when its claims are this source's, and this source's
/// claims on a torrent stored under the key. Each removal is SRC-1's
/// refile write with no decision.
fn drop_source_claims(
    store: &mut Store,
    key: [u8; 20],
    source: SourceId,
) -> Result<(), lodestar_store::Error> {
    if let Some(item) = store.unverified(Network::Bt, &key)?
        && item.source == source
    {
        let r = RefiledUnverified {
            id: item.id,
            filtered: None,
        };
        store.refile_unverified(&[r], || true)?;
    }
    if let Some(torrent_id) = store.find(&lodestar_metainfo::InfoHash(key))? {
        let r = RefiledClaims {
            torrent_id,
            source,
            filtered: None,
        };
        store.refile_claims(&[r], || true)?;
    }
    Ok(())
}

/// Which table the refile sweep is on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Items,
    Claims,
}

/// The sweep that judges stored unverified items and torrent claims again
/// whenever the filter engine changes, and once per start (as
/// `CandidateSweep`, `2026-10-31-B-import-refile.md`).
#[derive(Debug, Default)]
pub struct Sweep {
    /// The engine version it runs under; `None` until the first step.
    pub version: Option<u64>,
    pub phase: Phase,
    /// The last item id reached.
    pub after_item: i64,
    /// The last `(torrent_id, source)` of claims reached.
    pub after_claims: (i64, u8),
    /// Nothing is left to judge under `version`.
    pub done: bool,
    /// The wait after a store error.
    pub(crate) backoff: Backoff,
}

impl Sweep {
    /// Has a step to take now.
    pub fn ready(&self) -> bool {
        !self.done && self.backoff.ready()
    }

    /// Starts again from the first item under `version`.
    pub fn restart(&mut self, version: u64) {
        *self = Sweep {
            version: Some(version),
            ..Sweep::default()
        };
    }
}

/// What one refile step did.
#[derive(Debug, Default)]
pub struct Step {
    /// The rules changed under it: nothing was written; start again.
    pub rejected: bool,
    /// The phase it read is finished.
    pub end: bool,
    /// Where the phase goes on from.
    pub after_item: i64,
    pub after_claims: (i64, u8),
    /// Hashes the step held: intake refuses them.
    pub held: Vec<[u8; 20]>,
    /// Items and claims removed.
    pub removed: usize,
    /// Torrents a held decision deleted, for the search index.
    pub deleted: Vec<i64>,
}

/// One step of the refile sweep: judges one page of items (or claims)
/// under the engine at `version` and holds back what newly matches, in
/// one short transaction the store refuses when the rules moved on.
///
/// - **Items:** any hold removes the item and holds its hash (it has no
///   verified metadata). Claims that don't decode are removed: they can't
///   be judged, so they can't be kept.
/// - **Claims on stored torrents:** a floor or core hold removes the
///   claims, holds the hash and deletes the torrent; any other hold only
///   removes the claims, since the torrent's verified metadata is judged
///   on its own (claims never override it).
///
/// Memory is one page: at most [`REFILE_PAGE`] rows and SRC-1's
/// `PAGE_BYTES` (8 MiB) of decoded claims plus one row.
#[allow(clippy::too_many_arguments)]
pub fn refile_step(
    store: &mut Store,
    handle: &Handle,
    version: u64,
    phase: Phase,
    after_item: i64,
    after_claims: (i64, u8),
    ctx: Ctx<'_>,
    now: i64,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Step, lodestar_store::Error> {
    let rejected = Step {
        rejected: true,
        ..Step::default()
    };
    let engine = handle.current();
    if engine.version != version {
        return Ok(rejected);
    }
    let current = || handle.current().version == version;
    match phase {
        Phase::Items => {
            let page = store.unverified_after(after_item, REFILE_PAGE)?;
            let Some(last) = page.last().map(|i| i.id) else {
                return Ok(Step {
                    end: true,
                    after_item,
                    after_claims,
                    ..Step::default()
                });
            };
            let judged: Vec<(i64, Option<Hold>)> = page
                .iter()
                .filter_map(|i| judge_item(&engine, i, ctx, now))
                .collect();
            let refiled: Vec<RefiledUnverified<'_>> = judged
                .iter()
                .map(|(id, h)| RefiledUnverified {
                    id: *id,
                    filtered: h.as_ref().map(|h| h.filtered(now, ctx.quarantine_secs)),
                })
                .collect();
            let held: Vec<[u8; 20]> = judged
                .iter()
                .flat_map(|(_, h)| h.iter().flat_map(|h| h.hashes.iter().copied()))
                .collect();
            let mut step = Step {
                after_item: last,
                after_claims,
                ..Step::default()
            };
            if refiled.is_empty() {
                return Ok(step);
            }
            if !held.is_empty() {
                mark(store, &held);
            }
            let Some(done) = store.refile_unverified(&refiled, current)? else {
                return Ok(rejected);
            };
            step.removed = done.ids.len();
            step.deleted = done.torrents_deleted;
            step.held = held;
            Ok(step)
        }
        Phase::Claims => {
            let source = SourceId::new(after_claims.1).unwrap_or(SourceId::new(0).expect("0"));
            let page = store.claims_after((after_claims.0, source), REFILE_PAGE)?;
            let Some(last) = page.last().map(|c| (c.torrent_id, c.source.get())) else {
                return Ok(Step {
                    end: true,
                    after_item,
                    after_claims,
                    ..Step::default()
                });
            };
            let mut judged: Vec<(i64, SourceId, Option<Hold>)> = Vec::new();
            for c in &page {
                let Some(claims) = &c.claims else {
                    judged.push((c.torrent_id, c.source, None));
                    continue;
                };
                let Some(hashes) = store.torrent_swarm_hashes(c.torrent_id)? else {
                    continue;
                };
                // Claims an always-adult source made: judged as adult too.
                let adult = ctx.adult.has(c.source);
                if let Some(mut h) = judge_hashes(&engine, &hashes, claims, ctx.sealer, adult, now)
                {
                    // Only the floor and the core hold a stored torrent on
                    // a claim; any other rule only takes the claims away.
                    let hold = h.core.then(|| {
                        h.hashes = hashes.clone();
                        h
                    });
                    judged.push((c.torrent_id, c.source, hold));
                }
            }
            let refiled: Vec<RefiledClaims<'_>> = judged
                .iter()
                .map(|(t, s, h)| RefiledClaims {
                    torrent_id: *t,
                    source: *s,
                    filtered: h.as_ref().map(|h| h.filtered(now, ctx.quarantine_secs)),
                })
                .collect();
            let held: Vec<[u8; 20]> = judged
                .iter()
                .flat_map(|(_, _, h)| h.iter().flat_map(|h| h.hashes.iter().copied()))
                .collect();
            let mut step = Step {
                after_item,
                after_claims: last,
                ..Step::default()
            };
            if refiled.is_empty() {
                return Ok(step);
            }
            if !held.is_empty() {
                mark(store, &held);
            }
            let Some(done) = store.refile_claims(&refiled, current)? else {
                return Ok(rejected);
            };
            step.removed = done.claims;
            step.deleted = done.torrents_deleted;
            step.held = held;
            Ok(step)
        }
    }
}

/// An item to remove, with the decision to write: `None` keeps it.
fn judge_item(
    engine: &Engine,
    i: &UnverifiedItem,
    ctx: Ctx<'_>,
    now: i64,
) -> Option<(i64, Option<Hold>)> {
    let Some(claims) = &i.claims else {
        // Unreadable claims can't be judged: removed. An item whose
        // claims a purge took has none to judge: kept until its other
        // finder's next claims (SRC-1).
        return i.unreadable.then_some((i.id, None));
    };
    // Not a swarm hash (no network but clearnet BitTorrent exists yet):
    // it can't be judged by hash, so it's removed rather than kept.
    let Ok(key) = <[u8; 20]>::try_from(i.key.as_slice()) else {
        return Some((i.id, None));
    };
    // Only the floor and the core hold; any other rule removes the item
    // and holds nothing (runner, #383). An item any always-adult source
    // found is judged as adult too, whichever source's claims it holds.
    let adult = ctx.adult.any(i.sources) || ctx.adult.has(i.source);
    judge(engine, key, claims, ctx.sealer, adult, now).map(|h| (i.id, h.core.then_some(h)))
}

/// [`judge`] against every swarm hash of a stored torrent.
fn judge_hashes(
    engine: &Engine,
    hashes: &[[u8; 20]],
    c: &Claims,
    sealer: Option<&Sealer>,
    adult: bool,
    now: i64,
) -> Option<Hold> {
    let first = *hashes.first()?;
    let mut h = judge(engine, first, c, sealer, adult, now);
    for &other in &hashes[1..] {
        if h.is_some() {
            break;
        }
        h = judge(engine, other, c, sealer, adult, now);
    }
    h
}

/// The verification cursor (`sources.verify_unverified`, default off):
/// the stored items are the queue, read a few at a time, so nothing is
/// held in memory however many there are.
#[derive(Debug, Default)]
pub struct Verify {
    pub on: bool,
    /// The last item id handed over.
    pub after: i64,
    /// After a full round, when the next one starts (Unix seconds).
    pub resume_at: Option<i64>,
}

impl Verify {
    /// The next keys to fetch: at most `limit` unverified items after the
    /// cursor, as discoveries from the source whose claims they carry.
    /// Private items stay private: the pool sends them only to their own
    /// source, never to public peer finding (§18.7).
    pub fn next(
        &mut self,
        store: &Store,
        limit: usize,
        now: i64,
    ) -> Result<Vec<Discovery>, lodestar_store::Error> {
        if !self.on || self.resume_at.is_some_and(|at| now < at) {
            return Ok(Vec::new());
        }
        self.resume_at = None;
        // Keys only, no claims decoded; a disabled or purged source's
        // items are passed over.
        let page = store.pending_keys_after(self.after, limit)?;
        let Some(last) = page.last() else {
            if self.after != 0 {
                self.after = 0;
                self.resume_at = Some(now.saturating_add(VERIFY_ROUND_SECS));
            }
            return Ok(Vec::new());
        };
        self.after = last.id;
        Ok(page
            .into_iter()
            .filter_map(|i| {
                let key: [u8; 20] = i.key.as_slice().try_into().ok()?;
                let mut found =
                    lodestar_sources::Found::key(lodestar_metainfo::InfoHash(key), VERIFY_STRATEGY);
                found.private_source = i.private_source;
                Some(Discovery {
                    source: i.source,
                    found,
                    at: now,
                    reply: false,
                })
            })
            .collect())
    }
}

/// `p` with the `claim-mismatch` junk reason added to its enrichment, when
/// the claimed files contradict its verified ones (§18.5). The verified
/// metadata itself is never changed: claims only add the reason. `None`
/// when they agree, or there's no enrichment to add it to.
pub fn with_mismatch(p: &Prepared, verified: &[FileInfo], c: &Claims) -> Option<Prepared> {
    let claimed: Vec<lodestar_classify::ClaimedFile<'_>> = c
        .files
        .iter()
        .map(|f| lodestar_classify::ClaimedFile {
            path: &f.path,
            size: f.size,
        })
        .collect();
    if !lodestar_classify::claim_mismatch(verified, &claimed) {
        return None;
    }
    let e = p.enrichment.as_ref()?;
    let mut record = Record::decode(&e.data).ok()?;
    record
        .junk
        .add(lodestar_classify::JunkReason::ClaimMismatch);
    let mut out = p.clone();
    let e = out.enrichment.as_mut()?;
    e.junk_score = record.junk.score;
    e.data = record.encode();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claimed_paths_split_on_either_separator() {
        assert_eq!(segments("a/b\\c.mkv"), vec!["a", "b", "c.mkv"]);
        assert_eq!(segments("/x//y/"), vec!["x", "y"]);
        assert_eq!(segments(""), vec![""]);
        assert_eq!(segments("//"), vec!["//"]);
    }

    #[test]
    fn a_hold_prints_no_name() {
        let h = Hold {
            hashes: vec![[1; 20]],
            rule: "CS-2a".into(),
            action: Action::Quarantine.code(),
            held: Held::Quarantine,
            sealed: Some(b"sealed-secret".to_vec()),
            evidence: vec![],
            adult_hold: false,
            core: true,
        };
        let s = format!("{h:?}");
        assert!(!s.contains("sealed-secret") && !s.contains("115"), "{s}");
    }
}
