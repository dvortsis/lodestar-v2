//! Enrichment (architecture §6, §19.4): the release parse and the
//! classification of a torrent, from its name and file list, in the form
//! the store keeps (`Enriched`). Workers run it in the prepare step; the
//! coordinator runs it for torrents that arrive without it (imports,
//! torrents stored before M3, records from an older parser).

use lodestar_classify::{Classification, Correction, ENRICHMENT_VERSION, FileInfo};
use lodestar_filter::Handle;
use lodestar_metainfo::{File, TextEncoding};
use lodestar_store::{Enriched, Store};

pub use lodestar_filter::reclassify::{enrich_corrected, enrich_row};

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
    enrich_stored_with(store, after, limit, None)
}

/// [`enrich_stored`], applying your classification rules from `filter`
/// too. A torrent's own correction wins over a rule. Nothing is written
/// if the rules changed during the batch (§20.9): their change marks the
/// torrents again, and the next pass picks them up.
pub fn enrich_stored_with(
    store: &mut Store,
    after: i64,
    limit: usize,
    filter: Option<&Handle>,
) -> Result<Option<(i64, usize)>, lodestar_store::Error> {
    // The generation first, then the rules: rules read after a change
    // are never older than the generation they're written under.
    let generation = store.class_gen()?;
    let engine = filter.map(Handle::current);
    let ids = store.unenriched(after, ENRICHMENT_VERSION, limit)?;
    let Some(&last) = ids.last() else {
        return Ok(None);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let mut batch = Vec::with_capacity(ids.len());
    for id in ids {
        if let (Some(row), Some(tree)) = (store.torrent(id)?, store.files(id)?) {
            let own = store.correction(id)?.map(correction).unwrap_or_default();
            let files = file_infos(&tree.files, tree.encoding);
            batch.push((id, enrich_row(&row, &files, own, engine.as_deref(), now).0));
        }
    }
    let n = store
        .set_enrichments_at(generation, &batch)?
        .unwrap_or_default();
    Ok(Some((last, n)))
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
