//! The pipeline's side of torrent-cache enrichment (architecture §18.2
//! enrichment #3, §18.5, §18.7; v0.6 sources plan SRC-13, §5 Q3 and Q8;
//! `docs/decisions/2026-11-05-SRC-13.md`).
//!
//! - **What may be looked up** ([`Cursor`]): unverified items, a page at
//!   a time from the store, **never one flagged `private_source`** (a
//!   lookup tells the cache the hash, §18.7). User interest is the
//!   host's to add; it passes the item's `private_source` flag to the
//!   plugin's `Lookup::new`, which refuses a private key too.
//! - **What a cached file becomes** ([`judge`]): a `.torrent` whose info
//!   dict hashes to the key is verified metadata (Q8), with provenance
//!   [`PROVENANCE`]. It still goes through the full filter pipeline
//!   before storage, exactly as a BEP 9 fetch does on the worker
//!   (`worker::prepare_one`): the floor's hash list first, then
//!   enrichment, then the name and files through the locked floor, the
//!   core, your rules, packs and lists, with your classification rules. A
//!   held verdict (drop or quarantine) is never handed on for storage.
//!
//! Nothing here logs a name or a hash.

use lodestar_classify::{Classification, FileInfo};
use lodestar_filter::{Action, Engine, Facts, Item, Verdict};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_store::unverified::{MAX_PAGE, Network};
use lodestar_store::{Prepared, Preparer, Store};

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
    /// Keys that may be looked up: never a `private_source` one.
    pub keys: Vec<InfoHash>,
    /// Items passed over because they're `private_source`.
    pub private: usize,
}

impl Cursor {
    /// The next page, at most `limit` keys (capped at [`PAGE`]). Empty
    /// while the round is resting. Only items whose source is enabled or
    /// paused are read (`Store::pending_keys_after`).
    pub fn next(
        &mut self,
        store: &Store,
        limit: usize,
        now: i64,
    ) -> Result<Page, lodestar_store::Error> {
        if now < self.resume_at {
            return Ok(Page::default());
        }
        let rows = store.pending_keys_after(self.after, limit.clamp(1, PAGE.min(MAX_PAGE)))?;
        let Some(last) = rows.last() else {
            // The end: rest, then start over.
            self.after = 0;
            self.resume_at = now.saturating_add(VERIFY_ROUND_SECS);
            return Ok(Page::default());
        };
        self.after = last.id;
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
            // One lookup per key, however many rows hold it: each costs a
            // cache's once-a-minute slot.
            if let Ok(k) = <[u8; 20]>::try_from(row.key.as_slice())
                && !page.keys.contains(&InfoHash(k))
            {
                page.keys.push(InfoHash(k));
            }
        }
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
    /// Hashes to the key but the store can't prepare it.
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
    },
    Refused(Refusal),
}

/// Judges a cached file's info dict for `key` as the worker judges a
/// fetched one: the hash check, the floor's hash list, preparing,
/// enrichment, then the full filter with your classification rules.
pub fn judge(
    engine: &Engine,
    preparer: &mut Preparer,
    key: InfoHash,
    info: &Info,
    now: i64,
) -> Judged {
    let v2 = info.v2.map(|h| h.truncated());
    if info.v1 != Some(key) && v2 != Some(key) {
        return Judged::Refused(Refusal::Mismatch);
    }
    let swarm: Vec<[u8; 20]> = info.swarm_hashes().map(|h| h.0).collect();
    if engine.blocked_hash(&swarm).is_some() || engine.blocked_hash(&[key.0]).is_some() {
        return Judged::Refused(Refusal::Blocked);
    }
    // The raw dict isn't archived: the cache's file is the only copy
    // (importers do the same).
    let Ok(mut prepared) = preparer.prepare(info, None) else {
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
    if matches!(
        verdict.decision.as_ref().map(|d| d.action),
        Some(Action::Drop | Action::Quarantine)
    ) {
        return Judged::Held {
            hashes,
            verdict,
            version: engine.version,
        };
    }
    let fix = crate::enrich::rule_fix(says, &class);
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
