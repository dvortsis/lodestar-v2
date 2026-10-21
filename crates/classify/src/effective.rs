//! The effective title (§6 "Ambiguous names are the core use case"): the
//! release parser runs on the torrent name, its top folder and its
//! dominant files, and the most informative result wins.

use lodestar_release_parser::{Release, is_uninformative, parse};

use crate::FileInfo;
use crate::composition::Composition;
use crate::ext::{self, Category};

/// Where the effective title came from when the name didn't say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolvedVia {
    /// The largest file of the dominant category.
    File,
    /// The single folder every file is in.
    Folder,
    /// The folder holding a `BDMV/` or `VIDEO_TS/`.
    Structure,
}

impl ResolvedVia {
    /// The name used by `resolved:` queries and the UI tooltip.
    pub fn as_str(self) -> &'static str {
        match self {
            ResolvedVia::File => "files",
            ResolvedVia::Folder => "folder",
            ResolvedVia::Structure => "structure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub release: Release,
    /// The torrent name itself says nothing useful.
    pub ambiguous: bool,
    /// Set when an ambiguous name was resolved from the file tree.
    pub resolved: Option<ResolvedVia>,
    /// 0–100: how sure we are of the title. 100 for an informative name.
    pub confidence: u8,
    /// Some dominant file names carry season or episode numbers, so the
    /// torrent is episodic even when its name isn't.
    pub episodic_files: bool,
}

/// Files parsed per torrent, at most. Parsing is cheap, but 200k-file
/// torrents exist and the top few files decide anyway.
const MAX_PARSED_FILES: usize = 3;

pub(crate) fn resolve(name: &str, files: &[FileInfo], comp: &Composition) -> Effective {
    let named = parse(name);
    let ambiguous = named.title.is_empty() || is_uninformative(name);
    let dominant = dominant_files(files, comp);
    let parsed: Vec<Release> = dominant.iter().map(|&i| parse(files[i].name())).collect();
    let episodic_files = parsed
        .iter()
        .any(|r| r.season.is_some() || r.episode.is_some());
    let several = dominant.len() > 1;

    if !ambiguous {
        let mut release = named;
        if let Some(best) = parsed.first() {
            fill_quality(&mut release, best);
        }
        return Effective {
            release,
            ambiguous,
            resolved: None,
            confidence: 100,
            episodic_files,
        };
    }

    let mut candidates: Vec<(Release, ResolvedVia)> = Vec::new();
    for (i, r) in dominant.iter().zip(parsed) {
        if !is_uninformative(files[*i].name()) && !r.title.is_empty() {
            candidates.push((r, ResolvedVia::File));
        }
    }
    for (folder, via) in [
        (top_folder(files), ResolvedVia::Folder),
        (disc_folder(files), ResolvedVia::Structure),
    ] {
        if let Some(f) = folder.filter(|f| !is_uninformative(f) && !f.eq_ignore_ascii_case(name)) {
            let r = parse(f);
            if !r.title.is_empty() {
                candidates.push((r, via));
            }
        }
    }
    // Ties go to the earlier candidate: files, then folder, then structure.
    let best = candidates
        .into_iter()
        .enumerate()
        .max_by_key(|(i, (r, _))| (r.informativeness(), std::cmp::Reverse(*i)))
        .map(|(_, c)| c);
    match best {
        Some((mut release, via)) => {
            // Several episode files: the torrent is the pack, not the
            // first episode.
            if via == ResolvedVia::File && several && release.episode.is_some() {
                release.episode = None;
                release.pack = true;
            }
            fill_quality(&mut release, &named);
            let identified =
                release.year.is_some() || release.season.is_some() || release.episode.is_some();
            let confidence = match (via, identified) {
                (ResolvedVia::File, true) => 85,
                (ResolvedVia::File, false) => 65,
                (ResolvedVia::Folder, true) => 80,
                (ResolvedVia::Folder, false) => 60,
                (ResolvedVia::Structure, _) => 50,
            };
            Effective {
                release,
                ambiguous,
                resolved: Some(via),
                confidence,
                episodic_files,
            }
        }
        None => Effective {
            release: named,
            ambiguous,
            resolved: None,
            confidence: 0,
            episodic_files,
        },
    }
}

/// The largest files of the dominant category, skipping samples, largest
/// first.
fn dominant_files(files: &[FileInfo], comp: &Composition) -> Vec<usize> {
    let Some(dominant) = comp.dominant() else {
        return Vec::new();
    };
    let mut top: Vec<usize> = Vec::with_capacity(MAX_PARSED_FILES + 1);
    for (i, f) in files.iter().enumerate() {
        if f.padding || ext::category(f.name()) != dominant || is_sample_or_disc(f) {
            continue;
        }
        let at = top.partition_point(|&j| files[j].size >= f.size);
        if at < MAX_PARSED_FILES {
            top.insert(at, i);
            top.truncate(MAX_PARSED_FILES);
        }
    }
    top
}

/// Samples, and the stream files of a disc structure (`VTS_01_1.VOB`,
/// `00800.m2ts`), are never titles.
fn is_sample_or_disc(f: &FileInfo) -> bool {
    f.path.iter().any(|s| {
        s.eq_ignore_ascii_case("bdmv")
            || s.eq_ignore_ascii_case("video_ts")
            || s.eq_ignore_ascii_case("sample")
            || s.eq_ignore_ascii_case("samples")
            || (ext::category(s) == Category::Video
                && s.to_ascii_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|w| w == "sample"))
    })
}

/// The folder every file sits in, below the torrent's own name.
fn top_folder(files: &[FileInfo]) -> Option<&str> {
    let mut real = files.iter().filter(|f| !f.padding);
    let first = real.next()?.path.first()?;
    let shared = files
        .iter()
        .filter(|f| !f.padding)
        .all(|f| f.path.len() > 1 && f.path[0] == *first);
    shared.then_some(first.as_str())
}

/// `Movie Name (2019)/BDMV/…`: the folder above a disc structure.
fn disc_folder(files: &[FileInfo]) -> Option<&str> {
    files.iter().find_map(|f| {
        let at = f
            .path
            .iter()
            .position(|s| s.eq_ignore_ascii_case("bdmv") || s.eq_ignore_ascii_case("video_ts"))?;
        at.checked_sub(1).map(|p| f.path[p].as_str())
    })
}

/// Quality fields the other parse has and this one lacks. Identity
/// (title, year, episodes, group) is never mixed between names.
fn fill_quality(dst: &mut Release, src: &Release) {
    fn fill<T: Clone>(d: &mut Option<T>, s: &Option<T>) {
        if d.is_none() {
            d.clone_from(s);
        }
    }
    fn fill_list<T: Clone>(d: &mut Vec<T>, s: &[T]) {
        if d.is_empty() {
            d.extend_from_slice(s);
        }
    }
    fill(&mut dst.resolution, &src.resolution);
    fill(&mut dst.source, &src.source);
    fill(&mut dst.service, &src.service);
    fill(&mut dst.video_codec, &src.video_codec);
    fill(&mut dst.bit_depth, &src.bit_depth);
    fill(&mut dst.channels, &src.channels);
    fill(&mut dst.container, &src.container);
    fill(&mut dst.bitrate, &src.bitrate);
    fill(&mut dst.sample_rate, &src.sample_rate);
    fill_list(&mut dst.hdr, &src.hdr);
    fill_list(&mut dst.audio, &src.audio);
    fill_list(&mut dst.languages, &src.languages);
    fill_list(&mut dst.subtitles, &src.subtitles);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[(&str, u64)]) -> (Vec<FileInfo>, Composition) {
        let files: Vec<FileInfo> = paths
            .iter()
            .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
            .collect();
        let mut comp = Composition::default();
        for f in &files {
            comp.add(ext::category(f.name()), f.size);
        }
        (files, comp)
    }

    #[test]
    fn hash_named_torrent_resolves_via_files() {
        let (files, comp) = tree(&[("Show.Name.S02E05.2160p.WEB-DL.mkv", 4_000_000_000)]);
        let e = resolve("a8f3x.zip.part", &files, &comp);
        assert!(e.ambiguous);
        assert_eq!(e.resolved, Some(ResolvedVia::File));
        assert_eq!(e.release.title, "Show Name");
        assert_eq!(e.release.episode.map(|s| s.first), Some(5));
        assert_eq!(e.confidence, 85);
    }

    #[test]
    fn episode_files_make_a_pack() {
        let (files, comp) = tree(&[
            ("New Folder/Show.S01E01.720p.mkv", 900),
            ("New Folder/Show.S01E02.720p.mkv", 800),
            ("New Folder/Show.S01E02.sample.mkv", 10),
        ]);
        let e = resolve("New Folder", &files, &comp);
        assert_eq!(e.release.title, "Show");
        assert_eq!(e.release.season.map(|s| s.first), Some(1));
        assert_eq!(e.release.episode, None);
        assert!(e.release.pack && e.episodic_files);
    }

    #[test]
    fn folder_and_structure() {
        let (files, comp) = tree(&[
            ("Some.Movie.2019.1080p/BDMV/STREAM/00001.m2ts", 30),
            ("Some.Movie.2019.1080p/BDMV/STREAM/00002.m2ts", 20),
        ]);
        let e = resolve("disc", &files, &comp);
        assert_eq!(e.resolved, Some(ResolvedVia::Folder));
        assert_eq!(e.release.title, "Some Movie");

        let (files, comp) = tree(&[
            ("Movie Name (2010)/BDMV/STREAM/00001.m2ts", 30),
            ("x/y.m2ts", 1),
        ]);
        let e = resolve("disc", &files, &comp);
        assert_eq!(e.resolved, Some(ResolvedVia::Structure));
        assert_eq!(e.release.title, "Movie Name");
    }

    #[test]
    fn informative_name_keeps_its_identity_and_borrows_quality() {
        let (files, comp) = tree(&[("Show.Name.S03E01.1080p.WEB.H264-GRP.mkv", 9)]);
        let e = resolve("Show Name Season 3", &files, &comp);
        assert!(!e.ambiguous);
        assert_eq!(e.release.title, "Show Name");
        assert_eq!(e.release.episode, None);
        assert!(e.release.pack);
        assert_eq!(e.release.group, None);
        assert_eq!(e.release.resolution.map(|r| r.as_str()), Some("1080p"));
        assert_eq!(e.release.container, Some("mkv"));
    }

    #[test]
    fn unresolvable() {
        let (files, comp) = tree(&[("New Folder/01.mp4", 9)]);
        let e = resolve("New Folder", &files, &comp);
        assert!(e.ambiguous);
        assert_eq!((e.resolved, e.confidence), (None, 0));
    }
}
