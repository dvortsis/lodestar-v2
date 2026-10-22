//! Classification as it's stored (§20.9): detection, your per-torrent
//! correction and your classification rules, in the form the store keeps
//! (`Enriched`). Filtering judges detection's own classification, and
//! also judges as adult what a correction or rule relabels adult.

use lodestar_classify::{
    Classification, ContentType, Correction, ENRICHMENT_VERSION, FileInfo, Record,
    classify_corrected,
};
use lodestar_store::{Enriched, Filtered, Held, Refiled, TorrentRow, flags};

use crate::core::{Action, Item};
use crate::engine::{Decision, Engine};
use crate::matcher::Facts;
use crate::seal::Sealer;

/// The stored enrichment of a torrent with this name and file list, your
/// correction applied, and the classification it came from.
pub fn enrich_corrected(
    name: &str,
    files: &[FileInfo],
    fix: Correction,
) -> (Enriched, Classification) {
    let c = classify_corrected(name, files, fix);
    let enriched = Enriched {
        version: ENRICHMENT_VERSION,
        content_type: c.content_type.code(),
        junk_score: c.junk.score,
        comp: c.composition.encode(),
        data: Record::new(&c).encode(),
    };
    (enriched, c)
}

/// The enrichment of a stored torrent: its own correction `own`, then the
/// newest matching classification rule, then detection.
pub fn enrich_row(
    row: &TorrentRow,
    files: &[FileInfo],
    own: Correction,
    engine: Option<&Engine>,
    now: i64,
) -> (Enriched, Classification) {
    let r = reclassify_row(row, files, own, engine, now);
    (r.enriched, r.class)
}

/// A stored torrent classified again (§20.9).
#[derive(Debug)]
pub struct Reclassified {
    pub enriched: Enriched,
    pub class: Classification,
    /// When it was relabelled adult (detection didn't call it adult), the
    /// decision filtering makes of it as adult, which ingest would have
    /// applied: the Adult-only core checks and your `type:adult` rules.
    pub as_adult: Option<Decision>,
}

/// [`enrich_row`], judging a torrent relabelled adult as adult too, so
/// nothing stored as adult has skipped the Adult-only core (§20.9).
pub fn reclassify_row(
    row: &TorrentRow,
    files: &[FileInfo],
    own: Correction,
    engine: Option<&Engine>,
    now: i64,
) -> Reclassified {
    let plain = Correction { adult: None, ..own };
    let (enriched, class) = enrich_corrected(&row.name, files, plain);
    let detected_adult = class.content_type == ContentType::Adult;
    let says = own
        .adult
        .or_else(|| engine.and_then(|e| rule_says(e, row, files, &class, now)));
    let Some(adult) = says.filter(|a| *a != detected_adult) else {
        return Reclassified {
            enriched,
            class,
            as_adult: None,
        };
    };
    let fix = Correction {
        adult: Some(adult),
        ..own
    };
    let (enriched, class) = enrich_corrected(&row.name, files, fix);
    let as_adult = match engine {
        Some(e) if adult => {
            let hashes = row_hashes(row);
            let facts = facts(row, &class, now);
            let item = Item {
                hashes: &hashes,
                name: &row.name,
                files,
                content_type: ContentType::Adult,
            };
            e.judge(&item, Some(&facts), now).decision
        }
        _ => None,
    };
    Reclassified {
        enriched,
        class,
        as_adult,
    }
}

/// What your classification rules say about a stored torrent.
fn rule_says(
    engine: &Engine,
    row: &TorrentRow,
    files: &[FileInfo],
    class: &Classification,
    now: i64,
) -> Option<bool> {
    if engine.class_rules.is_empty() {
        return None;
    }
    let facts = facts(row, class, now);
    let item = Item {
        hashes: &[],
        name: &row.name,
        files,
        content_type: class.content_type,
    };
    engine.class_correction(&item, Some(&facts))
}

fn facts<'a>(row: &TorrentRow, class: &'a Classification, now: i64) -> Facts<'a> {
    Facts {
        class,
        size: row.size,
        piece_len: row.piece_len,
        flags: row.flags,
        discovered_at: row.discovered_at,
        now,
    }
}

/// Every swarm hash of a stored torrent: its primary hash and, for a
/// hybrid, the truncated v2 hash.
fn row_hashes(row: &TorrentRow) -> Vec<[u8; 20]> {
    let mut out = vec![row.info_hash.0];
    if let Some(t) = row.info_hash_v2.map(|h| h.truncated()) {
        if t != row.info_hash {
            out.push(t.0);
        }
    }
    out
}

/// A stored torrent's decision as the store records it (§20.3): what's
/// held back is quarantined (name sealed) or tombstoned, what's kept gets
/// its marks.
#[derive(Debug, Clone)]
pub struct Refiling {
    pub id: i64,
    pub hashes: Vec<[u8; 20]>,
    pub rule: String,
    pub action: u8,
    pub held: Option<Held>,
    pub sealed: Option<Vec<u8>>,
    pub evidence: Vec<u8>,
    pub flags: i64,
}

impl Refiling {
    pub fn new(row: &TorrentRow, d: &Decision, sealer: Option<&Sealer>) -> Refiling {
        let held = match d.action {
            Action::Drop => Some(Held::Tombstone),
            Action::Quarantine => Some(Held::Quarantine),
            _ => None,
        };
        let flags = match d.action {
            Action::Hide => flags::HIDDEN,
            Action::Demote => flags::DEMOTED,
            Action::Flag => flags::FLAGGED,
            _ => 0,
        };
        Refiling {
            id: row.id,
            hashes: row_hashes(row),
            rule: d.rule.id(),
            action: d.action.code(),
            held,
            sealed: (held == Some(Held::Quarantine))
                .then(|| sealer.and_then(|s| s.seal(&row.name).ok()))
                .flatten(),
            evidence: d.evidence_codes(),
            flags,
        }
    }

    /// The store's view, decided at `at`; a quarantine lasts until
    /// `expires_at`.
    pub fn refiled(&self, at: i64, expires_at: i64) -> Refiled<'_> {
        Refiled {
            id: self.id,
            filtered: Filtered {
                hashes: &self.hashes,
                rule: &self.rule,
                action: self.action,
                held: self.held,
                sealed: self.sealed.as_deref(),
                evidence: &self.evidence,
                at,
                expires_at,
            },
            flags: self.flags,
        }
    }
}
