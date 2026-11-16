//! `index-stats DIR [--torrents N]`: what a torrent index directory holds
//! on disk, and where its bytes go (dev tooling for the §3 size budget;
//! nothing here ships in the product binary).
//!
//! `DIR` is a data folder's `index` (this build's `v{schema}-t{tokenizer}`
//! inside it is read) or a Tantivy index directory itself. It opens no
//! writer, but takes Tantivy's meta lock briefly, so the folder must be
//! writable. Stop the server first, or read a copy, so files don't change
//! underneath it.
//!
//! It tells a real overrun from a measurement artefact: `du` also counts
//! segment files the live segments don't use (stale: merged away and not
//! yet collected) and the file index's subdirectory. The budget figure is
//! the live segments' bytes per torrent. Those include deleted documents
//! until a merge drops them, so measure after the final commit and merge.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lodestar_search::DiskStats;

const USAGE: &str = "usage: index-stats DIR [--torrents N]

  DIR           a data folder's index (this build's index inside it is
                read) or a Tantivy index directory
  --torrents N  divide by N torrents (default: the torrent index's docs)";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("index-stats: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<String, Box<dyn std::error::Error>> {
    let mut dir = None;
    let mut torrents = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--torrents" => {
                let n: u64 = it.next().ok_or("--torrents needs a value")?.parse()?;
                if n == 0 {
                    return Err("--torrents must be positive".into());
                }
                torrents = Some(n);
            }
            "-h" | "--help" => return Ok(format!("{USAGE}\n")),
            _ if dir.is_none() => dir = Some(PathBuf::from(a)),
            other => return Err(format!("unexpected argument {other:?}").into()),
        }
    }
    let dir = resolve(&dir.ok_or("DIR is required")?);
    report(&dir, torrents)
}

/// The Tantivy directory to read: `dir` itself, or this build's index
/// inside a data folder's `index`.
fn resolve(dir: &Path) -> PathBuf {
    if dir.join("meta.json").is_file() {
        dir.to_path_buf()
    } else {
        lodestar_search::index_dir(dir)
    }
}

fn report(dir: &Path, torrents: Option<u64>) -> Result<String, Box<dyn std::error::Error>> {
    use std::fmt::Write;
    let s = DiskStats::read(dir)?;
    let n = torrents.unwrap_or(s.docs.max(1)) as f64;
    let mb = |b: u64| b as f64 / 1e6;
    let per = |b: u64| b as f64 / n;
    let mut out = String::new();
    writeln!(out, "index-stats {}", dir.display())?;
    writeln!(out, "  per torrent of        {n:.0}")?;
    writeln!(out, "  segments              {}", s.segments)?;
    writeln!(
        out,
        "  docs                  {} ({} deleted = {:.1}%, still in the live bytes until a merge)",
        s.docs,
        s.deleted,
        100.0 * s.deleted as f64 / (s.docs + s.deleted).max(1) as f64
    )?;
    let rows = [
        ("live segment files", s.live_file_bytes, "the budget figure"),
        (
            "  by Tantivy's count",
            s.space_usage_bytes,
            "the parts below; no file headers",
        ),
        (
            "top-level files",
            s.top_level_bytes,
            "what size-report counts",
        ),
        (
            "stale segment files",
            s.stale_bytes,
            "not used by live segments",
        ),
        ("everything (du)", s.recursive_bytes, "incl. the file index"),
    ];
    for (label, bytes, note) in rows {
        writeln!(
            out,
            "  {label:<21} {:>9.1} MB = {:>6.1} B per torrent  ({note})",
            mb(bytes),
            per(bytes)
        )?;
    }
    let files = dir.join("files");
    if files.join("meta.json").is_file() {
        let f = DiskStats::read(&files)?;
        writeln!(
            out,
            "  file index (live)     {:>9.1} MB = {:>6.1} B per torrent  ({} files, {} segments)",
            mb(f.live_file_bytes),
            per(f.live_file_bytes),
            f.docs,
            f.segments
        )?;
    }
    writeln!(out, "\n  torrent index by part (bytes per torrent)")?;
    for (part, bytes) in lodestar_search::space_by_field_at(dir)? {
        if per(bytes) >= 0.5 {
            writeln!(out, "    {part:<28} {:>7.1}", per(bytes))?;
        }
    }
    if !s.stale_files.is_empty() {
        writeln!(out, "\n  stale files: {}", s.stale_files.join(" "))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_small_index_found_from_its_data_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = lodestar_search::index_dir(tmp.path());
        let index = lodestar_search::SearchIndex::open(&index_dir).unwrap();
        let mut w = index.writer(15 << 20).unwrap();
        w.commit().unwrap();
        w.finish().unwrap();
        drop(index);
        // A segment file no live segment uses: reported, by name, as stale.
        let orphan = "0123456789abcdef0123456789abcdef.idx";
        std::fs::write(index_dir.join(orphan), [0u8; 100]).unwrap();

        assert_eq!(resolve(tmp.path()), index_dir);
        assert_eq!(resolve(&index_dir), index_dir);
        let text = run(vec![
            tmp.path().display().to_string(),
            "--torrents".into(),
            "10".into(),
        ])
        .unwrap();
        for want in [
            "segments",
            "live segment files",
            "top-level files",
            "stale segment files",
            "everything (du)",
            "file index (live)",
        ] {
            assert!(text.contains(want), "{want} missing from\n{text}");
        }
        assert!(text.contains(&format!("stale files: {orphan}")), "{text}");
        assert!(run(vec![]).is_err());
        let zero = run(vec![
            tmp.path().display().to_string(),
            "--torrents".into(),
            "0".into(),
        ]);
        assert!(zero.is_err());
        assert!(run(vec!["a".into(), "b".into()]).is_err());
    }
}
