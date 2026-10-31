//! Enrichment (architecture §6, §19.4): the release parse and the
//! classification of a torrent, from its name and file list, in the form
//! the store keeps (`Enriched`). Workers run it in the prepare step; the
//! coordinator runs it for torrents that arrive without it (imports,
//! torrents stored before M3, records from an older parser).

use lodestar_classify::ContentType;
use lodestar_classify::{Classification, Correction, ENRICHMENT_VERSION, FileInfo};
use lodestar_filter::reclassify::reclassify_row;
use lodestar_filter::{Handle, Sealer};
use lodestar_metainfo::{File, TextEncoding};
use lodestar_store::{Enriched, Store};

pub use lodestar_filter::reclassify::enrich_corrected;

pub use lodestar_classify::file_infos;

/// The enrichment of a torrent with this name and file list.
pub fn enrich(name: &str, files: &[File], encoding: TextEncoding) -> Enriched {
    enrich_infos(name, &file_infos(files, encoding)).0
}

/// The enrichment of a torrent from its decoded file list, and the
/// classification it came from (for the filter).
pub fn enrich_infos(name: &str, files: &[FileInfo]) -> (Enriched, Classification) {
    enrich_corrected(name, files, Correction::default())
}

/// Enriches up to `limit` stored torrents after `after` (by id) whose
/// enrichment is missing or outdated, and logs them for re-indexing.
/// Returns the last id looked at and how many were enriched, or `None`
/// when none are left.
pub fn enrich_stored(
    store: &mut Store,
    after: i64,
    limit: usize,
) -> Result<Option<(i64, usize)>, lodestar_store::Error> {
    loop {
        match enrich_stored_with(store, after, limit, &Backfill::default())? {
            None => return Ok(None),
            // The rules changed under the batch: do it again.
            Some(b) if b.rejected => continue,
            Some(b) => return Ok(Some((b.last, b.enriched))),
        }
    }
}

/// What background enrichment applies besides detection (§20.9).
#[derive(Debug, Clone, Copy, Default)]
pub struct Backfill<'a> {
    /// Your classification rules, and the filter that judges a torrent
    /// they (or its own correction) relabel adult.
    pub filter: Option<&'a Handle>,
    /// Seals the name of a torrent that judgement quarantines.
    pub sealer: Option<&'a Sealer>,
    /// How long such a quarantine lasts.
    pub quarantine_secs: i64,
}

/// One batch of background enrichment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Backfilled {
    /// The last id looked at: the next batch starts after it.
    pub last: i64,
    pub enriched: usize,
    /// Swarm hashes of torrents relabelled adult and then held back
    /// (dropped or quarantined): intake must refuse them now.
    pub held: Vec<[u8; 20]>,
    /// The classification rules changed during the batch, so nothing was
    /// written: run it again from the same id.
    pub rejected: bool,
}

/// [`enrich_stored`] with your classification rules and corrections. A
/// torrent's own correction wins over a rule. A torrent that becomes
/// adult (by them or a newer parser) is judged as adult too, and its
/// decision applied as at ingest, in the same transaction (§20.9).
/// Without a filter, Is adult corrections and rules don't apply. Nothing
/// is written if the rules changed during the batch.
pub fn enrich_stored_with(
    store: &mut Store,
    after: i64,
    limit: usize,
    ctx: &Backfill<'_>,
) -> Result<Option<Backfilled>, lodestar_store::Error> {
    // The generation first, then the rules: rules read after a change
    // are never older than the generation they're written under.
    let generation = store.class_gen()?;
    let engine = ctx.filter.map(Handle::current);
    let ids = store.unenriched(after, ENRICHMENT_VERSION, limit)?;
    let Some(&last) = ids.last() else {
        return Ok(None);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let mut batch = Vec::with_capacity(ids.len());
    let mut refilings = Vec::new();
    for id in ids {
        if let (Some(row), Some(tree)) = (store.torrent(id)?, store.files(id)?) {
            let own = store.correction(id)?.map(correction).unwrap_or_default();
            let was_adult = store.content_type(id)? == Some(ContentType::Adult.code());
            let files = file_infos(&tree.files, tree.encoding);
            let r = reclassify_row(&row, &files, own, was_adult, engine.as_deref(), now);
            refilings.extend(r.refilings(&row, ctx.sealer));
            batch.push((id, r.enriched));
        }
    }
    let refiled: Vec<_> = refilings
        .iter()
        .map(|r| r.refiled(now, now + ctx.quarantine_secs))
        .collect();
    let Some(enriched) = store.set_enrichments_at(generation, &batch, &refiled)? else {
        return Ok(Some(Backfilled {
            last: after,
            rejected: true,
            ..Backfilled::default()
        }));
    };
    let held = refilings
        .iter()
        .filter(|r| r.held.is_some())
        .flat_map(|r| r.hashes.iter().copied())
        .collect();
    Ok(Some(Backfilled {
        last,
        enriched,
        held,
        rejected: false,
    }))
}

/// One batch of the import-candidate refile sweep
/// (`docs/decisions/2026-10-31-B-import-refile.md`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidatesRefiled {
    /// The last candidate id looked at: the next batch starts after it.
    pub last: i64,
    /// Candidates a rule now holds back, removed and counted.
    pub held: usize,
    /// Their swarm hashes: intake must refuse them now.
    pub hashes: Vec<[u8; 20]>,
    /// The engine changed during the batch, so nothing was written: start
    /// the sweep again under the new rules.
    pub rejected: bool,
}

/// Judges up to `limit` waiting import candidates after `after` (by id)
/// with the current engine, as an import judges a candidate (the hash
/// lists, then its name with your classification rules), and holds back
/// those a drop or quarantine decision now matches (§20.3, §20.9). Other
/// decisions change nothing: a candidate has no marks, and it's judged
/// again in full if it's ever stored. `None` when none are left.
///
/// `version` is the engine version the sweep runs under; the batch is
/// judged with the engine only when it's still that version, and its
/// write is refused if the engine moves on before it commits.
pub fn refile_candidates(
    store: &mut Store,
    handle: &Handle,
    version: u64,
    after: i64,
    limit: usize,
    ctx: &Backfill<'_>,
) -> Result<Option<CandidatesRefiled>, lodestar_store::Error> {
    let rejected = || {
        Ok(Some(CandidatesRefiled {
            last: after,
            rejected: true,
            ..CandidatesRefiled::default()
        }))
    };
    let engine = handle.current();
    if engine.version != version {
        return rejected();
    }
    let page = store.waiting_candidates(after, limit)?;
    let Some(last) = page.last().map(|c| c.id) else {
        return Ok(None);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let held: Vec<CandidateHold> = page
        .iter()
        .filter_map(|c| judge_candidate(&engine, c, ctx.sealer, now))
        .collect();
    let refiled: Vec<_> = held
        .iter()
        .map(|h| lodestar_store::import::RefiledCandidate {
            id: h.id,
            filtered: lodestar_store::Filtered {
                hashes: &h.hashes,
                rule: &h.rule,
                action: h.action,
                held: Some(h.held),
                sealed: h.sealed.as_deref(),
                evidence: &h.evidence,
                at: now,
                expires_at: now + ctx.quarantine_secs,
                adult_hold: h.adult_hold,
            },
        })
        .collect();
    let written = if refiled.is_empty() {
        Some(0)
    } else {
        store.refile_import_candidates(&refiled, now, || handle.current().version == version)?
    };
    let Some(n) = written else {
        return rejected();
    };
    Ok(Some(CandidatesRefiled {
        last,
        held: n,
        hashes: held.into_iter().flat_map(|h| h.hashes).collect(),
        rejected: false,
    }))
}

/// A candidate's hold, owned until the batch is written. No name in the
/// clear: only the sealed one, for quarantine.
struct CandidateHold {
    id: i64,
    hashes: Vec<[u8; 20]>,
    rule: String,
    action: u8,
    held: lodestar_store::Held,
    sealed: Option<Vec<u8>>,
    evidence: Vec<u8>,
    adult_hold: bool,
}

/// What the engine makes of one waiting candidate, as the import driver
/// judges it (`sources-importers`, `judge_candidate`): a hash on the
/// floor's or a dropping list first, then the name, with no files.
fn judge_candidate(
    engine: &lodestar_filter::Engine,
    c: &lodestar_store::import::WaitingCandidate,
    sealer: Option<&Sealer>,
    now: i64,
) -> Option<CandidateHold> {
    use lodestar_filter::{Action, Facts, Item};
    let hashes = c.hashes();
    let name = c
        .name
        .as_deref()
        .map(String::from_utf8_lossy)
        .unwrap_or_default();
    let (decision, fix) = match engine.blocked_hash(&hashes) {
        Some(d) => (Some(d), None),
        None => {
            let (_, class) = enrich_corrected(&name, &[], Correction::default());
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
                name: &name,
                files: &[],
                content_type: class.content_type,
            };
            let (v, says) = engine.judge_classified(&item, Some(&facts), now);
            (v.decision, rule_fix(says, &class))
        }
    };
    let d = decision?;
    let held = match d.action {
        Action::Drop => lodestar_store::Held::Tombstone,
        Action::Quarantine => lodestar_store::Held::Quarantine,
        _ => return None,
    };
    let sealed = (held == lodestar_store::Held::Quarantine)
        .then(|| sealer.and_then(|s| s.seal(&name).ok()))
        .flatten();
    Some(CandidateHold {
        id: c.id,
        hashes,
        rule: d.rule.id(),
        action: d.action.code(),
        held,
        sealed,
        evidence: d.evidence_codes(),
        // Held by the Adult-only core only because your rule made it
        // adult: a tombstone when the quarantine ends (§20.9).
        adult_hold: fix == Some(true) && lodestar_filter::reclassify::adult_hold(&d),
    })
}

/// What a classification rule's say changes: Is adult (`true`) or Not
/// adult (`false`) where it differs from detection, else nothing.
pub fn rule_fix(says: Option<bool>, class: &Classification) -> Option<bool> {
    says.filter(|adult| *adult != (class.content_type == lodestar_classify::ContentType::Adult))
}

/// The stored enrichment with a classification rule's fix (or detection's
/// own when `None`).
pub fn enrich_with_rule(name: &str, files: &[FileInfo], fix: Option<bool>) -> Enriched {
    let c = Correction {
        adult: fix,
        not_junk: false,
    };
    enrich_corrected(name, files, c).0
}

/// A stored correction as the classifier takes it.
pub fn correction(c: lodestar_store::ClassCorrection) -> Correction {
    Correction {
        adult: c.adult,
        not_junk: c.not_junk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_classify::{ContentType, Record};
    use lodestar_metainfo::FileAttrs;

    fn file(path: &[&[u8]], size: u64) -> File {
        File {
            path: path.iter().map(|s| s.to_vec()).collect(),
            size,
            attrs: FileAttrs::empty(),
            symlink_target: None,
            pieces_root: None,
            hashes: Default::default(),
        }
    }

    #[test]
    fn enriches_from_decoded_paths() {
        let files = [
            file(&[b"Show.S01E01.1080p.mkv"], 900),
            file(&[b"Show.S01E02.1080p.mkv"], 800),
        ];
        let e = enrich("a8f3x", &files, TextEncoding::UTF8);
        assert_eq!(e.version, ENRICHMENT_VERSION);
        assert_eq!(
            ContentType::from_code(e.content_type),
            Some(ContentType::Tv)
        );
        let r = Record::decode(&e.data).unwrap();
        assert_eq!(r.release("title"), Some("Show"));
        assert!(r.ambiguous);
        assert_eq!(r.release("pack"), Some("yes"));
    }
}
