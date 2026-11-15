//! What an index directory holds on disk, against what its live segments
//! use: dev tooling for the size budget (§3), read by `size-report` and
//! `index-stats` (bench). A `du` of a working index can count segments a
//! merge has replaced but garbage collection hasn't removed yet, and
//! deleted documents still waiting for a merge; this tells them apart.
//!
//! It opens the index read-only (a reader, no writer, no lock), so it can
//! read a copy of a served index, or one a stopped server left.

use std::collections::BTreeSet;
use std::path::Path;

use tantivy::{Index, IndexReader, ReloadPolicy};

use crate::index::{Error, Result};

/// One index directory's segments and bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskStats {
    /// Live segments in the last commit.
    pub segments: usize,
    /// Documents the live segments hold, deleted ones excluded.
    pub docs: u64,
    /// Deleted documents still in live segments, until a merge drops them.
    pub deleted: u64,
    /// Bytes the live segments use, by Tantivy's own accounting.
    pub live_bytes: u64,
    /// Bytes of the files directly in the directory: what `size-report`
    /// counts.
    pub top_level_bytes: u64,
    /// Bytes of every file under the directory, subdirectories included:
    /// what `du` counts.
    pub recursive_bytes: u64,
    /// Bytes of top-level segment files no live segment owns: merged away
    /// and not yet garbage-collected.
    pub stale_bytes: u64,
    /// The stale files' names, for the report.
    pub stale_files: Vec<String>,
}

impl DiskStats {
    /// Reads `dir`, a Tantivy index directory (the torrent index's
    /// `v{schema}-t{tokenizer}`, or its `files`).
    pub fn read(dir: &Path) -> Result<DiskStats> {
        let reader = open_reader(dir)?;
        let searcher = reader.searcher();
        let mut live = BTreeSet::new();
        let (mut docs, mut deleted, mut live_bytes) = (0u64, 0u64, 0u64);
        for seg in searcher.segment_readers() {
            live.insert(seg.segment_id().uuid_string());
            docs += u64::from(seg.num_docs());
            deleted += u64::from(seg.num_deleted_docs());
            live_bytes += seg.space_usage().map_err(Error::Io)?.total().get_bytes();
        }
        let (mut top_level_bytes, mut stale_bytes, mut stale_files) = (0, 0, Vec::new());
        for entry in std::fs::read_dir(dir).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            let meta = entry.metadata().map_err(Error::Io)?;
            if !meta.is_file() {
                continue;
            }
            top_level_bytes += meta.len();
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = segment_id(&name)
                && !live.contains(id)
            {
                stale_bytes += meta.len();
                stale_files.push(name);
            }
        }
        stale_files.sort();
        Ok(DiskStats {
            segments: searcher.segment_readers().len(),
            docs,
            deleted,
            live_bytes,
            top_level_bytes,
            recursive_bytes: recursive_size(dir)?,
            stale_bytes,
            stale_files,
        })
    }
}

/// Bytes by (component, field) across `dir`'s live segments, largest
/// first, read-only: [`SearchIndex::space_by_field`]'s table for any
/// index directory.
///
/// [`SearchIndex::space_by_field`]: crate::SearchIndex::space_by_field
pub fn space_by_field_at(dir: &Path) -> Result<Vec<(String, u64)>> {
    crate::index::space_by_field(&open_reader(dir)?)
}

fn open_reader(dir: &Path) -> Result<IndexReader> {
    let index = Index::open_in_dir(dir)?;
    Ok(index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?)
}

/// The segment a top-level file belongs to: Tantivy names segment files
/// `{32 hex digits}.{component}` (and `.{opstamp}.del` for deletes).
/// Anything else (`meta.json`, `.managed.json`, locks) is no segment's.
fn segment_id(name: &str) -> Option<&str> {
    let (id, _) = name.split_once('.')?;
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(id)
}

fn recursive_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let meta = entry.metadata().map_err(Error::Io)?;
        if meta.is_dir() {
            total += recursive_size(&entry.path())?;
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchIndex;
    use tantivy::Term;

    #[test]
    fn counts_segments_deletes_and_stale_files_of_a_real_index() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = crate::index_dir(tmp.path());
        let index = SearchIndex::open(&dir).unwrap();
        // Two segments of 10 torrents, never merged, then one delete.
        crate::testing::append(&index, 1..11, 20);
        crate::testing::append(&index, 11..21, 20);
        crate::testing::delete(&index, [3]);

        let s = DiskStats::read(&dir).unwrap();
        assert_eq!((s.segments, s.docs, s.deleted), (2, 19, 1));
        assert!(0 < s.live_bytes && s.live_bytes <= s.top_level_bytes, "{s:?}");
        // The file index lives in a subdirectory: `du` counts it, the
        // top level doesn't.
        assert!(s.top_level_bytes < s.recursive_bytes, "{s:?}");
        assert_eq!((s.stale_bytes, s.stale_files.len()), (0, 0), "{s:?}");
        let parts = space_by_field_at(&dir).unwrap();
        assert!(parts.iter().any(|(p, b)| p == "postings name" && *b > 0), "{parts:?}");

        // A segment file no live segment owns is stale; `meta.json` never is.
        let orphan = "0123456789abcdef0123456789abcdef.idx";
        std::fs::write(dir.join(orphan), [0u8; 100]).unwrap();
        let s = DiskStats::read(&dir).unwrap();
        assert_eq!((s.stale_bytes, s.stale_files), (100, vec![orphan.to_owned()]));
        std::fs::remove_file(dir.join(orphan)).unwrap();

        // Merged into one, the delete is gone with the old segments.
        let (raw, _) = index.raw();
        let mut w: tantivy::IndexWriter = raw.writer_with_num_threads(1, 15 << 20).unwrap();
        let ids = raw.searchable_segment_ids().unwrap();
        w.merge(&ids).wait().unwrap();
        w.delete_term(Term::from_field_u64(index.fields().id, 999));
        w.commit().unwrap();
        w.wait_merging_threads().unwrap();
        let s = DiskStats::read(&dir).unwrap();
        assert_eq!((s.segments, s.docs, s.deleted), (1, 19, 0), "{s:?}");
    }

    #[test]
    fn segment_files_are_told_from_the_rest() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(segment_id(&format!("{id}.idx")), Some(id));
        assert_eq!(segment_id(&format!("{id}.12.del")), Some(id));
        for other in ["meta.json", ".managed.json", ".tantivy-writer.lock", "abc.idx", "files"] {
            assert_eq!(segment_id(other), None, "{other}");
        }
    }
}
