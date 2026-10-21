//! Enrichment (architecture §6, §19.4): the release parse and the
//! classification of a torrent, from its name and file list, in the form
//! the store keeps (`Enriched`). Workers run it in the prepare step; the
//! coordinator runs it for torrents that arrive without it (imports,
//! torrents stored before M3, records from an older parser).

use lodestar_classify::{ENRICHMENT_VERSION, FileInfo, Record, classify};
use lodestar_metainfo::{File, Text, TextEncoding};
use lodestar_store::Enriched;

/// The file list with decoded paths, as classification takes it.
pub(crate) fn file_infos(files: &[File], encoding: TextEncoding) -> Vec<FileInfo> {
    files
        .iter()
        .map(|f| FileInfo {
            path: f
                .path
                .iter()
                .map(|seg| {
                    Text {
                        raw: seg.clone(),
                        encoding,
                    }
                    .decode()
                    .into_owned()
                })
                .collect(),
            size: f.size,
            padding: f.is_padding(),
        })
        .collect()
}

pub(crate) fn enrich(name: &str, files: &[File], encoding: TextEncoding) -> Enriched {
    let files = file_infos(files, encoding);
    let c = classify(name, &files);
    Enriched {
        version: ENRICHMENT_VERSION,
        content_type: c.content_type.code(),
        junk_score: c.junk.score,
        comp: c.composition.encode(),
        data: Record::new(&c, &files).encode(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_classify::ContentType;
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
