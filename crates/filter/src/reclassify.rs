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

/// What today's core makes of stored torrent `row` (§20.5), judged as
/// ingest judges it: on detection's own classification, so a Not adult
/// correction or rule never takes it out of the Adult-only core checks,
/// and as Adult too where your correction, your rules or an always-adult
/// source make it adult (`also_adult` for the first and last), the
/// stricter verdict kept. The decision is applied when it holds the
/// torrent back, whatever rule decided, or when it's the core's: the
/// packs' and your rules' marks have their own re-evaluation (§20.3).
/// With it, whether the hold is an adult hold (as [`Reclassified`]'s).
pub fn rejudge_core(
    row: &TorrentRow,
    files: &[FileInfo],
    also_adult: bool,
    engine: &Engine,
    now: i64,
) -> Option<(Decision, bool)> {
    let (_, class) = enrich_corrected(&row.name, files, Correction::default());
    let hashes = row_hashes(row);
    let item = Item {
        hashes: &hashes,
        name: &row.name,
        files,
        content_type: class.content_type,
    };
    let facts = facts(row, &class, now);
    let (v, _) = engine.judge_classified_with(&item, Some(&facts), now, also_adult, None);
    let d = v.decision.filter(|d| {
        matches!(d.action, Action::Drop | Action::Quarantine) || matches!(d.rule, RuleRef::Core(_))
    })?;
    // Held only because it was read as adult, which detection didn't say.
    let relabelled = class.content_type != ContentType::Adult;
    let held_as_adult = relabelled && adult_hold(&d);
    Some((d, held_as_adult))
}

/// Whether holding a torrent relabelled adult on decision `d` is an adult
/// hold: one by the Adult-only core, which only checks it because it's
/// adult. Any other rule would hold it as not adult too.
pub fn adult_hold(d: &Decision) -> bool {
    matches!(d.rule, RuleRef::Core(r) if r.adult_only())
        && matches!(d.action, Action::Quarantine | Action::Drop)
}

/// The correction a stored torrent is classified with when `adult` is the
/// bitset of always-adult sources (X-source-adult): your own correction
/// `own`, with Is adult added when one of its `sources` is always adult
/// and you haven't said Not adult. Clearing a correction or setting only
/// Not junk never un-adults it; your explicit Not adult does.
pub fn source_adult(own: Correction, sources: u64, adult: u64) -> Correction {
    match own.adult {
        None if adult & sources != 0 => Correction {
            adult: Some(true),
            ..own
        },
        _ => own,
    }
}

/// What filtering makes of an item an always-adult source found, and the
/// fix to store with it: `Some(true)` when detection didn't call it adult,
/// else `None` (a Not adult rule never applies to it).
///
/// The engine's verdict with your classification rules
/// ([`Engine::judge_classified`]), and, unless that already judged the
/// adult reading, the adult reading too; the stricter of the two stands.
/// The floor's hit comes back alone, as it does from the engine.
pub fn judge_source_adult(
    engine: &Engine,
    item: &Item<'_>,
    facts: &Facts<'_>,
    now: i64,
) -> (Verdict, Option<bool>) {
    judge_source_adult_with(engine, item, facts, now, None)
}

/// [`judge_source_adult`], keeping what it found above the pack layer in
/// `trail` for the writer's second judgment
/// ([`Engine::judge_at_commit_reusing`]). The Adult reading shares what
/// the reading as found read ([`Engine::judge_classified_with`]).
pub fn judge_source_adult_with(
    engine: &Engine,
    item: &Item<'_>,
    facts: &Facts<'_>,
    now: i64,
    trail: Option<&mut crate::engine::Trail>,
) -> (Verdict, Option<bool>) {
    // The engine judges it as adult unless detection or an Is adult rule
    // already did, or the floor decided, as before.
    // (`item.content_type` is `facts.class`'s.)
    let (v, _) = engine.judge_classified_with(item, Some(facts), now, true, trail);
    let detected = facts.class.content_type == ContentType::Adult;
    (v, (!detected).then_some(true))
}

/// [`judge_source_adult`] as it judged before X-commit-rejudge-reuse: the
/// Adult reading on a torrent prepared afresh. The differential tests'
/// oracle.
#[doc(hidden)]
pub fn judge_source_adult_unshared(
    engine: &Engine,
    item: &Item<'_>,
    facts: &Facts<'_>,
    now: i64,
) -> (Verdict, Option<bool>) {
    let (v, says) = engine.judge_classified_unshared(item, Some(facts), now);
    let detected = facts.class.content_type == ContentType::Adult;
    let fix = (!detected).then_some(true);
    // The engine judged it as adult already: detection or an Is adult
    // rule said so.
    // (`item.content_type` is `facts.class`'s, so `detected` covers it.)
    if detected || says == Some(true) {
        return (v, fix);
    }
    // A locked rule's hit (the floor, CS-0a) is final, as the engine
    // returns it.
    if v.decision
        .as_ref()
        .is_some_and(|d| matches!(d.rule, RuleRef::Core(r) if r.locked()))
    {
        return (v, fix);
    }
    let adult_class = facts.class.as_adult();
    let adult_facts = Facts {
        class: &adult_class,
        ..*facts
    };
    let adult_item = Item {
        content_type: ContentType::Adult,
        ..*item
    };
    let as_adult = engine.judge(&adult_item, Some(&adult_facts), now);
    (stricter(v, as_adult), fix)
}

/// The heavier decision of two verdicts (a tie keeps `a`'s), and both
/// verdicts' shadow decisions: the engine's own rule for two readings.
pub fn stricter(a: Verdict, b: Verdict) -> Verdict {
    crate::engine::stricter(a, b)
}

/// What a classification rule's say changes: Is adult (`true`) or Not
/// adult (`false`) where it differs from detection, else nothing.
pub fn rule_fix(says: Option<bool>, class: &Classification) -> Option<bool> {
    says.filter(|adult| *adult != (class.content_type == ContentType::Adult))
}

/// The engine's verdict on a record about to be stored or held: a hash on
/// the floor's or a dropping list first, then the full judgement with your
/// classification rules, and what those rules change (`rule_fix`).
pub fn judge_with_rules(
    engine: &Engine,
    item: &Item,
    class: &Classification,
    facts: &Facts<'_>,
    now: i64,
) -> (Verdict, Option<bool>) {
    if let Some(d) = engine.blocked_hash(item.hashes) {
        let v = Verdict {
            decision: Some(d),
            shadow: Vec::new(),
        };
        return (v, None);
    }
    let (v, says) = engine.judge_classified(item, Some(facts), now);
    (v, rule_fix(says, class))
}

/// An import candidate as filtering sees it: no files, and its name and
/// size only when the source gave them.
#[derive(Clone, Copy)]
pub struct Candidate<'a> {
    pub hash: [u8; 20],
    /// A hybrid's v2 hash; its truncated form is a swarm hash too.
    pub hash_v2: Option<[u8; 32]>,
    pub name: &'a str,
    pub size: Option<u64>,
    pub source_at: Option<i64>,
}

// No name in its `Debug`: it may be one a rule holds.
impl std::fmt::Debug for Candidate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate")
            .field("name_len", &self.name.len())
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// What filtering makes of an import candidate.
#[derive(Debug)]
pub struct CandidateJudgement {
    /// Its swarm hashes: the primary, and a hybrid's truncated v2 hash.
    pub hashes: Vec<[u8; 20]>,
    pub verdict: Verdict,
    /// What your classification rules change (`rule_fix`).
    pub fix: Option<bool>,
}

/// Judges an import candidate, the same way at import and in the writer's
/// refile sweep (`docs/decisions/2026-10-31-X-import-refile-2.md`): its
/// hashes, then its name with your classification rules. A `source_at`
/// outside `1..=now` counts as discovered now.
pub fn judge_candidate(engine: &Engine, c: Candidate<'_>, now: i64) -> CandidateJudgement {
    // As `lodestar_metainfo::InfoHashV2::truncated` and
    // `WaitingCandidate::hashes` (this crate doesn't depend on metainfo).
    let mut hashes = vec![c.hash];
    if let Some(v2) = c.hash_v2 {
        let mut t = [0u8; 20];
        t.copy_from_slice(&v2[..20]);
        if t != c.hash {
            hashes.push(t);
        }
    }
    let class = classify_corrected(c.name, &[], Correction::default());
    let facts = Facts {
        class: &class,
        size: c.size.unwrap_or(0),
        piece_len: 0,
        flags: 0,
        discovered_at: c
            .source_at
            .filter(|at| (1..=now).contains(at))
            .unwrap_or(now),
        now,
    };
    let item = Item {
        hashes: &hashes,
        name: c.name,
        files: &[],
        content_type: class.content_type,
    };
    let (verdict, fix) = judge_with_rules(engine, &item, &class, &facts, now);
    CandidateJudgement {
        hashes,
        verdict,
        fix,
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
    // Every swarm hash, as ingest judges it, so `hash:` reads the same.
    let hashes = row_hashes(row);
    let item = Item {
        hashes: &hashes,
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
    if let Some(t) = row.info_hash_v2.map(|h| h.truncated())
        && t != row.info_hash
    {
        out.push(t.0);
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

    /// Stored torrent `id` held on `d` when its row won't read: its hashes
    /// as stored (`hashes`), and no sealed name, since none can be read.
    /// The torrent is deleted whatever `hashes` holds.
    pub fn unread(id: i64, hashes: Vec<[u8; 20]>, d: &Decision) -> Refiling {
        Refiling {
            id,
            hashes,
            rule: d.rule.id(),
            action: d.action.code(),
            held: match d.action {
                Action::Drop => Some(Held::Tombstone),
                _ => Some(Held::Quarantine),
            },
            sealed: None,
            evidence: d.evidence_codes(),
            flags: 0,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CoreRule;

    #[test]
    fn source_adult_adds_is_adult_unless_you_said_not_adult() {
        let mask = 1 << 4;
        let c = |adult, not_junk| Correction { adult, not_junk };
        // Cleared, or Not junk only: Is adult added.
        assert_eq!(
            source_adult(c(None, false), 1 << 4 | 1, mask),
            c(Some(true), false)
        );
        assert_eq!(
            source_adult(c(None, true), 1 << 4, mask),
            c(Some(true), true)
        );
        // Yours stands either way.
        assert_eq!(
            source_adult(c(Some(false), true), 1 << 4, mask),
            c(Some(false), true)
        );
        assert_eq!(
            source_adult(c(Some(true), false), 1 << 4, mask),
            c(Some(true), false)
        );
        // Not found by an always-adult source, or none registered.
        assert_eq!(source_adult(c(None, false), 1, mask), c(None, false));
        assert_eq!(source_adult(c(None, false), 1 << 4, 0), c(None, false));
    }

    #[test]
    fn stricter_keeps_the_heavier_decision_and_every_shadow() {
        let d = |action| Decision {
            rule: RuleRef::Core(CoreRule::Cs5a),
            action,
            evidence: vec![],
        };
        let s = stricter(
            Verdict {
                decision: Some(d(Action::Flag)),
                shadow: vec![d(Action::Hide)],
            },
            Verdict {
                decision: Some(d(Action::Quarantine)),
                shadow: vec![d(Action::Hide), d(Action::Drop)],
            },
        );
        assert_eq!(s.decision.unwrap().action, Action::Quarantine);
        assert_eq!(s.shadow.len(), 2);
        // A tie keeps the first's.
        let tie = stricter(
            Verdict {
                decision: Some(d(Action::Flag)),
                shadow: vec![],
            },
            Verdict::default(),
        );
        assert_eq!(tie.decision.unwrap().action, Action::Flag);
    }
}
