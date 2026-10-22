//! Classification as it's stored (§20.9): detection, your per-torrent
//! correction and your classification rules, in the form the store keeps
//! (`Enriched`). Filtering judges detection's own classification, and
//! also judges as adult a stored torrent that becomes adult.

use lodestar_classify::{
    Classification, ContentType, Correction, ENRICHMENT_VERSION, FileInfo, Record,
    classify_corrected,
};
use lodestar_store::{Enriched, Filtered, Held, Refiled, SHADOW, TorrentRow, flags};

use crate::core::{Action, Item};
use crate::engine::{Decision, Engine, RuleRef, Verdict};
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

/// A stored torrent classified again (§20.9).
#[derive(Debug)]
pub struct Reclassified {
    pub enriched: Enriched,
    pub class: Classification,
    /// When it becomes adult (it wasn't stored as adult), what filtering
    /// makes of it as adult, which ingest would have applied: the
    /// Adult-only core checks and your `type:adult` rules.
    pub as_adult: Option<Verdict>,
    /// Your correction or a classification rule made it adult, not
    /// detection.
    pub relabelled: bool,
}

impl Reclassified {
    /// How the store records its judgement as adult (§20.3): the decision,
    /// then what rules in shadow mode would have done. A hold by the
    /// Adult-only core of a torrent you relabelled adult is an adult hold:
    /// at the end of its quarantine it becomes a tombstone, since a refetch
    /// wouldn't know your correction (§20.9).
    pub fn refilings(&self, row: &TorrentRow, sealer: Option<&Sealer>) -> Vec<Refiling> {
        let Some(v) = &self.as_adult else {
            return Vec::new();
        };
        let mut out: Vec<Refiling> = v
            .decision
            .iter()
            .map(|d| Refiling {
                adult_hold: self.relabelled && adult_hold(d),
                ..Refiling::new(row, d, sealer)
            })
            .collect();
        out.extend(v.shadow.iter().map(|d| Refiling::shadow(row, d)));
        out
    }
}

/// A stored torrent classified again: its own correction `own`, then the
/// newest matching classification rule, then detection. `was_adult` is
/// whether it's stored as adult now.
///
/// A torrent that becomes adult, whatever the cause (a newer parser's
/// detection, a correction or a rule), is judged as adult too, so nothing
/// stored as adult has skipped the Adult-only core (§20.9). A torrent
/// with no stored classification (stored before enrichment) counts as not
/// adult. Without an `engine` to judge it, an Is adult correction is
/// ignored.
pub fn reclassify_row(
    row: &TorrentRow,
    files: &[FileInfo],
    own: Correction,
    was_adult: bool,
    engine: Option<&Engine>,
    now: i64,
) -> Reclassified {
    let own = Correction {
        adult: own.adult.filter(|adult| !adult || engine.is_some()),
        ..own
    };
    let plain = Correction { adult: None, ..own };
    let (enriched, class) = enrich_corrected(&row.name, files, plain);
    let detected_adult = class.content_type == ContentType::Adult;
    let says = own
        .adult
        .or_else(|| engine.and_then(|e| rule_says(e, row, files, &class, now)));
    let fix = says.filter(|a| *a != detected_adult);
    let (enriched, class) = match fix {
        Some(adult) => {
            let fix = Correction {
                adult: Some(adult),
                ..own
            };
            enrich_corrected(&row.name, files, fix)
        }
        None => (enriched, class),
    };
    let as_adult = match engine {
        Some(e) if class.content_type == ContentType::Adult && !was_adult => {
            let hashes = row_hashes(row);
            let facts = facts(row, &class, now);
            let item = Item {
                hashes: &hashes,
                name: &row.name,
                files,
                content_type: ContentType::Adult,
            };
            Some(e.judge(&item, Some(&facts), now))
        }
        _ => None,
    };
    Reclassified {
        enriched,
        class,
        as_adult,
        relabelled: fix == Some(true),
    }
}

/// Whether holding a torrent relabelled adult on decision `d` is an adult
/// hold: one by the Adult-only core, which only checks it because it's
/// adult. Any other rule would hold it as not adult too.
pub fn adult_hold(d: &Decision) -> bool {
    matches!(d.rule, RuleRef::Core(r) if r.adult_only())
        && matches!(d.action, Action::Quarantine | Action::Drop)
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
    /// An adult hold ([`Filtered::adult_hold`]).
    pub adult_hold: bool,
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
            adult_hold: false,
        }
    }

    /// What a rule in shadow mode would have done: logged, not applied.
    pub fn shadow(row: &TorrentRow, d: &Decision) -> Refiling {
        Refiling {
            id: row.id,
            hashes: row_hashes(row),
            rule: d.rule.id(),
            action: d.action.code() | SHADOW,
            held: None,
            sealed: None,
            evidence: d.evidence_codes(),
            flags: 0,
            adult_hold: false,
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
                adult_hold: self.adult_hold,
            },
            flags: self.flags,
        }
    }
}
