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
