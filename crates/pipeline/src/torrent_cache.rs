//! The pipeline's side of torrent-cache enrichment (architecture §18.2
//! enrichment #3, §18.5, §18.7; v0.6 sources plan SRC-13, §5 Q3 and Q8;
//! `docs/decisions/2026-11-05-SRC-13.md`).
//!
//! - **What may be looked up** ([`Cursor`]): unverified items, a page at
//!   a time from the store, **never one flagged `private_source`** (a
//!   lookup tells the cache the hash, §18.7). For user interest,
//!   [`interest`] reads the key and its `private_source` flag from the
//!   store, never from the caller, and the host passes that flag to the
//!   plugin's `Lookup::new`, which refuses a private key too.
//! - **Host wiring** (SRC-13b, `docs/decisions/2026-11-05-SRC-13b.md`):
//!   the host hands each verified file to [`Feed`]. The pipeline's judge
//!   task runs [`judge_sealed`], and the writer commits what it may store
//!   or hold through its lease-free entry, judging it again through the
//!   always-adult mask.
//! - **What a cached file becomes** ([`judge`]): a `.torrent` whose info
//!   dict hashes to the key is verified metadata (Q8), with provenance
//!   [`PROVENANCE`]. It's judged from the info dict's raw bytes, parsed
//!   with no encoding hint, exactly as BEP 9 delivers it: the file's
//!   top-level `encoding` key isn't covered by the hash, so a cache can't
//!   choose how a name decodes to slip it past the name rules. It still goes through the full filter pipeline
//!   before storage, exactly as a BEP 9 fetch does on the worker
//!   (`worker::prepare_one`): the floor's hash list first, then
//!   enrichment, then the name and files through the locked floor, the
//!   core, your rules, packs and lists, with your classification rules. A
//!   held verdict (drop or quarantine) is never handed on for storage.
//!
//! Nothing here logs a name or a hash.

use lodestar_classify::{Classification, FileInfo};
use lodestar_filter::{Action, Engine, Facts, Item, Sealer, Verdict};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_store::unverified::{MAX_PAGE, Network};
use lodestar_store::{Prepared, Preparer, SourceId, SourceState, Store};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::unverified::VERIFY_ROUND_SECS;

/// What provenance records for metadata from a cache (Q8).
pub const PROVENANCE: &str = "torrent cache";

/// Keys one [`Cursor`] page offers at most. The plugin asks each cache
/// at most once a minute, so a page lasts a long time; the bound keeps
/// its queue, not the store, the limit.
pub const PAGE: usize = 64;

/// A keyset cursor over the store's unverified items, for the plugin's
/// queue. It keeps only an id, so it holds nothing in memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    after: i64,
    /// After the last item, the next round starts no sooner than this
    /// (Unix seconds): a key no cache has is asked again a few times a
    /// day, not in a loop.
    resume_at: i64,
}

/// One page of eligible keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// Keys that may be looked up now ([`Gate::check`]): never a
    /// `private_source` one.
    pub keys: Vec<InfoHash>,
    /// Items passed over because they're `private_source`, or a torrent
    /// stored under the key is private.
    pub private: usize,
    /// Items passed over for any other reason [`Gate::check`] gives.
    pub withheld: usize,
}

/// Why a key isn't sent to a cache now ([`Gate::check`]). The strictest
/// reason is the one given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Withheld {
    /// The cache plugin's own source isn't enabled.
    CacheOff,
    /// On the floor's hash list, or a subscribed list that drops.
    Blocked,
    /// Tombstoned or quarantined.
    Held,
    /// Only private sources found it, or a torrent stored under it is
    /// private (BEP 27) or `private_source` (§18.7).
    Private,
    /// A torrent stored under it is hidden by your filters.
    Hidden,
    /// The unverified item's source is disabled or purged.
    SourceOff,
    /// The store no longer has it: promoted, dropped or purged.
    Gone,
    /// Stored with its whole file list: a cache has nothing to add.
    Complete,
}

/// The check every key passes, read from the store and the filter now:
/// when it's queued ([`Cursor::next`], [`interest`]) and again right
/// before it's sent (`docs/decisions/2026-11-06-X-tcache-recheck.md`).
/// A key can join the floor, a dropping list or a tombstone, or its
/// source can be switched off, in the hours it waits in the plugin's
/// queue; the send-time check is what keeps it from being sent then, as
/// ENR-1's `adult_enrich::check` does for every adult lookup.
#[derive(Clone, Copy)]
pub struct Gate<'a> {
    pub engine: &'a Engine,
    /// The cache plugin's id in the store (`sources.plugin`).
    pub plugin: &'a str,
}

impl std::fmt::Debug for Gate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gate")
            .field("plugin", &self.plugin)
            .finish_non_exhaustive()
    }
}

impl Gate<'_> {
    /// Whether `key` may be sent to a cache now. Reads only: no
    /// transaction is open when it returns, so none is held across the
    /// send. Every hash of a torrent stored under the key is checked,
    /// found by either of a hybrid's hashes (`Store::find`), so an item
    /// under one swarm hash sees a private torrent stored under the other.
    pub fn check(
        &self,
        store: &Store,
        key: InfoHash,
    ) -> Result<Result<(), Withheld>, lodestar_store::Error> {
        let sources = store.sources()?;
        if !cache_on(&sources, self.plugin) {
            return Ok(Err(Withheld::CacheOff));
        }
        key_check(store, self.engine, &sources, key)
    }
}

/// Whether any of `hashes` is on the store's floor list (`cs_hashes`).
/// `Store::held_among` reads tombstones and quarantine only, and the
/// filter's copy of the floor can trail the store's: confirming a held
/// hash writes the store's list, releases the hold, and only then updates
/// the filter, in separate steps.
fn floored(store: &Store, hashes: &[[u8; 20]]) -> Result<bool, lodestar_store::Error> {
    let mut stmt = store
        .connection()
        .prepare_cached("SELECT 1 FROM cs_hashes WHERE hash = ?1")?;
    for h in hashes {
        if stmt.exists([&h[..]])? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cache_on(sources: &[lodestar_store::SourceRow], plugin: &str) -> bool {
    sources
        .iter()
        .any(|s| s.plugin == plugin && s.state == SourceState::Enabled)
}

/// [`Gate::check`] for one key, with the sources already read.
fn key_check(
    store: &Store,
    engine: &Engine,
    sources: &[lodestar_store::SourceRow],
    key: InfoHash,
) -> Result<Result<(), Withheld>, lodestar_store::Error> {
    use lodestar_store::flags;
    let stored = match store.find(&key)? {
        Some(id) => match (store.torrent(id)?, store.torrent_swarm_hashes(id)?) {
            (Some(row), Some(hashes)) => Some((row, hashes)),
            // Deleted between the two reads: a store with nothing under
            // the key is checked as one.
            _ => return Ok(Err(Withheld::Gone)),
        },
        None => None,
    };
    let mut hashes = vec![key.0];
    if let Some((_, h)) = &stored {
        hashes.extend(h.iter().filter(|h| **h != key.0));
    }
    if engine.blocked_hash(&hashes).is_some() {
        return Ok(Err(Withheld::Blocked));
    }
    if store.held_among(&hashes)?.into_iter().any(|h| h) || floored(store, &hashes)? {
        return Ok(Err(Withheld::Held));
    }
    if let Some((row, _)) = stored {
        if row.flags & (flags::PRIVATE_SOURCE | flags::PRIVATE) != 0 {
            return Ok(Err(Withheld::Private));
        }
        // An item a private source reported under any of its hashes keeps
        // the key private, even beside a public import (`Store::insert`
        // leaves the item where it is), as `interest` always held.
        for h in &hashes {
            if store
                .unverified(Network::Bt, h)?
                .is_some_and(|i| i.private_source)
            {
                return Ok(Err(Withheld::Private));
            }
        }
        if row.flags & flags::HIDDEN != 0 {
            return Ok(Err(Withheld::Hidden));
        }
        if row.flags & flags::FILES_PARTIAL == 0 {
            return Ok(Err(Withheld::Complete));
        }
        return Ok(Ok(()));
    }
    let Some(item) = store.unverified(Network::Bt, &key.0)? else {
        return Ok(Err(Withheld::Gone));
    };
    if item.private_source {
        return Ok(Err(Withheld::Private));
    }
    // As `Store::pending_keys_after` reads them: the item's source must
    // be enabled or paused.
    let live = sources.iter().any(|s| {
        s.id == item.source && matches!(s.state, SourceState::Enabled | SourceState::Paused)
    });
    if !live {
        return Ok(Err(Withheld::SourceOff));
    }
    Ok(Ok(()))
}

impl Cursor {
    /// The next page, at most `limit` keys (capped at [`PAGE`]). Empty
    /// while the round is resting, and while the cache plugin is off (the
    /// cursor then stays where it is). Only items whose source is enabled
    /// or paused are read (`Store::pending_keys_after`), and each key
    /// offered has passed [`Gate::check`].
    pub fn next(
        &mut self,
        store: &Store,
        gate: &Gate<'_>,
        limit: usize,
        now: i64,
    ) -> Result<Page, lodestar_store::Error> {
        if now < self.resume_at {
            return Ok(Page::default());
        }
        let sources = store.sources()?;
        if !cache_on(&sources, gate.plugin) {
            return Ok(Page::default());
        }
        let rows = store.pending_keys_after(self.after, limit.clamp(1, PAGE.min(MAX_PAGE)))?;
        let Some(last) = rows.last() else {
            // The end: rest, then start over.
            self.after = 0;
            self.resume_at = now.saturating_add(VERIFY_ROUND_SECS);
            return Ok(Page::default());
        };
        let after = last.id;
        let mut page = Page::default();
        for row in rows {
            if row.private_source {
                page.private += 1;
                continue;
            }
            // Caches serve BitTorrent files only.
            if row.network != Network::Bt {
                continue;
            }
            let Ok(k) = <[u8; 20]>::try_from(row.key.as_slice()) else {
                continue;
            };
            let key = InfoHash(k);
            // One lookup per key, however many rows hold it: each costs a
            // cache's once-a-minute slot.
            if page.keys.contains(&key) {
                continue;
            }
            match key_check(store, gate.engine, &sources, key)? {
                Ok(()) => page.keys.push(key),
                Err(Withheld::Private) => page.private += 1,
                Err(_) => page.withheld += 1,
            }
        }
        // Only once the whole page was read: on a store error the cursor
        // stays where it was.
        self.after = after;
        Ok(page)
    }
}

/// Why a cached file was refused before the filter judged its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Its info dict doesn't hash to the key (Q8). The plugin checks this
    /// first; it's checked again here.
    Mismatch,
    /// The key is on the floor's hash list.
    Blocked,
    /// Doesn't parse as an info dict, or hashes to the key but the store
    /// can't prepare it.
    Invalid,
}

/// A cached torrent that may be stored, with what the filter made of it:
/// the same record the worker hands the writer for a BEP 9 fetch.
#[derive(Debug)]
pub struct Admitted {
    pub key: InfoHash,
    pub torrent: Prepared,
    pub files: Vec<FileInfo>,
    pub class: Classification,
    /// A verdict that leaves it stored: none, or hide, demote or flag.
    pub verdict: Verdict,
    /// The rule-set version the verdict used: the writer judges again if
    /// the rules have moved on by its commit.
    pub version: u64,
    /// The classification rule applied (§20.9).
    pub fix: Option<bool>,
}

/// What [`judge`] made of a cached file.
#[derive(Debug)]
pub enum Judged {
    /// Store it, as verified metadata from a cache.
    Store(Box<Admitted>),
    /// The filter holds it (drop or quarantine): never stored. The verdict
    /// says which rule, for the hold the writer records.
    Held {
        hashes: Vec<[u8; 20]>,
        verdict: Verdict,
        version: u64,
        /// The name sealed for a quarantine's review ([`judge_sealed`]
        /// with a sealer); `None` otherwise.
        sealed: Option<Vec<u8>>,
        /// Held by the Adult-only core only because your rule made it
        /// adult: a tombstone when the quarantine ends (§20.9).
        adult_hold: bool,
    },
    Refused(Refusal),
}

/// Judges a cached file's info dict for `key` as the worker judges a
/// fetched one: the hash check, the floor's hash list, preparing,
/// enrichment, then the full filter with your classification rules.
///
/// `raw_info` is the info dict's bytes exactly as the file holds them
/// (the plugin's `Verified::info_raw`). They're parsed here with no
/// encoding hint and kept for the store as a BEP 9 fetch's are.
pub fn judge(
    engine: &Engine,
    preparer: &mut Preparer,
    key: InfoHash,
    raw_info: &[u8],
    now: i64,
) -> Judged {
    judge_sealed(engine, preparer, None, key, raw_info, now)
}

/// [`judge`], sealing a quarantined name with `sealer` for its review, as
/// the writer seals a fetched one.
pub fn judge_sealed(
    engine: &Engine,
    preparer: &mut Preparer,
    sealer: Option<&Sealer>,
    key: InfoHash,
    raw_info: &[u8],
    now: i64,
) -> Judged {
    // Hint-free, as `worker::prepare_one` parses a fetched dict.
    let Ok(info) = Info::parse(raw_info) else {
        return Judged::Refused(Refusal::Invalid);
    };
    let info = &info;
    let v2 = info.v2.map(|h| h.truncated());
    if info.v1 != Some(key) && v2 != Some(key) {
        return Judged::Refused(Refusal::Mismatch);
    }
    let swarm: Vec<[u8; 20]> = info.swarm_hashes().map(|h| h.0).collect();
    if engine.blocked_hash(&swarm).is_some() || engine.blocked_hash(&[key.0]).is_some() {
        return Judged::Refused(Refusal::Blocked);
    }
    let Ok(mut prepared) = preparer.prepare(info, Some(raw_info)) else {
        return Judged::Refused(Refusal::Invalid);
    };
    let files = crate::enrich::file_infos(&info.files, info.path_encoding);
    let (enriched, class) = crate::enrich::enrich_infos(&prepared.name, &files);
    prepared.enrichment = Some(enriched);
    let hashes: Vec<[u8; 20]> = prepared.swarm_hashes().map(|h| h.0).collect();
    let facts = Facts {
        class: &class,
        size: prepared.size,
        piece_len: prepared.piece_len,
        flags: prepared.flags,
        discovered_at: now,
        now,
    };
    let item = Item {
        hashes: &hashes,
        name: &prepared.name,
        files: &files,
        content_type: class.content_type,
    };
    let (verdict, says) = engine.judge_classified(&item, Some(&facts), now);
    let fix = crate::enrich::rule_fix(says, &class);
    let held = verdict.decision.as_ref().map(|d| d.action);
    if matches!(held, Some(Action::Drop | Action::Quarantine)) {
        let sealed = (held == Some(Action::Quarantine))
            .then(|| sealer.and_then(|s| s.seal(&prepared.name).ok()))
            .flatten();
        let adult_hold = fix == Some(true)
            && verdict
                .decision
                .as_ref()
                .is_some_and(lodestar_filter::reclassify::adult_hold);
        return Judged::Held {
            hashes,
            verdict,
            version: engine.version,
            sealed,
            adult_hold,
        };
    }
    if fix.is_some() {
        prepared.enrichment = Some(crate::enrich::enrich_with_rule(&prepared.name, &files, fix));
    }
    Judged::Store(Box::new(Admitted {
        key,
        torrent: prepared,
        files,
        class,
        verdict,
        version: engine.version,
        fix,
    }))
}

/// A key user interest may look up: the item's or torrent's own key and
/// its `private_source` flag, both read from the store, never from the
/// caller (`docs/decisions/2026-11-05-SRC-13b.md`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Interest {
    pub key: InfoHash,
    /// The store has it as private (§18.7): the plugin's `Lookup::new`
    /// refuses it, and so does the host before it gets there.
    pub private_source: bool,
}

// The key is what a lookup leaks: never in a log line.
impl std::fmt::Debug for Interest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interest")
            .field("private_source", &self.private_source)
            .finish_non_exhaustive()
    }
}

/// What interest in item or torrent `id` (a search hit, a detail view)
/// may look up, from the store alone. `None` when there's nothing a cache
/// could add, or the store doesn't know the id:
/// - **an unverified item** gives its key;
/// - **a stored torrent** only when its file list is partial (an import);
/// - anything else (a complete torrent, an unknown id, a non-BitTorrent
///   item) gives `None`: it isn't looked up.
///
/// The key then passes [`Gate::check`], as it does again before it's
/// sent: a private key (§18.7) gives `private_source`, which the host
/// passes to the plugin's `Lookup::new`, which refuses it; any other
/// reason gives `None`.
pub fn interest(
    store: &Store,
    gate: &Gate<'_>,
    id: i64,
) -> Result<Option<Interest>, lodestar_store::Error> {
    use lodestar_store::flags;
    let key = if let Some(row) = store.torrent(id)? {
        if row.flags & flags::FILES_PARTIAL == 0 {
            return Ok(None);
        }
        row.info_hash
    } else {
        let Some(item) = store
            .pending_keys_after(id.saturating_sub(1), 1)?
            .into_iter()
            .next()
            .filter(|i| i.id == id && i.network == Network::Bt)
        else {
            return Ok(None);
        };
        let Ok(k) = <[u8; 20]>::try_from(item.key.as_slice()) else {
            return Ok(None);
        };
        InfoHash(k)
    };
    Ok(match gate.check(store, key)? {
        Ok(()) => Some(Interest {
            key,
            private_source: false,
        }),
        Err(Withheld::Private) => Some(Interest {
            key,
            private_source: true,
        }),
        Err(_) => None,
    })
}

/// A verified file from the plugin, as the host hands it to the pipeline
/// ([`Feed`]): the key, the info dict's raw bytes and the fields outside
/// it as claims (`Verified::claims`, `Verified::provenance`). Its `Debug`
/// shows none of them.
pub struct Fetched {
    pub key: InfoHash,
    pub info_raw: Vec<u8>,
    pub claims: lodestar_sources::claims::Claims,
    pub provenance: lodestar_sources::claims::Provenance,
}

impl std::fmt::Debug for Fetched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetched").finish_non_exhaustive()
    }
}

/// Where the host hands the pipeline the plugin's verified files
/// (`Pipeline::torrent_cache_feed`). Cheap to clone. Sending waits for
/// room, so a slow writer slows the plugin's lookups rather than filling
/// memory.
#[derive(Debug, Clone)]
pub struct Feed {
    tx: tokio::sync::mpsc::Sender<Fetched>,
}

/// The pipeline has stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

impl Feed {
    pub async fn send(&self, f: Fetched) -> Result<(), Closed> {
        self.tx.send(f).await.map_err(|_| Closed)
    }
}

/// What the pipeline did with the plugin's files, for the status page.
/// Never holds a hash or a name.
#[derive(Debug, Default)]
pub struct CacheStats {
    /// Files handed over by the host.
    pub received: AtomicU64,
    /// Stored as verified metadata, as new torrents or promotions.
    pub stored: AtomicU64,
    /// Of `stored`, unverified items promoted under their own ids.
    pub promoted: AtomicU64,
    /// Already stored: the file added nothing but its source.
    pub existing: AtomicU64,
    /// Held by the filter (drop or quarantine), at judging or at commit.
    pub held: AtomicU64,
    /// Refused before the filter: a hash mismatch, the floor's hash list,
    /// or a dict that doesn't parse or prepare ([`Refusal`]).
    pub refused: AtomicU64,
    /// Held under rules that changed before the commit: judged again by
    /// the writer under the current ones (and, should they change again
    /// meanwhile, dropped unwritten).
    pub stale: AtomicU64,
    /// Lost to a store error.
    pub errors: AtomicU64,
}

/// What the writer commits for one file: a judged file from source
/// `source` with its claims, at `at`.
pub(crate) struct ToCommit {
    pub source: SourceId,
    pub judged: Judged,
    pub claims: lodestar_sources::claims::Claims,
    pub provenance: lodestar_sources::claims::Provenance,
    pub at: i64,
    /// The info dict's raw bytes, kept for a `Held` only: the writer
    /// judges it again when the rules changed before its commit.
    pub info_raw: Vec<u8>,
    pub key: InfoHash,
}

/// A feed's channel: its sending half for the host, and the receiving
/// half the pipeline's judge task reads. The pipeline makes its own
/// (`Pipeline::torrent_cache_feed`); a host's tests use this one.
pub fn feed() -> (Feed, tokio::sync::mpsc::Receiver<Fetched>) {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    (Feed { tx }, rx)
}

/// Judges each file the host hands over ([`judge_sealed`], on the blocking
/// pool, under the filter's current rules) and passes what may be stored
/// or held on to the writer. A refusal is counted and dropped here. Ends
/// when the feed or the writer closes.
pub(crate) async fn judge_task(
    mut fetched: tokio::sync::mpsc::Receiver<Fetched>,
    out: tokio::sync::mpsc::Sender<ToCommit>,
    mut preparer: Preparer,
    filter: std::sync::Arc<lodestar_filter::Handle>,
    sealer: Option<std::sync::Arc<Sealer>>,
    source: SourceId,
    stats: std::sync::Arc<crate::WriterStats>,
) {
    while let Some(f) = fetched.recv().await {
        stats.torrent_cache.received.fetch_add(1, Ordering::Relaxed);
        let engine = filter.current();
        let sealer = sealer.clone();
        let now = crate::unix_now();
        let Ok((p, judged, f)) = tokio::task::spawn_blocking(move || {
            let j = judge_sealed(
                &engine,
                &mut preparer,
                sealer.as_deref(),
                f.key,
                &f.info_raw,
                now,
            );
            (preparer, j, f)
        })
        .await
        else {
            return;
        };
        preparer = p;
        if let Judged::Refused(_) = judged {
            stats.torrent_cache.refused.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let held = matches!(judged, Judged::Held { .. });
        let commit = ToCommit {
            source,
            judged,
            claims: f.claims,
            provenance: f.provenance,
            at: now,
            info_raw: if held { f.info_raw } else { Vec::new() },
            key: f.key,
        };
        if out.send(commit).await.is_err() {
            return;
        }
    }
}
