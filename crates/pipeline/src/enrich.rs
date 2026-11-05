//! Enrichment (architecture §6, §19.4): the release parse and the
//! classification of a torrent, from its name and file list, in the form
//! the store keeps (`Enriched`). Workers run it in the prepare step; the
//! coordinator runs it for torrents that arrive without it (imports,
//! torrents stored before M3, records from an older parser).

use lodestar_classify::{
    Classification, ContentType, Correction, ENRICHMENT_VERSION, EnrichmentMatch, FileInfo,
    MatchKind, Record, classify_enriched,
};
use lodestar_filter::reclassify::{Reclassified, reclassify_row};
use lodestar_filter::{Handle, Sealer};
use lodestar_metainfo::{File, TextEncoding};
use lodestar_store::claims::{AdultEnrichment, AdultKind};
use lodestar_store::{Enriched, Store, TorrentRow};

use crate::source_adult::AdultSources;

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
    /// The sources whose every find is adult: a torrent one of them found
    /// is classified as with an Is adult correction, unless its own
    /// correction says Not adult ([`crate::source_adult`]).
    pub adult: crate::source_adult::AdultSources,
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
    enrich_stored_marking(store, after, limit, ctx, &|_, _| {})
}

/// [`enrich_stored_with`], calling `mark` with the swarm hashes the batch
/// will hold just before its store write, so intake can refuse them from
/// before the commit (`2026-10-31-B-held-precommit.md`).
pub fn enrich_stored_marking(
    store: &mut Store,
    after: i64,
    limit: usize,
    ctx: &Backfill<'_>,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Option<Backfilled>, lodestar_store::Error> {
    // The generation first, then the rules: rules read after a change
    // are never older than the generation they're written under.
    let generation = store.class_gen()?;
    let engine = ctx.filter.map(Handle::current);
    let ids = store.unenriched(after, ENRICHMENT_VERSION, limit)?;
    let Some(&last) = ids.last() else {
        return Ok(None);
    };
    let done = enrich_ids(store, generation, engine.as_deref(), &ids, ctx, mark)?;
    Ok(Some(Backfilled {
        // Rejected: the same batch again.
        last: if done.rejected { after } else { last },
        ..done
    }))
}

/// Classifies stored torrents `ids` again under generation `generation`,
/// as the backfill does, and writes them in one transaction. `last` is
/// left 0 for the caller to set.
fn enrich_ids(
    store: &mut Store,
    generation: i64,
    engine: Option<&lodestar_filter::Engine>,
    ids: &[i64],
    ctx: &Backfill<'_>,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Backfilled, lodestar_store::Error> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let mut batch = Vec::with_capacity(ids.len());
    let mut refilings = Vec::new();
    for &id in ids {
        if let (Some(row), Some(tree)) = (store.torrent(id)?, store.files(id)?) {
            let own = store.correction(id)?.map(correction).unwrap_or_default();
            // An always-adult source among its finders: Is adult, unless
            // your own correction says otherwise.
            let own = crate::source_adult::correction(own, row.sources, ctx.adult);
            let was_adult = store.content_type(id)? == Some(ContentType::Adult.code());
            let files = file_infos(&tree.files, tree.encoding);
            let r = reclassify_row(&row, &files, own, was_adult, engine, now);
            refilings.extend(r.refilings(&row, ctx.sealer));
            // A stored enrichment match pins the adult kind (ENR-1): a
            // newer parser keeps it.
            let enriched = match with_match(store, &row, &files, own, &r)? {
                Some(e) => e,
                None => r.enriched,
            };
            batch.push((id, enriched));
        }
    }
    let refiled: Vec<_> = refilings
        .iter()
        .map(|r| r.refiled(now, now + ctx.quarantine_secs))
        .collect();
    let held: Vec<[u8; 20]> = refilings
        .iter()
        .filter(|r| r.held.is_some())
        .flat_map(|r| r.hashes.iter().copied())
        .collect();
    if !held.is_empty() {
        mark(store, &held);
    }
    let Some(enriched) = store.set_enrichments_at(generation, &batch, &refiled)? else {
        return Ok(Backfilled {
            rejected: true,
            ..Backfilled::default()
        });
    };
    Ok(Backfilled {
        last: 0,
        enriched,
        held,
        rejected: false,
    })
}

/// Classifies again, at once, the stored torrents among `ids` that an
/// always-adult source has just found and that aren't stored as adult
/// yet (X-source-adult): Is adult unless your own correction says Not
/// adult, judged as adult as at ingest, in one transaction. The rest are
/// left alone. When the rules changed under it, nothing is written and
/// the torrents are marked for the backfill instead, which applies the
/// same correction (`rejected` is set: the caller starts that pass).
pub fn adult_by_source(
    store: &mut Store,
    ids: &[i64],
    ctx: &Backfill<'_>,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Backfilled, lodestar_store::Error> {
    if ctx.adult.is_empty() || ids.is_empty() {
        return Ok(Backfilled::default());
    }
    let generation = store.class_gen()?;
    let engine = ctx.filter.map(Handle::current);
    let mut due = Vec::new();
    for &id in ids {
        if due.contains(&id) || store.content_type(id)? == Some(ContentType::Adult.code()) {
            continue;
        }
        let Some(row) = store.torrent(id)? else {
            continue;
        };
        let own = store.correction(id)?.map(correction).unwrap_or_default();
        if crate::source_adult::correction(own, row.sources, ctx.adult).adult == Some(true) {
            due.push(id);
        }
    }
    if due.is_empty() {
        return Ok(Backfilled::default());
    }
    let done = enrich_ids(store, generation, engine.as_deref(), &due, ctx, mark)?;
    if done.rejected {
        store.mark_unenriched(&due)?;
    }
    Ok(done)
}

/// The enrichment match an adult-enrichment section gives classification
/// (ENR-6's [`EnrichmentMatch`]): the kind the service reports. A section
/// without a kind pins nothing.
pub fn match_of(a: &AdultEnrichment) -> Option<EnrichmentMatch> {
    let kind = match a.kind? {
        AdultKind::Scene => MatchKind::Scene,
        AdultKind::Movie => MatchKind::Movie,
        AdultKind::Vr => MatchKind::Vr,
    };
    Some(EnrichmentMatch::new(kind))
}

/// The enrichment match stored on torrent `id`: the first, by source, of
/// its claims' adult sections that names a kind. `unverified::ingest`
/// keeps a section only on a stored torrent and never through a private
/// source; it doesn't yet check that the source is an enrichment plugin
/// (`docs/decisions/2026-11-05-ENR-1.md`).
pub fn stored_match(
    store: &Store,
    id: i64,
) -> Result<Option<EnrichmentMatch>, lodestar_store::Error> {
    Ok(store
        .torrent_claims(id)?
        .iter()
        .filter_map(|c| c.claims.as_ref()?.adult.as_ref())
        .find_map(match_of))
}

/// `r`'s enrichment with the torrent's stored match applied through
/// ENR-6's entry point ([`classify_enriched`]), when `r` is adult and its
/// claims hold one; `None` otherwise. Only the adult kind and confidence
/// can change: a match never makes an item adult here, never un-adults
/// one, and filtering never judges this result (§20.7).
fn with_match(
    store: &Store,
    row: &TorrentRow,
    files: &[FileInfo],
    own: Correction,
    r: &Reclassified,
) -> Result<Option<Enriched>, lodestar_store::Error> {
    if r.class.content_type != ContentType::Adult {
        return Ok(None);
    }
    let Some(m) = stored_match(store, row.id)? else {
        return Ok(None);
    };
    // The correction (or rule, or always-adult source) that made it adult,
    // so its confidence stays 100; else detection's, with the match.
    let fix = Correction {
        adult: (own.adult == Some(true) || r.relabelled).then_some(true),
        not_junk: own.not_junk,
    };
    Ok(Some(enriched_of(&classify_enriched(
        &row.name,
        files,
        fix,
        Some(m),
    ))))
}

/// Marks for the backfill those of the stored torrents `ids` whose claims
/// hold an adult-enrichment section, so their match is applied again.
/// For a commit that rewrote a torrent's classification without reading
/// its claims: a partial import's upgrade writes prepare's record, which
/// knows no match (ENR-1b). Returns the ids marked.
pub fn mark_matched(store: &mut Store, ids: &[i64]) -> Result<Vec<i64>, lodestar_store::Error> {
    let mut marked = Vec::new();
    for &id in ids {
        if !marked.contains(&id) && stored_match(store, id)?.is_some() {
            marked.push(id);
        }
    }
    if !marked.is_empty() {
        store.mark_unenriched(&marked)?;
    }
    Ok(marked)
}

/// A classification in the form the store keeps.
fn enriched_of(c: &Classification) -> Enriched {
    Enriched {
        version: ENRICHMENT_VERSION,
        content_type: c.content_type.code(),
        junk_score: c.junk.score,
        comp: c.composition.encode(),
        data: Record::new(c).encode(),
    }
}

/// What [`apply_matches`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Matched {
    /// Torrents classified again with their match.
    pub classified: usize,
    /// Torrents marked for the backfill instead: one not stored as adult
    /// yet (becoming adult needs the backfill's judgement and holds), or
    /// a batch the classification rules moved under.
    pub deferred: Vec<i64>,
}

/// Classifies again, at once, the stored torrents among `ids` that just
/// gained an enrichment match (ENR-1), so the match pins their adult kind.
///
/// Only a torrent stored as adult is classified here: classifying it
/// again can't make it adult, so nothing new needs judging or holding,
/// and the write holds nothing. Any other is marked for the backfill,
/// which judges and holds as usual. A torrent whose classification is no
/// longer adult (your Not adult correction or rule since), or whose
/// claims no longer hold a match, is left alone. When the rules changed
/// under the batch, it's all marked for the backfill instead.
pub fn apply_matches(
    store: &mut Store,
    ids: &[i64],
    filter: &Handle,
    adult: AdultSources,
    now: i64,
) -> Result<Matched, lodestar_store::Error> {
    // The generation first, then the rules (as the backfill reads them):
    // rules read after a change are never older than the generation they're
    // written under, so a write can't clear a mark a rules change set.
    let generation = store.class_gen()?;
    #[cfg(test)]
    if let Some(between) = tests::BETWEEN_READS.with(|b| b.borrow_mut().take()) {
        between(store);
    }
    let engine = filter.current();
    let engine = &*engine;
    let mut batch: Vec<(i64, Enriched)> = Vec::new();
    let mut deferred = Vec::new();
    for &id in ids {
        if batch.iter().any(|(b, _)| *b == id) || deferred.contains(&id) {
            continue;
        }
        let (Some(row), Some(tree)) = (store.torrent(id)?, store.files(id)?) else {
            continue;
        };
        if store.content_type(id)? != Some(ContentType::Adult.code()) {
            deferred.push(id);
            continue;
        }
        let own = store.correction(id)?.map(correction).unwrap_or_default();
        let own = crate::source_adult::correction(own, row.sources, adult);
        let files = file_infos(&tree.files, tree.encoding);
        // Stored as adult, so it's judged as adult already: nothing to hold.
        let r = reclassify_row(&row, &files, own, true, Some(engine), now);
        if let Some(e) = with_match(store, &row, &files, own, &r)? {
            batch.push((id, e));
        }
    }
    if !deferred.is_empty() {
        store.mark_unenriched(&deferred)?;
    }
    if batch.is_empty() {
        return Ok(Matched {
            classified: 0,
            deferred,
        });
    }
    match store.set_enrichments_at(generation, &batch, &[])? {
        Some(classified) => Ok(Matched {
            classified,
            deferred,
        }),
        None => {
            let ids: Vec<i64> = batch.iter().map(|(id, _)| *id).collect();
            store.mark_unenriched(&ids)?;
            deferred.extend(ids);
            Ok(Matched {
                classified: 0,
                deferred,
            })
        }
    }
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
    refile_candidates_marking(store, handle, version, after, limit, ctx, &|_, _| {})
}

/// [`refile_candidates`], calling `mark` with the swarm hashes the batch
/// will hold just before its store write (`2026-10-31-B-held-precommit.md`).
pub fn refile_candidates_marking(
    store: &mut Store,
    handle: &Handle,
    version: u64,
    after: i64,
    limit: usize,
    ctx: &Backfill<'_>,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
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
        Some(Vec::new())
    } else {
        let marked: Vec<[u8; 20]> = held.iter().flat_map(|h| h.hashes.iter().copied()).collect();
        mark(store, &marked);
        store.refile_import_candidates(&refiled, now, || handle.current().version == version)?
    };
    let Some(ids) = written else {
        return rejected();
    };
    // Only the candidates the store held back: one already gone has no
    // hold behind it for intake to honour.
    let hashes = held
        .into_iter()
        .filter(|h| ids.contains(&h.id))
        .flat_map(|h| h.hashes)
        .collect();
    Ok(Some(CandidatesRefiled {
        last,
        held: ids.len(),
        hashes,
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

/// What the engine makes of one waiting candidate: the judgement the
/// import driver makes too (`lodestar_filter::reclassify::judge_candidate`),
/// kept only when it drops or quarantines.
fn judge_candidate(
    engine: &lodestar_filter::Engine,
    c: &lodestar_store::import::WaitingCandidate,
    sealer: Option<&Sealer>,
    now: i64,
) -> Option<CandidateHold> {
    use lodestar_filter::Action;
    use lodestar_filter::reclassify::{Candidate, adult_hold, judge_candidate};
    let name = c
        .name
        .as_deref()
        .map(String::from_utf8_lossy)
        .unwrap_or_default();
    let j = judge_candidate(
        engine,
        Candidate {
            hash: c.hash.0,
            hash_v2: c.hash_v2.map(|h| h.0),
            name: &name,
            size: c.size,
            source_at: c.source_at,
        },
        now,
    );
    let d = j.verdict.decision?;
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
        hashes: j.hashes,
        rule: d.rule.id(),
        action: d.action.code(),
        held,
        sealed,
        evidence: d.evidence_codes(),
        // Held by the Adult-only core only because your rule made it
        // adult: a tombstone when the quarantine ends (§20.9).
        adult_hold: j.fix == Some(true) && adult_hold(&d),
    })
}

pub use lodestar_filter::reclassify::rule_fix;

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

    type Between = Box<dyn FnOnce(&mut Store)>;

    thread_local! {
        /// Runs once inside `apply_matches`, between its generation read
        /// and its rules read: the race the order closes.
        pub(super) static BETWEEN_READS: std::cell::RefCell<Option<Between>> =
            const { std::cell::RefCell::new(None) };
    }

    /// A rules change that lands between `apply_matches`'s two reads, in
    /// the API's order (the live rules, then the generation and the mark),
    /// isn't written over: the match is classified under the new rules, so
    /// the row stays marked. With the reads swapped it would be classified
    /// under the old rules and written under the new generation, clearing
    /// the mark.
    #[test]
    fn a_rules_change_between_the_reads_is_not_written_over() {
        use lodestar_bencode::{OwnedValue, to_vec};
        use lodestar_store::claims::{AdultEnrichment, AdultKind, Claims};
        use lodestar_store::{NewTorrent, StoreConfig};
        let name = "Studiox Garden Scene XXX 1080p";
        let raw = to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(900 << 20)),
                (b"name".to_vec(), OwnedValue::from(name)),
                (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
                (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20 * 900])),
            ]
            .into_iter()
            .collect(),
        ));
        let info = lodestar_metainfo::Info::parse(&raw).unwrap();
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let dht = store.register_source("dht", "1", 0).unwrap();
        let enr = store.register_source("enr", "1", 0).unwrap();
        let id = store
            .insert(&[NewTorrent {
                info: &info,
                raw_info: None,
                discovered_at: 0,
                metadata_at: 0,
                extra_flags: 0,
                source: dht,
            }])
            .unwrap()[0]
            .id();
        let files = file_infos(&info.files, info.path_encoding);
        store
            .set_enrichments(&[(id, enrich_infos(name, &files).0)])
            .unwrap();
        assert_eq!(
            store.content_type(id).unwrap(),
            Some(ContentType::Adult.code())
        );
        let section = AdultEnrichment {
            kind: Some(AdultKind::Vr),
            ..AdultEnrichment::new("1")
        };
        let claims = Claims {
            adult: Some(section),
            ..Claims::default()
        };
        assert!(store.put_claims(id, enr, &claims, false).unwrap());
        let filter = std::sync::Arc::new(crate::default_filter());
        let live = filter.clone();
        BETWEEN_READS.with(|b| {
            *b.borrow_mut() = Some(Box::new(move |store: &mut Store| {
                let rule = store.add_class_rule("garden", false).unwrap();
                live.update_engine(|e| {
                    assert!(
                        e.load_class_rules(&[(rule, "garden".into(), false)])
                            .is_empty()
                    );
                });
                store.bump_class_gen().unwrap();
                store.mark_unenriched(&[id]).unwrap();
            }))
        });
        apply_matches(&mut store, &[id], &filter, AdultSources::default(), 0).unwrap();
        assert!(BETWEEN_READS.with(|b| b.borrow().is_none()), "the hook ran");
        let e = store.enrichment(id).unwrap().unwrap();
        assert_ne!(e.version, ENRICHMENT_VERSION, "still marked");
        let r = Record::decode(&e.data).unwrap();
        assert_ne!(r.adult_kind, Some(lodestar_classify::AdultKind::Vr));
    }

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
