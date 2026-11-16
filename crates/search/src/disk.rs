//! What an index directory holds on disk, against what its live segments
//! use: dev tooling for the size budget (§3), read by `size-report` and
//! `index-stats` (bench). A `du` of a working index can count segments a
//! merge has replaced but garbage collection hasn't removed yet, and
//! deleted documents still waiting for a merge; this tells them apart.
//!
//! It opens no writer. Like any Tantivy reader it briefly takes the meta
//! lock (`.tantivy-meta.lock`), so the folder must be writable; that lock
//! keeps it safe beside a running writer's garbage collection. A commit
//! landing mid-read can still show its new files as stale, so stop the
//! server (or read a copy) for exact figures.
//!
//! Live segment bytes include deleted documents until a merge drops them;
//! their count is reported beside it, so measure after the final merge.

use std::collections::HashMap;
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
    /// Bytes of the files the live segments use, as on disk: the budget
    /// figure (§3), and what `smoke.py` counts.
    pub live_file_bytes: u64,
    /// The live segments' bytes by Tantivy's own accounting, which the
    /// per-part table splits. It leaves out per-file headers and footers,
    /// so it reads a little under [`DiskStats::live_file_bytes`].
    pub space_usage_bytes: u64,
    /// Bytes of the files directly in the directory: what `size-report`
    /// counts.
    pub top_level_bytes: u64,
    /// Bytes of every file under the directory, subdirectories included:
    /// what `du` counts.
    pub recursive_bytes: u64,
    /// Bytes of top-level segment files the live segments don't use:
    /// segments merged away and not yet garbage-collected, a live
    /// segment's superseded delete files, and leftover temporary stores.
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
        // Each live segment and the opstamp of its current delete file.
        let mut live = HashMap::new();
        let (mut docs, mut deleted, mut space_usage_bytes) = (0u64, 0u64, 0u64);
        for seg in searcher.segment_readers() {
            live.insert(seg.segment_id().uuid_string(), seg.delete_opstamp());
            docs += u64::from(seg.num_docs());
            deleted += u64::from(seg.num_deleted_docs());
            space_usage_bytes += seg.space_usage().map_err(Error::Io)?.total().get_bytes();
        }
        let (mut top_level_bytes, mut live_file_bytes) = (0, 0);
        let (mut stale_bytes, mut stale_files) = (0, Vec::new());
        for entry in std::fs::read_dir(dir).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            let meta = entry.metadata().map_err(Error::Io)?;
            if !meta.is_file() {
                continue;
            }
            top_level_bytes += meta.len();
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_stale(&name, &live) {
                stale_bytes += meta.len();
                stale_files.push(name);
            } else if segment_id(&name).is_some() {
                live_file_bytes += meta.len();
            }
        }
        stale_files.sort();
        Ok(DiskStats {
            segments: searcher.segment_readers().len(),
            docs,
            deleted,
            live_file_bytes,
            space_usage_bytes,
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

/// Whether a top-level file is a segment file the live segments don't
/// use: another segment's, or a live one's superseded `.{opstamp}.del` or
/// leftover `.store.temp`.
fn is_stale(name: &str, live: &HashMap<String, Option<u64>>) -> bool {
    let Some(id) = segment_id(name) else {
        return false;
    };
    let Some(&current) = live.get(id) else {
        return true;
    };
    let rest = &name[id.len() + 1..];
    if rest == "store.temp" {
        return true;
    }
    match rest.strip_suffix(".del") {
        Some(opstamp) => opstamp.parse::<u64>().ok() != current,
        None => false,
    }
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
        // The files on disk are the budget figure: Tantivy's accounting
        // leaves out per-file headers and footers, so it's never more.
        assert!(
            0 < s.space_usage_bytes
                && s.space_usage_bytes <= s.live_file_bytes
                && s.live_file_bytes <= s.top_level_bytes,
            "{s:?}"
        );
        // The file index lives in a subdirectory: `du` counts it, the
        // top level doesn't.
        assert!(s.top_level_bytes < s.recursive_bytes, "{s:?}");
        assert_eq!((s.stale_bytes, s.stale_files.len()), (0, 0), "{s:?}");
        let parts = space_by_field_at(&dir).unwrap();
        assert!(
            parts.iter().any(|(p, b)| p == "postings name" && *b > 0),
            "{parts:?}"
        );

        // A segment file no live segment owns is stale; `meta.json` never is.
        let orphan = "0123456789abcdef0123456789abcdef.idx";
        std::fs::write(dir.join(orphan), [0u8; 100]).unwrap();
        let s = DiskStats::read(&dir).unwrap();
        assert_eq!(
            (s.stale_bytes, s.stale_files),
            (100, vec![orphan.to_owned()])
        );
        std::fs::remove_file(dir.join(orphan)).unwrap();

        // Merged into one, the delete is gone with the old segments, and
        // garbage collection removed their files. The no-op delete makes
        // the commit that publishes the merge.
        let (raw, _) = index.raw();
        let mut w: tantivy::IndexWriter = raw.writer_with_num_threads(1, 15 << 20).unwrap();
        let ids = raw.searchable_segment_ids().unwrap();
        w.merge(&ids).wait().unwrap();
        w.delete_term(Term::from_field_u64(index.fields().id, 999));
        w.commit().unwrap();
        w.wait_merging_threads().unwrap();
        let s = DiskStats::read(&dir).unwrap();
        assert_eq!((s.segments, s.docs, s.deleted), (1, 19, 0), "{s:?}");
        assert_eq!(s.stale_bytes, 0, "{s:?}");
    }

    #[test]
    fn only_files_the_live_segments_use_are_live() {
        let (a, b) = (
            "0123456789abcdef0123456789abcdef",
            "fedcba9876543210fedcba9876543210",
        );
        let live = HashMap::from([(a.to_owned(), Some(7)), (b.to_owned(), None)]);
        for name in [
            format!("{a}.idx"),
            format!("{a}.7.del"),
            format!("{b}.store"),
            "meta.json".into(),
        ] {
            assert!(!is_stale(&name, &live), "{name}");
        }
        let other = "00000000000000000000000000000000";
        for name in [
            format!("{a}.5.del"),
            format!("{b}.3.del"),
            format!("{a}.store.temp"),
            format!("{other}.idx"),
        ] {
            assert!(is_stale(&name, &live), "{name}");
        }
    }

    #[test]
    fn segment_files_are_told_from_the_rest() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(segment_id(&format!("{id}.idx")), Some(id));
        assert_eq!(segment_id(&format!("{id}.12.del")), Some(id));
        for other in [
            "meta.json",
            ".managed.json",
            ".tantivy-writer.lock",
            "abc.idx",
            "files",
        ] {
            assert_eq!(segment_id(other), None, "{other}");
        }
    }
}
