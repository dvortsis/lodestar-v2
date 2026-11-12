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
    /// An enrichment plugin identified the work ([`via_plugin`], ENR-10).
    Plugin,
}

impl ResolvedVia {
    /// The listed methods: what `/search/fields` and the query language's
    /// hints offer. [`ResolvedVia::Plugin`] isn't listed yet: ENR-10's own
    /// lookups never resolve a title (runner, 2026-11-12), so offering it
    /// would match nothing. The slice that first makes it fire lists it,
    /// with its label. [`from_name`](Self::from_name) still reads it, so
    /// stored records and `resolved:plugin` queries work.
    pub const ALL: [ResolvedVia; 3] = [
        ResolvedVia::File,
        ResolvedVia::Folder,
        ResolvedVia::Structure,
    ];

    /// The name used by `resolved:` queries and the UI tooltip.
    pub fn as_str(self) -> &'static str {
        match self {
            ResolvedVia::File => "files",
            ResolvedVia::Folder => "folder",
            ResolvedVia::Structure => "structure",
            ResolvedVia::Plugin => "plugin",
        }
    }

    pub fn from_name(name: &str) -> Option<ResolvedVia> {
        ResolvedVia::ALL
            .into_iter()
            .chain([ResolvedVia::Plugin])
            .find(|v| v.as_str() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub release: Release,
    /// The torrent name itself says nothing useful.
    pub ambiguous: bool,
    /// Set when an ambiguous name was resolved from the file tree.
    pub resolved: Option<ResolvedVia>,
    /// The file or folder name `release` was parsed from, when it was
    /// resolved from the file tree (`resolved` is set); with `None` it was
    /// parsed from the torrent's name. The cracked check reads its crack
    /// words from this text (RP-UC).
    pub resolved_from: Option<String>,
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
            resolved_from: None,
            confidence: 100,
            episodic_files,
        };
    }

    let mut candidates: Vec<(Release, ResolvedVia, &str)> = Vec::new();
    for (i, r) in dominant.iter().zip(parsed) {
        if !is_uninformative(files[*i].name()) && !r.title.is_empty() {
            candidates.push((r, ResolvedVia::File, files[*i].name()));
        }
    }
    for (folder, via) in [
        (top_folder(files), ResolvedVia::Folder),
        (disc_folder(files), ResolvedVia::Structure),
    ] {
        if let Some(f) = folder.filter(|f| !is_uninformative(f) && !f.eq_ignore_ascii_case(name)) {
            let r = parse(f);
            if !r.title.is_empty() {
                candidates.push((r, via, f));
            }
        }
    }
    // Ties go to the earlier candidate: files, then folder, then structure.
    let best = candidates
        .into_iter()
        .enumerate()
        .max_by_key(|(i, (r, _, _))| (r.informativeness(), std::cmp::Reverse(*i)))
        .map(|(_, c)| c);
    match best {
        Some((mut release, via, from)) => {
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
                // Never a file-tree candidate.
                (ResolvedVia::Plugin, _) => 0,
            };
            Effective {
                release,
                ambiguous,
                resolved: Some(via),
                resolved_from: Some(from.to_owned()),
                confidence,
                episodic_files,
            }
        }
        None => Effective {
            release: named,
            ambiguous,
            resolved: None,
            resolved_from: None,
            confidence: 0,
            episodic_files,
        },
    }
}

/// What an enrichment plugin resolved a torrent to (architecture §6,
/// plan ENR-10): the work's title and year as its database holds them,
/// and how sure the plugin is of the match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginTitle<'a> {
    pub title: &'a str,
    pub year: Option<u16>,
    /// 0–100 percent.
    pub confidence: u8,
}

/// The highest title confidence a plugin's answer gives: an informative
/// name (100) is always surer than a lookup.
pub const MAX_PLUGIN_CONFIDENCE: u8 = 95;

/// `e` resolved via a plugin: `Some` only when the torrent's name is
/// ambiguous and the plugin is surer of the title than the file tree was
/// (its confidence, capped at [`MAX_PLUGIN_CONFIDENCE`], above
/// `e.confidence`). An informative name always keeps its own title, and a
/// plugin never changes what the name says.
///
/// The plugin names the work: its title and year replace `e`'s. Episode
/// numbers, the pack flag and every quality field still come from the
/// torrent itself, since the plugin knows the work, not this release of
/// it. `resolved_from` is cleared: the title wasn't parsed from a file or
/// folder name.
pub fn via_plugin(e: &Effective, p: PluginTitle<'_>) -> Option<Effective> {
    let title: String = p.title.chars().filter(|c| !c.is_control()).collect();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let confidence = p.confidence.min(MAX_PLUGIN_CONFIDENCE);
    if !e.ambiguous || title.is_empty() || confidence <= e.confidence {
        return None;
    }
    let mut release = Release {
        title,
        year: p
            .year
            .filter(|y| (1..=9999).contains(y))
            .map(lodestar_release_parser::Span::one),
        season: e.release.season,
        episode: e.release.episode,
        absolute: e.release.absolute,
        date: e.release.date,
        pack: e.release.pack,
        ..Release::default()
    };
    fill_quality(&mut release, &e.release);
    Some(Effective {
        release,
        ambiguous: true,
        resolved: Some(ResolvedVia::Plugin),
        resolved_from: None,
        confidence,
        episodic_files: e.episodic_files,
    })
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
    fn a_plugin_resolves_only_an_ambiguous_name_it_is_surer_of() {
        let p = PluginTitle {
            title: " The\u{7}  Work ",
            year: Some(1999),
            confidence: 100,
        };
        let (files, comp) = tree(&[("New Folder/01.mp4", 9)]);
        let e = resolve("New Folder", &files, &comp);
        let r = via_plugin(&e, p).unwrap();
        assert_eq!(r.release.title, "The Work");
        assert_eq!(r.release.year.map(|y| y.first), Some(1999));
        assert_eq!(r.resolved, Some(ResolvedVia::Plugin));
        assert_eq!(r.confidence, MAX_PLUGIN_CONFIDENCE);
        assert!(r.ambiguous && r.resolved_from.is_none());
        assert_eq!(ResolvedVia::from_name("plugin"), Some(ResolvedVia::Plugin));
        assert!(
            !ResolvedVia::ALL.contains(&ResolvedVia::Plugin),
            "not listed yet"
        );
        // Not surer than the files: kept.
        let (files, comp) = tree(&[("Show.Name.S02E05.2160p.WEB-DL.mkv", 9)]);
        let e = resolve("a8f3x.zip.part", &files, &comp);
        assert_eq!(
            via_plugin(
                &e,
                PluginTitle {
                    confidence: 85,
                    ..p
                }
            ),
            None
        );
        let r = via_plugin(
            &e,
            PluginTitle {
                confidence: 90,
                year: Some(0),
                ..p
            },
        )
        .unwrap();
        // The episode and the quality still come from the release.
        assert_eq!(r.release.episode.map(|s| s.first), Some(5));
        assert_eq!(r.release.resolution.map(|r| r.as_str()), Some("2160p"));
        assert_eq!(r.release.year, None);
        // An informative name keeps its own title, and an empty title
        // resolves nothing.
        let (files, comp) = tree(&[("Movie.2010.mkv", 9)]);
        let e = resolve("Some Movie 2010", &files, &comp);
        assert_eq!(via_plugin(&e, p), None);
        let (files, comp) = tree(&[("New Folder/01.mp4", 9)]);
        let e = resolve("New Folder", &files, &comp);
        assert_eq!(
            via_plugin(
                &e,
                PluginTitle {
                    title: " \u{0} ",
                    ..p
                }
            ),
            None
        );
    }

    #[test]
    fn unresolvable() {
        let (files, comp) = tree(&[("New Folder/01.mp4", 9)]);
        let e = resolve("New Folder", &files, &comp);
        assert!(e.ambiguous);
        assert_eq!((e.resolved, e.confidence), (None, 0));
    }
}
