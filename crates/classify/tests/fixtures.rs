//! Whole-torrent fixtures: a name and its files, and what classification
//! should conclude. Written before the classifier was run on them; the
//! first-run result is recorded in architecture §16.

use lodestar_classify::{ContentType, FileInfo, JunkReason, ResolvedVia, classify};

struct Fixture {
    name: &'static str,
    files: &'static [(&'static str, &'static str)],
    kind: ContentType,
    title: &'static str,
    resolved: Option<ResolvedVia>,
    junk: &'static [JunkReason],
}

/// `"4.2G"`, `"350M"`, `"12K"`, `"0"`, `"pad:16K"` (a padding file).
fn file(path: &str, size: &str) -> FileInfo {
    let (padding, size) = match size.strip_prefix("pad:") {
        Some(s) => (true, s),
        None => (false, size),
    };
    let (num, mult) = match size.as_bytes().last() {
        Some(b'K') => (&size[..size.len() - 1], 1u64 << 10),
        Some(b'M') => (&size[..size.len() - 1], 1 << 20),
        Some(b'G') => (&size[..size.len() - 1], 1 << 30),
        _ => (size, 1),
    };
    let bytes = (num.parse::<f64>().unwrap() * mult as f64) as u64;
    FileInfo {
        path: path.split('/').map(str::to_owned).collect(),
        size: bytes,
        padding,
    }
}

use ContentType::*;
use JunkReason::*;

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "The.Matrix.1999.1080p.BluRay.x264-AMIABLE",
        files: &[
            ("The.Matrix.1999.1080p.BluRay.x264-AMIABLE.mkv", "8.7G"),
            ("The.Matrix.1999.1080p.BluRay.x264-AMIABLE.nfo", "4K"),
            ("Sample/the.matrix.1999.1080p.sample.mkv", "60M"),
        ],
        kind: Movie,
        title: "The Matrix",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Breaking.Bad.S05.1080p.BluRay.x264-ROVERS",
        files: &[
            ("Breaking.Bad.S05E01.1080p.BluRay.x264-ROVERS.mkv", "2.1G"),
            ("Breaking.Bad.S05E02.1080p.BluRay.x264-ROVERS.mkv", "2.0G"),
            ("Breaking.Bad.S05E03.1080p.BluRay.x264-ROVERS.mkv", "2.2G"),
        ],
        kind: Tv,
        title: "Breaking Bad",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "a8f3x.zip.part",
        files: &[("Show.Name.S02E05.2160p.WEB-DL.mkv", "6.1G")],
        kind: Tv,
        title: "Show Name",
        resolved: Some(ResolvedVia::File),
        junk: &[],
    },
    Fixture {
        name: "New Folder (2)",
        files: &[
            (
                "Planet.Earth.II.S01E01.Islands.2160p.UHD.BluRay.x265.mkv",
                "9.0G",
            ),
            (
                "Planet.Earth.II.S01E02.Mountains.2160p.UHD.BluRay.x265.mkv",
                "8.8G",
            ),
        ],
        kind: Tv,
        title: "Planet Earth II",
        resolved: Some(ResolvedVia::File),
        junk: &[],
    },
    Fixture {
        name: "DISC1",
        files: &[
            ("Inception (2010)/BDMV/STREAM/00800.m2ts", "33G"),
            ("Inception (2010)/BDMV/STREAM/00801.m2ts", "1.2G"),
            ("Inception (2010)/BDMV/index.bdmv", "1K"),
            ("Inception (2010)/CERTIFICATE/id.bdmv", "1K"),
        ],
        kind: Movie,
        title: "Inception",
        resolved: Some(ResolvedVia::Folder),
        junk: &[],
    },
    Fixture {
        name: "Pink Floyd - The Dark Side of the Moon (1973) [FLAC]",
        files: &[
            ("01 - Speak to Me.flac", "30M"),
            ("02 - Breathe.flac", "40M"),
            ("03 - On the Run.flac", "50M"),
            ("cover.jpg", "2M"),
            ("Pink Floyd - The Dark Side of the Moon.cue", "2K"),
            ("Pink Floyd - The Dark Side of the Moon.log", "8K"),
        ],
        kind: Music,
        title: "Pink Floyd - The Dark Side of the Moon",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Frank Herbert - Dune [Audiobook]",
        files: &[
            ("Dune Part 1.m4b", "600M"),
            ("Dune Part 2.m4b", "580M"),
            ("cover.jpg", "1M"),
        ],
        kind: Audiobook,
        title: "Frank Herbert - Dune",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Brandon Sanderson - The Way of Kings (2010) [EPUB]",
        files: &[("Brandon Sanderson - The Way of Kings (2010).epub", "3M")],
        kind: Ebook,
        title: "Brandon Sanderson - The Way of Kings",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Saga (2012-2018) (Digital)",
        files: &[
            ("Saga 001 (2012).cbz", "40M"),
            ("Saga 002 (2012).cbz", "38M"),
            ("Saga 003 (2012).cbr", "39M"),
        ],
        kind: Comic,
        title: "Saga",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Blender 4.2.0 Windows x64",
        files: &[
            ("blender-4.2.0-windows-x64.msi", "320M"),
            ("readme.txt", "2K"),
        ],
        kind: Software,
        title: "Blender",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Ubuntu-24.04-desktop-amd64.iso",
        files: &[("Ubuntu-24.04-desktop-amd64.iso", "5.7G")],
        kind: Software,
        title: "Ubuntu",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Hogwarts Legacy - Deluxe Edition [FitGirl Repack]",
        files: &[
            ("setup.exe", "5M"),
            ("fg-01.bin", "4G"),
            ("fg-02.bin", "4G"),
            ("fg-03.bin", "3.1G"),
            ("MD5/fitgirl-bins.md5", "1K"),
        ],
        kind: Game,
        title: "Hogwarts Legacy",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Doom.Eternal-CODEX",
        files: &[
            ("codex-doom.eternal.iso", "42G"),
            ("codex-doom.eternal.nfo", "4K"),
        ],
        kind: Game,
        title: "Doom Eternal",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "The.Legend.of.Zelda.Tears.of.the.Kingdom.NSW-VENOM",
        files: &[("venom-totk.nsp", "16G")],
        kind: Game,
        title: "The Legend of Zelda Tears of the Kingdom",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Some.Movie.2023.1080p.BluRay.x264-GRP",
        files: &[
            ("some.movie.2023.1080p.bluray.x264-grp.rar", "50M"),
            ("some.movie.2023.1080p.bluray.x264-grp.r00", "50M"),
            ("some.movie.2023.1080p.bluray.x264-grp.r01", "50M"),
            ("some.movie.2023.1080p.bluray.x264-grp.sfv", "1K"),
            ("some.movie.2023.1080p.bluray.x264-grp.nfo", "3K"),
        ],
        kind: Movie,
        title: "Some Movie",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Wedding Photos 2019",
        files: &[
            ("DCIM/IMG_0001.JPG", "6M"),
            ("DCIM/IMG_0002.JPG", "6M"),
            ("DCIM/IMG_0003.JPG", "6M"),
        ],
        kind: Images,
        title: "Wedding Photos",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Home Videos",
        files: &[
            ("clip1.mp4", "300M"),
            ("clip2.mp4", "200M"),
            ("clip3.mp4", "250M"),
        ],
        kind: Video,
        title: "Home Videos",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Avengers.Endgame.2019.1080p.WEBRip.x264",
        files: &[
            ("Avengers.Endgame.2019.1080p.WEBRip.x264.mp4.exe", "2M"),
            ("Readme.txt", "1K"),
        ],
        kind: Software,
        title: "Avengers Endgame",
        resolved: None,
        junk: &[DoubleExtension, NameMismatch],
    },
    Fixture {
        name: "Oppenheimer.2023.2160p.WEB-DL.DDP5.1.HEVC",
        files: &[
            ("Oppenheimer.2023.2160p.WEB-DL.DDP5.1.HEVC.mkv", "18M"),
            ("Codec Installer.exe", "1M"),
            ("PASSWORD.txt", "1K"),
        ],
        kind: Movie,
        title: "Oppenheimer",
        resolved: None,
        junk: &[Executable, LureFile, TinyVideo],
    },
    Fixture {
        name: "Dune.Part.Two.2024.1080p.WEB-DL.x264",
        files: &[
            ("Dune.Part.Two.2024.1080p.WEB-DL.x264.zip", "1.4G"),
            ("Get Password Here.url", "1K"),
        ],
        kind: Other,
        title: "Dune Part Two",
        resolved: None,
        junk: &[WebShortcut, LureFile, ArchiveForMedia],
    },
    Fixture {
        name: "Interstellar.2014.1080p.BluRay.x264-SPARKS",
        files: &[
            ("Interstellar.2014.1080p.BluRay.x264-SPARKS.mkv", "12G"),
            ("Torrent downloaded from Example.com.url", "1K"),
        ],
        kind: Movie,
        title: "Interstellar",
        resolved: None,
        junk: &[WebShortcut],
    },
    Fixture {
        name: "Movie.2020.1080p.WEB-DL",
        files: &[("readme.txt", "1K"), ("cover.jpg", "100K")],
        kind: Images,
        title: "Movie",
        resolved: None,
        junk: &[NameMismatch],
    },
    Fixture {
        name: "padding",
        files: &[(".pad/0", "pad:16K"), (".pad/1", "pad:16K")],
        kind: Other,
        title: "padding",
        resolved: None,
        junk: &[PaddingOnly],
    },
    Fixture {
        name: "Empty.Release.2021.720p.HDTV",
        files: &[("Empty.Release.2021.720p.HDTV.mkv", "0"), ("info.nfo", "0")],
        kind: Movie,
        title: "Empty Release",
        resolved: None,
        junk: &[Empty, TinyVideo],
    },
    Fixture {
        name: "[SubsPlease] Frieren - 01 (1080p) [A1B2C3D4].mkv",
        files: &[("[SubsPlease] Frieren - 01 (1080p) [A1B2C3D4].mkv", "1.4G")],
        kind: Tv,
        title: "Frieren",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "VIDEO_TS",
        files: &[
            ("VIDEO_TS/VTS_01_1.VOB", "1G"),
            ("VIDEO_TS/VTS_01_2.VOB", "1G"),
            ("VIDEO_TS/VTS_01_0.IFO", "20K"),
        ],
        kind: Movie,
        title: "VIDEO TS",
        resolved: None,
        junk: &[],
    },
    Fixture {
        name: "Movies",
        files: &[
            ("Movies/The.Thing.1982.1080p.BluRay.x264-MOOVEE.mkv", "9G"),
            ("Movies/The.Thing.1982.1080p.BluRay.x264-MOOVEE.nfo", "4K"),
        ],
        kind: Movie,
        title: "The Thing",
        resolved: Some(ResolvedVia::File),
        junk: &[],
    },
    Fixture {
        name: "Total.Commander.v11.03.x86.x64.Incl.Keygen-BTCR",
        files: &[
            ("tcmd1103x32_64.exe", "12M"),
            ("Keygen/keygen.exe", "300K"),
            ("btcr.nfo", "5K"),
        ],
        kind: Software,
        title: "Total Commander",
        resolved: None,
        junk: &[],
    },
];

#[test]
fn fixtures() {
    let mut failures = Vec::new();
    for f in FIXTURES {
        let files: Vec<FileInfo> = f.files.iter().map(|(p, s)| file(p, s)).collect();
        let c = classify(f.name, &files);
        let mut problems = Vec::new();
        if c.content_type != f.kind {
            problems.push(format!(
                "type {:?} (confidence {}), want {:?}",
                c.content_type, c.confidence, f.kind
            ));
        }
        if c.title.release.title != f.title {
            problems.push(format!(
                "title {:?}, want {:?}",
                c.title.release.title, f.title
            ));
        }
        if c.title.resolved != f.resolved {
            problems.push(format!(
                "resolved {:?}, want {:?}",
                c.title.resolved, f.resolved
            ));
        }
        let mut got = c.junk.reasons.clone();
        got.sort();
        let mut want = f.junk.to_vec();
        want.sort();
        if got != want {
            problems.push(format!(
                "junk {got:?} (score {}), want {want:?}",
                c.junk.score
            ));
        }
        if !problems.is_empty() {
            failures.push(format!("{}\n    {}", f.name, problems.join("\n    ")));
        }
    }
    println!(
        "{}/{} fixtures pass",
        FIXTURES.len() - failures.len(),
        FIXTURES.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
