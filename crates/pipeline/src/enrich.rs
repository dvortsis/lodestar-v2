//! Enrichment (architecture §6, §19.4): the release parse and the
//! classification of a torrent, from its name and file list, in the form
//! the store keeps (`Enriched`). Workers run it in the prepare step; the
//! coordinator runs it for torrents that arrive without it (imports,
//! torrents stored before M3, records from an older parser).

use lodestar_classify::{Classification, Correction, ENRICHMENT_VERSION, FileInfo};
use lodestar_filter::Engine;
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

/// [`enrich_stored`], applying your classification rules from `engine`
/// too. A torrent's own correction wins over a rule.
pub fn enrich_stored_with(
    store: &mut Store,
    after: i64,
    limit: usize,
    engine: Option<&Engine>,
) -> Result<Option<(i64, usize)>, lodestar_store::Error> {
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
            batch.push((id, enrich_row(&row, &files, own, engine, now).0));
        }
    }
    let n = store.set_enrichments(&batch)?;
    Ok(Some((last, n)))
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
