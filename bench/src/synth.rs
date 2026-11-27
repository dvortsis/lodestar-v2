//! A deterministic generator of realistic info dictionaries.
//!
//! Sizes are only meaningful if the synthetic names compress like real ones,
//! so the generator mimics how real torrents are named and laid out: scene
//! release names with a shared vocabulary of resolutions, sources, codecs
//! and groups; season packs with near-identical episode names; albums with
//! numbered tracks; game repacks; image and ebook dumps; a heavy tail of huge
//! packs; hash-like "ambiguous" names; CJK names, some in legacy GBK; and a
//! few hybrid v2 torrents with padding files. The mix averages about 30
//! files per torrent (the §3 budget assumption).
//!
//! Real names are more varied than any word list, so a synthetic dataset
//! will compress somewhat better than a real crawl. M1 re-measures sizes on
//! real data.
//!
//! The stream depends only on the seed: [`rand_chacha`] is portable and
//! nothing here reads the clock or the environment.

use std::collections::BTreeMap;

use lodestar_bencode::{OwnedValue, to_vec};
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// What a synthetic torrent imitates; used for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Movie,
    MovieFolder,
    Episode,
    SeasonPack,
    AnimeBatch,
    Album,
    Discography,
    Software,
    GameRepack,
    Ebook,
    EbookPack,
    ImageSet,
    Ambiguous,
    MegaPack,
    CjkSeason,
    HybridV2,
}

/// Relative frequencies, in tenths of a percent.
const MIX: &[(Kind, u32)] = &[
    (Kind::Movie, 210),
    (Kind::MovieFolder, 70),
    (Kind::Episode, 120),
    (Kind::SeasonPack, 110),
    (Kind::AnimeBatch, 40),
    (Kind::Album, 115),
    (Kind::Discography, 15),
    (Kind::Software, 60),
    (Kind::GameRepack, 30),
    (Kind::Ebook, 40),
    (Kind::EbookPack, 25),
    (Kind::ImageSet, 35),
    (Kind::Ambiguous, 30),
    (Kind::MegaPack, 4),
    (Kind::CjkSeason, 20),
    (Kind::HybridV2, 5),
];

#[derive(Debug, Clone)]
pub struct SynthTorrent {
    pub kind: Kind,
    /// The bencoded info dictionary, as BEP 9 would deliver it.
    pub info: Vec<u8>,
    /// Unix seconds, spread over the year before the generator's epoch.
    pub discovered_at: i64,
}

/// The generator's version. Raise it in the same change as anything that
/// alters the stream a seed gives (names, layouts, the mix, sizes, times),
/// so a persisted benchmark dataset built by an older generator is refused
/// rather than reported as this one ([`crate::dataset::Key`]).
pub const VERSION: u32 = 1;

/// A fixed reference time so output never depends on the clock.
pub const EPOCH: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

#[derive(Debug)]
pub struct Generator {
    rng: ChaCha8Rng,
    total_weight: u32,
}

struct FileSpec {
    path: Vec<Vec<u8>>,
    size: u64,
    /// Path is legacy-encoded bytes; `path.utf-8` carries the UTF-8 form.
    utf8: Option<Vec<Vec<u8>>>,
}

impl Generator {
    pub fn new(seed: u64) -> Generator {
        Generator {
            rng: ChaCha8Rng::seed_from_u64(seed),
            total_weight: MIX.iter().map(|(_, w)| w).sum(),
        }
    }

    pub fn next_torrent(&mut self) -> SynthTorrent {
        let mut pick = self.rng.random_range(0..self.total_weight);
        let kind = MIX
            .iter()
            .find(|(_, w)| {
                if pick < *w {
                    true
                } else {
                    pick -= w;
                    false
                }
            })
            .map(|(k, _)| *k)
            .expect("weights cover the range");
        let discovered_at = EPOCH - self.rng.random_range(0..365 * 86_400);
        let info = self.build(kind);
        SynthTorrent {
            kind,
            info,
            discovered_at,
        }
    }

    fn build(&mut self, kind: Kind) -> Vec<u8> {
        match kind {
            Kind::Movie => {
                let name = self.movie_name();
                let size = self.size(700 << 20, 40 << 30);
                self.single(format!("{name}.mkv"), size)
            }
            Kind::MovieFolder => self.movie_folder(),
            Kind::Episode => {
                let show = self.title(1..4);
                let (s, e) = (self.rng.random_range(1..12), self.rng.random_range(1..24));
                let tail = self.release_tail();
                let name = format!("{show}.S{s:02}E{e:02}.{tail}");
                let size = self.size(150 << 20, 6 << 30);
                self.single(format!("{name}.mkv"), size)
            }
            Kind::SeasonPack => self.season_pack(),
            Kind::AnimeBatch => self.anime_batch(),
            Kind::Album => {
                let (name, files) = self.album(None);
                self.multi(name, files)
            }
            Kind::Discography => self.discography(),
            Kind::Software => self.software(),
            Kind::GameRepack => self.game_repack(),
            Kind::Ebook => {
                let (name, ext) = (
                    self.book_name(),
                    *["epub", "pdf", "mobi", "azw3"]
                        .choose(&mut self.rng)
                        .unwrap(),
                );
                let size = self.size(200 << 10, 60 << 20);
                self.single(format!("{name}.{ext}"), size)
            }
            Kind::EbookPack => self.ebook_pack(),
            Kind::ImageSet => self.image_set(),
            Kind::Ambiguous => self.ambiguous(),
            Kind::MegaPack => self.mega_pack(),
            Kind::CjkSeason => self.cjk_season(),
            Kind::HybridV2 => self.hybrid(),
        }
    }

    // ---- name parts -------------------------------------------------------

    fn word(&mut self) -> &'static str {
        WORDS.choose(&mut self.rng).unwrap()
    }

    fn title(&mut self, words: std::ops::Range<usize>) -> String {
        let n = self.rng.random_range(words);
        (0..n)
            .map(|_| capitalize(self.word()))
            .collect::<Vec<_>>()
            .join(".")
    }

    fn release_tail(&mut self) -> String {
        let res = *pick(
            &mut self.rng,
            &[("2160p", 15), ("1080p", 55), ("720p", 25), ("480p", 5)],
        );
        let src = *pick(
            &mut self.rng,
            &[
                ("WEB-DL", 35),
                ("WEBRip", 20),
                ("BluRay", 25),
                ("REMUX", 5),
                ("HDTV", 10),
                ("DVDRip", 5),
            ],
        );
        let codec = *pick(
            &mut self.rng,
            &[
                ("x264", 35),
                ("x265", 25),
                ("H.264", 20),
                ("HEVC", 15),
                ("AV1", 5),
            ],
        );
        let group = *GROUPS.choose(&mut self.rng).unwrap();
        if self.rng.random_bool(0.5) {
            let audio = *[
                "DDP5.1",
                "DTS-HD.MA.5.1",
                "AAC2.0",
                "TrueHD.Atmos.7.1",
                "DD5.1",
                "AC3",
            ]
            .choose(&mut self.rng)
            .unwrap();
            format!("{res}.{src}.{audio}.{codec}-{group}")
        } else {
            format!("{res}.{src}.{codec}-{group}")
        }
    }

    fn movie_name(&mut self) -> String {
        let title = self.title(1..5);
        let year = self.rng.random_range(1950..2026);
        let tail = self.release_tail();
        if self.rng.random_bool(0.15) {
            format!(
                "{} ({year}) {}",
                title.replace('.', " "),
                tail.replace('.', " ")
            )
        } else {
            format!("{title}.{year}.{tail}")
        }
    }

    fn book_name(&mut self) -> String {
        let author = format!("{} {}", capitalize(self.word()), capitalize(self.word()));
        let title = self.title(1..6).replace('.', " ");
        let year = self.rng.random_range(1900..2026);
        format!("{author} - {title} ({year})")
    }

    /// Log-uniform size in `[lo, hi)`, never round.
    fn size(&mut self, lo: u64, hi: u64) -> u64 {
        let (l, h) = ((lo as f64).ln(), (hi as f64).ln());
        let x: f64 = self.rng.random::<f64>() * (h - l) + l;
        (x.exp() as u64).max(1)
    }

    // ---- layouts ----------------------------------------------------------

    fn movie_folder(&mut self) -> Vec<u8> {
        let name = self.movie_name();
        let mut files = vec![
            file(&[&format!("{name}.mkv")], self.size(1 << 30, 60 << 30)),
            file(&[&format!("{name}.nfo")], self.size(2 << 10, 30 << 10)),
        ];
        if self.rng.random_bool(0.6) {
            files.push(file(
                &["Sample", &format!("{}.sample.mkv", name.to_lowercase())],
                self.size(20 << 20, 200 << 20),
            ));
        }
        let n_subs = self.rng.random_range(0..6);
        for lang in LANGS.sample(&mut self.rng, n_subs) {
            files.push(file(
                &["Subs", &format!("{lang}.srt")],
                self.size(30 << 10, 150 << 10),
            ));
        }
        if self.rng.random_bool(0.2) {
            for i in 1..=self.rng.random_range(2..6) {
                files.push(file(
                    &["Screens", &format!("screen{i:02}.png")],
                    self.size(500 << 10, 5 << 20),
                ));
            }
        }
        self.multi(name, files)
    }

    fn season_pack(&mut self) -> Vec<u8> {
        let show = self.title(1..4);
        let season = self.rng.random_range(1..15);
        let tail = self.release_tail();
        let name = format!("{show}.S{season:02}.{tail}");
        let episodes = self.rng.random_range(6..25);
        let with_titles = self.rng.random_bool(0.4);
        let subs = self.rng.random_bool(0.3);
        let mut files = Vec::new();
        for e in 1..=episodes {
            let ep = if with_titles {
                format!("{show}.S{season:02}E{e:02}.{}.{tail}", self.title(1..4))
            } else {
                format!("{show}.S{season:02}E{e:02}.{tail}")
            };
            files.push(file(&[&format!("{ep}.mkv")], self.size(300 << 20, 8 << 30)));
            if subs {
                files.push(file(
                    &["Subs", &format!("{ep}.en.srt")],
                    self.size(20 << 10, 90 << 10),
                ));
            }
        }
        if self.rng.random_bool(0.5) {
            files.push(file(
                &[&format!("{name}.nfo")],
                self.size(2 << 10, 20 << 10),
            ));
        }
        self.multi(name, files)
    }

    fn anime_batch(&mut self) -> Vec<u8> {
        let group = *ANIME_GROUPS.choose(&mut self.rng).unwrap();
        let show = self.title(1..5).replace('.', " ");
        let res = *["1080p", "720p"].choose(&mut self.rng).unwrap();
        let episodes = self.rng.random_range(10..27);
        let name = format!("[{group}] {show} (01-{episodes:02}) [{res}][HEVC][Batch]");
        let mut files: Vec<FileSpec> = (1..=episodes)
            .map(|e| {
                let crc: u32 = self.rng.random();
                file(
                    &[&format!(
                        "[{group}] {show} - {e:02} [{res}][HEVC][{crc:08X}].mkv"
                    )],
                    self.size(200 << 20, 2 << 30),
                )
            })
            .collect();
        if self.rng.random_bool(0.3) {
            for i in 1..=self.rng.random_range(1..7) {
                files.push(file(
                    &["Extras", &format!("NCOP{i:02} [{res}].mkv")],
                    self.size(30 << 20, 200 << 20),
                ));
            }
        }
        self.multi(name, files)
    }

    fn album(&mut self, artist: Option<&str>) -> (String, Vec<FileSpec>) {
        let artist = artist
            .map(str::to_owned)
            .unwrap_or_else(|| self.title(1..4).replace('.', " "));
        let album = self.title(1..5).replace('.', " ");
        let year = self.rng.random_range(1960..2026);
        let lossless = self.rng.random_bool(0.55);
        let (fmt, ext, lo, hi) = if lossless {
            (
                *["FLAC", "FLAC 24-96", "FLAC 16-44"]
                    .choose(&mut self.rng)
                    .unwrap(),
                "flac",
                15u64 << 20,
                90u64 << 20,
            )
        } else {
            (
                *["MP3 320", "MP3 V0", "AAC 256"]
                    .choose(&mut self.rng)
                    .unwrap(),
                "mp3",
                4 << 20,
                15 << 20,
            )
        };
        let name = format!("{artist} - {album} ({year}) [{fmt}]");
        let mut files: Vec<FileSpec> = (1..=self.rng.random_range(7..19))
            .map(|t| {
                let track = self.title(1..5).replace('.', " ");
                file(&[&format!("{t:02} - {track}.{ext}")], self.size(lo, hi))
            })
            .collect();
        files.push(file(&["cover.jpg"], self.size(80 << 10, 3 << 20)));
        if lossless && self.rng.random_bool(0.5) {
            files.push(file(
                &[&format!("{artist} - {album}.cue")],
                self.size(1 << 10, 4 << 10),
            ));
            files.push(file(
                &[&format!("{artist} - {album}.log")],
                self.size(3 << 10, 12 << 10),
            ));
        }
        (name, files)
    }

    fn discography(&mut self) -> Vec<u8> {
        let artist = self.title(1..4).replace('.', " ");
        let mut files = Vec::new();
        for _ in 0..self.rng.random_range(4..16) {
            let (album, tracks) = self.album(Some(&artist));
            for mut f in tracks {
                f.path.insert(0, album.clone().into_bytes());
                files.push(f);
            }
        }
        self.multi(format!("{artist} - Discography"), files)
    }

    fn software(&mut self) -> Vec<u8> {
        let product = self.title(1..3);
        let v = format!(
            "v{}.{}.{}",
            self.rng.random_range(1..30),
            self.rng.random_range(0..20),
            self.rng.random_range(0..999)
        );
        let arch = *["x64", "x86", "x64-x86", "macOS", "Linux.x64"]
            .choose(&mut self.rng)
            .unwrap();
        let group = *GROUPS.choose(&mut self.rng).unwrap();
        let name = format!("{product}.{v}.{arch}-{group}");
        if self.rng.random_bool(0.3) {
            let size = self.size(50 << 20, 8 << 30);
            return self.single(format!("{name}.iso"), size);
        }
        let mut files = vec![
            file(
                &[&format!("{}_setup.exe", product.to_lowercase())],
                self.size(10 << 20, 3 << 30),
            ),
            file(&["readme.txt"], self.size(500, 8 << 10)),
        ];
        if self.rng.random_bool(0.7) {
            files.push(file(
                &["Crack", &format!("{product}.dll")],
                self.size(100 << 10, 30 << 20),
            ));
            files.push(file(
                &["Crack", "instructions.txt"],
                self.size(300, 3 << 10),
            ));
        }
        for i in 0..self.rng.random_range(0..8) {
            files.push(file(
                &["Docs", &format!("{}_{i}.pdf", self.word())],
                self.size(100 << 10, 10 << 20),
            ));
        }
        self.multi(name, files)
    }

    fn game_repack(&mut self) -> Vec<u8> {
        let game = self.title(1..5);
        let name = format!(
            "{} [{}] [Repack]",
            game.replace('.', " "),
            *ANIME_GROUPS.choose(&mut self.rng).unwrap()
        );
        let mut files = vec![
            file(&["setup.exe"], self.size(2 << 20, 8 << 20)),
            file(&["MD5", "QuickSFV.EXE"], 103_424),
            file(&["MD5", "fitgirl-bins.md5"], self.size(1 << 10, 4 << 10)),
        ];
        for i in 1..=self.rng.random_range(3..40) {
            files.push(file(
                &[&format!("fg-{i:02}.bin")],
                self.size(500 << 20, 4 << 30),
            ));
        }
        for r in REDIST.sample(&mut self.rng, 3) {
            files.push(file(&["_Redist", r], self.size(1 << 20, 80 << 20)));
        }
        self.multi(name, files)
    }

    fn ebook_pack(&mut self) -> Vec<u8> {
        let topic = self.title(1..3).replace('.', " ");
        let n = self.rng.random_range(20..400);
        let files = (0..n)
            .map(|_| {
                let ext = *["epub", "pdf", "mobi"].choose(&mut self.rng).unwrap();
                file(
                    &[&format!("{}.{ext}", self.book_name())],
                    self.size(200 << 10, 40 << 20),
                )
            })
            .collect();
        self.multi(format!("{topic} Ebook Collection {n} Books"), files)
    }

    fn image_set(&mut self) -> Vec<u8> {
        let n = self.rng.random_range(30..400);
        let start = self.rng.random_range(1000..9000);
        let camera = self.rng.random_bool(0.5);
        let name = format!(
            "{} Photo Set {}",
            self.title(1..3).replace('.', " "),
            self.rng.random_range(1..300)
        );
        let files = (0..n)
            .map(|i| {
                let fname = if camera {
                    format!("IMG_{:04}.JPG", start + i)
                } else {
                    format!("{:03}.jpg", i + 1)
                };
                file(&[&fname], self.size(200 << 10, 12 << 20))
            })
            .collect();
        self.multi(name, files)
    }

    fn ambiguous(&mut self) -> Vec<u8> {
        let hash: String = (0..self.rng.random_range(6..41))
            .map(|_| char::from(b"0123456789abcdef"[self.rng.random_range(0..16)]))
            .collect();
        match self.rng.random_range(0..3) {
            0 => {
                let size = self.size(1 << 20, 4 << 30);
                self.single(format!("{hash}.zip"), size)
            }
            1 => {
                // Uninformative name hiding an informative file (§6).
                let inner = self.movie_name();
                let files = vec![file(
                    &[&format!("{inner}.mkv")],
                    self.size(500 << 20, 10 << 30),
                )];
                self.multi(hash, files)
            }
            _ => {
                let files = (1..=self.rng.random_range(2..6))
                    .map(|i| {
                        file(
                            &[
                                &format!("New Folder ({i})"),
                                &format!("{}.rar", self.word()),
                            ],
                            self.size(1 << 20, 1 << 30),
                        )
                    })
                    .collect();
                self.multi("New Folder".to_owned(), files)
            }
        }
    }

    fn mega_pack(&mut self) -> Vec<u8> {
        let n = self.rng.random_range(500..3000);
        let topic = self.title(1..3).replace('.', " ");
        let dirs: Vec<String> = (0..self.rng.random_range(5..60))
            .map(|_| self.title(1..3))
            .collect();
        let files = (0..n)
            .map(|i| {
                let dir = dirs.choose(&mut self.rng).unwrap().clone();
                let ext = *["mp3", "flac", "jpg", "pdf", "mkv", "mp4", "txt", "zip"]
                    .choose(&mut self.rng)
                    .unwrap();
                file(
                    &[
                        &dir,
                        &format!("{} {i:04}.{ext}", self.title(1..3).replace('.', " ")),
                    ],
                    self.size(10 << 10, 500 << 20),
                )
            })
            .collect();
        self.multi(format!("{topic} Mega Pack"), files)
    }

    fn cjk_season(&mut self) -> Vec<u8> {
        let show: String = (0..self.rng.random_range(2..7))
            .map(|_| *CJK.choose(&mut self.rng).unwrap())
            .collect();
        let season = ["第一季", "第二季", "第三季"]
            .choose(&mut self.rng)
            .unwrap();
        let name = format!("{show} {season} 1080P 国语中字");
        let legacy = self.rng.random_bool(0.5);
        let files: Vec<FileSpec> = (1..=self.rng.random_range(10..31))
            .map(|e| {
                let fname = format!("{show} 第{e:02}集.mp4");
                let size = self.size(200 << 20, 2 << 30);
                if legacy {
                    FileSpec {
                        path: vec![gbk(&fname)],
                        size,
                        utf8: None,
                    }
                } else {
                    file(&[&fname], size)
                }
            })
            .collect();
        if legacy {
            self.multi_raw(gbk(&name), files)
        } else {
            self.multi(name, files)
        }
    }

    /// A BEP 52 hybrid: v1 file list with padding aligning every file to a
    /// piece boundary, plus the v2 file tree with pieces roots.
    fn hybrid(&mut self) -> Vec<u8> {
        let show = self.title(1..4);
        let tail = self.release_tail();
        let name = format!("{show}.S01.{tail}");
        let episodes: Vec<(String, u64)> = (1..=self.rng.random_range(4..13))
            .map(|e| {
                (
                    format!("{show}.S01E{e:02}.{tail}.mkv"),
                    self.size(200 << 20, 3 << 30),
                )
            })
            .collect();
        let total: u64 = episodes.iter().map(|(_, s)| s).sum();
        let piece_len = piece_length(total).max(16 << 10);
        let mut v1 = Vec::new();
        let mut tree = BTreeMap::new();
        let mut covered = 0u64;
        for (i, (fname, size)) in episodes.iter().enumerate() {
            v1.push(dict(vec![
                ("length", int(*size)),
                ("path", path_list(&[fname.as_bytes().to_vec()])),
            ]));
            covered += size;
            let root: [u8; 32] = self.rng.random();
            tree.insert(
                fname.as_bytes().to_vec(),
                dict(vec![(
                    "",
                    dict(vec![
                        ("length", int(*size)),
                        ("pieces root", OwnedValue::Bytes(root.to_vec())),
                    ]),
                )]),
            );
            let pad = (piece_len - size % piece_len) % piece_len;
            if pad > 0 && i + 1 < episodes.len() {
                v1.push(dict(vec![
                    ("attr", OwnedValue::Bytes(b"p".to_vec())),
                    ("length", int(pad)),
                    (
                        "path",
                        path_list(&[b".pad".to_vec(), pad.to_string().into_bytes()]),
                    ),
                ]));
                covered += pad;
            }
        }
        to_vec(&dict(vec![
            ("file tree", OwnedValue::Dict(tree)),
            ("files", OwnedValue::List(v1)),
            ("meta version", int(2)),
            ("name", OwnedValue::Bytes(name.into_bytes())),
            ("piece length", int(piece_len)),
            ("pieces", pieces(covered, piece_len)),
        ]))
    }

    // ---- encoding ---------------------------------------------------------

    fn single(&mut self, name: String, size: u64) -> Vec<u8> {
        let piece_len = piece_length(size);
        let mut entries = vec![
            ("length", int(size)),
            ("name", OwnedValue::Bytes(name.into_bytes())),
            ("piece length", int(piece_len)),
            ("pieces", pieces(size, piece_len)),
        ];
        self.maybe_private(&mut entries);
        to_vec(&dict(entries))
    }

    fn multi(&mut self, name: String, files: Vec<FileSpec>) -> Vec<u8> {
        self.multi_raw(name.into_bytes(), files)
    }

    fn multi_raw(&mut self, name: Vec<u8>, files: Vec<FileSpec>) -> Vec<u8> {
        let total: u64 = files.iter().map(|f| f.size).sum();
        let piece_len = piece_length(total);
        let files = files
            .into_iter()
            .map(|f| {
                let mut e = vec![("length", int(f.size)), ("path", path_list(&f.path))];
                if let Some(u) = &f.utf8 {
                    e.push(("path.utf-8", path_list(u)));
                }
                dict(e)
            })
            .collect();
        let mut entries = vec![
            ("files", OwnedValue::List(files)),
            ("name", OwnedValue::Bytes(name)),
            ("piece length", int(piece_len)),
            ("pieces", pieces(total, piece_len)),
        ];
        self.maybe_private(&mut entries);
        to_vec(&dict(entries))
    }

    fn maybe_private(&mut self, entries: &mut Vec<(&'static str, OwnedValue)>) {
        if self.rng.random_bool(0.02) {
            entries.push(("private", int(1)));
            let src = *["PTP", "BTN", "RED", "OPS", "HDB"]
                .choose(&mut self.rng)
                .unwrap();
            entries.push(("source", OwnedValue::Bytes(src.as_bytes().to_vec())));
        }
    }
}

impl Iterator for Generator {
    type Item = SynthTorrent;

    fn next(&mut self) -> Option<SynthTorrent> {
        Some(self.next_torrent())
    }
}

/// SplitMix64: a stateless hash from `(seed, id, stream)` to 64 bits, so
/// swarm values don't draw from (and shift) the generator's stream.
fn mix(seed: u64, id: u64, stream: u64) -> u64 {
    let mut z = seed
        .wrapping_add(id.wrapping_mul(0x9e37_79b9_7f4a_7c15))
        .wrapping_add(stream.wrapping_mul(0xd1b5_4a32_d192_ed03));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A uniform draw in `[0, 1)`.
fn unit(seed: u64, id: u64, stream: u64) -> f64 {
    (mix(seed, id, stream) >> 11) as f64 / (1u64 << 53) as f64
}

/// Torrent `id`'s swarm as a scrape would leave it, deterministic in
/// `(seed, id)`: about 30% never scraped (`None`), 40% dead (all zero),
/// and the rest a heavy-tailed (power-law) live swarm reported by 1–20
/// trackers, with a DHT estimate near its size.
pub fn swarm(seed: u64, id: u64) -> Option<lodestar_store::SwarmHot> {
    let kind = unit(seed, id, 0);
    if kind < 0.30 {
        return None;
    }
    if kind < 0.70 {
        return Some(lodestar_store::SwarmHot {
            seeders: 0,
            leechers: 0,
            trackers: 0,
            dht: 0,
        });
    }
    // Pareto, α = 1.1: most live swarms are a handful of peers, a few are
    // tens of thousands.
    let seeders = ((1.0 - unit(seed, id, 1)).powf(-1.0 / 1.1)).min(100_000.0) as u32;
    let leechers = (f64::from(seeders) * unit(seed, id, 2) * 0.8) as u32;
    let trackers = 1 + (19.0 * unit(seed, id, 3).powi(2)) as u32;
    // BEP 33 filters saturate near 7,700.
    let dht = ((f64::from(seeders + leechers) * (0.3 + 1.2 * unit(seed, id, 4))) as u32).min(7_700);
    Some(lodestar_store::SwarmHot {
        seeders,
        leechers,
        trackers,
        dht,
    })
}

/// When torrent `id` was last seen in the DHT, unix seconds: spread over
/// the year before [`EPOCH`], deterministic in `(seed, id)`.
pub fn last_seen(seed: u64, id: u64) -> u32 {
    (EPOCH - (unit(seed, id, 5) * 365.0 * 86_400.0) as i64) as u32
}

/// Picks from weighted options.
fn pick<'o, T>(rng: &mut ChaCha8Rng, options: &'o [(T, u32)]) -> &'o T {
    let total: u32 = options.iter().map(|(_, w)| w).sum();
    let mut x = rng.random_range(0..total);
    for (v, w) in options {
        if x < *w {
            return v;
        }
        x -= w;
    }
    unreachable!("weights cover the range")
}

/// The smallest power of two ≥ 16 KiB giving at most ~2,000 pieces.
fn piece_length(total: u64) -> u64 {
    let mut len = 16u64 << 10;
    while total / len > 2000 && len < 16 << 20 {
        len *= 2;
    }
    len
}

/// Placeholder piece hashes of the right length; `pieces` is never stored.
fn pieces(total: u64, piece_len: u64) -> OwnedValue {
    OwnedValue::Bytes(vec![0; total.div_ceil(piece_len) as usize * 20])
}

fn file(segments: &[&str], size: u64) -> FileSpec {
    FileSpec {
        path: segments.iter().map(|s| s.as_bytes().to_vec()).collect(),
        size,
        utf8: None,
    }
}

fn gbk(s: &str) -> Vec<u8> {
    encoding_rs::GBK.encode(s).0.into_owned()
}

fn capitalize(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn int(v: u64) -> OwnedValue {
    OwnedValue::Int(v as i64)
}

fn dict(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn path_list(segments: &[Vec<u8>]) -> OwnedValue {
    OwnedValue::List(
        segments
            .iter()
            .map(|s| OwnedValue::Bytes(s.clone()))
            .collect(),
    )
}

const GROUPS: &[&str] = &[
    "NTb",
    "FLUX",
    "SPARKS",
    "GECKOS",
    "AMIABLE",
    "RARBG",
    "YIFY",
    "EVO",
    "CMRG",
    "TEPES",
    "NOGRP",
    "playWEB",
    "KOGi",
    "SMURF",
    "MZABI",
    "ROVERS",
    "DIMENSION",
    "LOL",
    "KILLERS",
    "TBS",
    "FGT",
    "ION10",
    "PSA",
    "QxR",
    "Tigole",
    "HONE",
    "BYNDR",
    "EDITH",
    "APEX",
    "MiNX",
    "SYNCOPY",
    "FraMeSToR",
    "CtrlHD",
    "DON",
    "HiFi",
    "W4NK3R",
    "TOMMY",
    "ELiTE",
    "GalaxyRG",
    "PHOENiX",
];

const ANIME_GROUPS: &[&str] = &[
    "SubsPlease",
    "Erai-raws",
    "HorribleSubs",
    "Judas",
    "EMBER",
    "ASW",
    "Ohys-Raws",
    "Commie",
    "FitGirl",
    "DODI",
    "ElAmigos",
    "KaOs",
    "xatab",
];

const LANGS: &[&str] = &[
    "English",
    "Spanish",
    "French",
    "German",
    "Italian",
    "Portuguese",
    "Russian",
    "Japanese",
    "Korean",
    "Chinese",
    "Arabic",
    "Dutch",
    "Polish",
    "Swedish",
    "Turkish",
];

const REDIST: &[&str] = &[
    "vcredist_x64.exe",
    "vcredist_x86.exe",
    "dxwebsetup.exe",
    "oalinst.exe",
    "PhysX.msi",
    "dotNetFx40_Full_setup.exe",
    "xnafx40_redist.msi",
];

const CJK: &[&str] = &[
    "天", "地", "人", "大", "小", "山", "水", "火", "风", "云", "雨", "雪", "花", "月", "星", "日",
    "春", "夏", "秋", "冬", "东", "西", "南", "北", "中", "国", "家", "爱", "心", "梦", "龙", "凤",
    "虎", "侠", "剑", "王", "后", "城", "海", "江", "河", "林", "白", "红", "金", "长", "安", "乐",
    "传", "记", "之", "的", "我", "你", "他", "新", "老", "少", "年", "生",
];

const WORDS: &[&str] = &[
    "the",
    "of",
    "and",
    "night",
    "day",
    "dark",
    "light",
    "king",
    "queen",
    "war",
    "love",
    "house",
    "game",
    "lost",
    "last",
    "first",
    "city",
    "black",
    "white",
    "red",
    "blue",
    "green",
    "golden",
    "silver",
    "iron",
    "stone",
    "fire",
    "ice",
    "water",
    "storm",
    "wind",
    "shadow",
    "ghost",
    "dead",
    "living",
    "secret",
    "hidden",
    "broken",
    "fallen",
    "rising",
    "empire",
    "kingdom",
    "world",
    "star",
    "moon",
    "sun",
    "sky",
    "sea",
    "ocean",
    "river",
    "mountain",
    "forest",
    "island",
    "road",
    "street",
    "station",
    "hotel",
    "school",
    "office",
    "hospital",
    "prison",
    "castle",
    "tower",
    "bridge",
    "garden",
    "wolf",
    "dragon",
    "lion",
    "tiger",
    "bear",
    "eagle",
    "raven",
    "snake",
    "spider",
    "horse",
    "dog",
    "cat",
    "man",
    "woman",
    "boy",
    "girl",
    "child",
    "father",
    "mother",
    "brother",
    "sister",
    "family",
    "friends",
    "lovers",
    "strangers",
    "killer",
    "hunter",
    "soldier",
    "doctor",
    "detective",
    "agent",
    "spy",
    "thief",
    "pirate",
    "witch",
    "wizard",
    "prince",
    "princess",
    "knight",
    "warrior",
    "legend",
    "story",
    "tale",
    "chronicles",
    "saga",
    "rise",
    "fall",
    "return",
    "revenge",
    "redemption",
    "escape",
    "journey",
    "quest",
    "mission",
    "protocol",
    "code",
    "project",
    "file",
    "case",
    "affair",
    "theory",
    "paradox",
    "machine",
    "robot",
    "android",
    "planet",
    "galaxy",
    "space",
    "time",
    "future",
    "past",
    "forever",
    "never",
    "always",
    "tomorrow",
    "yesterday",
    "midnight",
    "morning",
    "evening",
    "summer",
    "winter",
    "spring",
    "autumn",
    "blood",
    "bone",
    "heart",
    "soul",
    "mind",
    "dream",
    "nightmare",
    "memory",
    "silence",
    "echo",
    "thunder",
    "lightning",
    "crown",
    "throne",
    "sword",
    "shield",
    "arrow",
    "bullet",
    "gun",
    "money",
    "gold",
    "diamond",
    "crystal",
    "glass",
    "paper",
    "letter",
    "book",
    "song",
    "dance",
    "music",
    "party",
    "wedding",
    "funeral",
    "holiday",
    "christmas",
    "vacation",
    "weekend",
    "monday",
    "friday",
    "zero",
    "one",
    "two",
    "three",
    "seven",
    "hundred",
    "thousand",
    "million",
    "big",
    "little",
    "great",
    "small",
    "new",
    "old",
    "young",
    "wild",
    "crazy",
    "happy",
    "sad",
    "angry",
    "lonely",
    "perfect",
    "beautiful",
    "ugly",
    "strange",
    "dangerous",
    "deadly",
    "final",
    "ultimate",
    "total",
    "absolute",
    "eternal",
    "infinite",
    "american",
    "british",
    "french",
    "italian",
    "chinese",
    "japanese",
    "london",
    "paris",
    "tokyo",
    "york",
    "vegas",
    "texas",
    "california",
    "boston",
    "chicago",
    "miami",
    "alpha",
    "beta",
    "omega",
    "delta",
    "sigma",
    "nexus",
    "vertex",
    "matrix",
];

// ---- identification answers (ENR-11) -------------------------------------

/// What [`answer`] imitates: the kind of work an identification plugin
/// looked up. ENR-10 sends only these kinds (Movie, TV, Music, Audiobook,
/// Ebook, Comic, Software and Game); Images, Video, Other and Adult are
/// never looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Work {
    Film,
    Show,
    /// A show whose name starts with a `[Group]` tag: AniDB and AniList
    /// ids as well as the TV ones.
    Anime,
    Music,
    Book,
    Software,
    Game,
}

/// The realistic share of identifiable torrents that hold a served answer,
/// in percent (`docs/decisions/2026-11-27-X-enr11-size-measure.md`).
pub const REALISTIC_IDENTIFIED_PERCENT: u32 = 60;

/// The `unit` stream that picks identified torrents; the answer itself
/// draws from its own generator, so neither shifts the torrent stream or
/// the swarm values.
const IDENTIFIED_STREAM: u64 = 16;
const ANSWER_STREAM: u64 = 17;

/// Whether torrent `id` holds an answer when `percent` of identifiable
/// torrents do, deterministic in `(seed, id)`. Nested: a torrent
/// identified at one share is identified at every higher one.
pub fn identified(seed: u64, id: u64, percent: u32) -> bool {
    unit(seed, id, IDENTIFIED_STREAM) * 100.0 < f64::from(percent.min(100))
}

/// The title a plugin would be asked about: the name's words up to the
/// first one holding a digit or a bracket (the year, `S01E02`, the
/// resolution, a `[tag]`), after any leading `[Group]` tags. A name with
/// none is cut to its first 40 characters.
pub fn work_title(name: &str) -> String {
    let mut rest = name.trim();
    while let Some(tail) = rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
        rest = tail.1.trim_start();
    }
    let mut words = Vec::new();
    for tok in rest.split(['.', ' ', '_']) {
        if tok.is_empty() || tok == "-" {
            continue;
        }
        if tok.chars().any(|c| c.is_ascii_digit() || "[(".contains(c)) {
            break;
        }
        words.push(tok);
        if words.len() == 8 {
            break;
        }
    }
    if words.is_empty() {
        let cut: String = rest.chars().take(40).collect();
        let cut = cut.trim();
        return if cut.is_empty() {
            "Untitled".to_owned()
        } else {
            cut.to_owned()
        };
    }
    words.join(" ")
}

/// An identification answer for torrent `id`, as an ENR-10 plugin's
/// `Identification::into_claims` stores it: one to three ids of the
/// schemes `work` has, a canonical title, alternative titles in several
/// scripts and lengths, genres, a year and a match confidence. Everything
/// but the confidence depends only on `(seed, work, title)`, so the
/// releases of one work share their answer, as they do in a real
/// catalogue. Always in stored form (`claims::encode` accepts it).
pub fn answer(seed: u64, id: u64, work: Work, title: &str) -> lodestar_store::claims::Claims {
    use lodestar_store::claims::{AltTitle, Claims, Details, ExternalId};

    let key = title
        .bytes()
        .chain([work as u8])
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    let mut rng = ChaCha8Rng::seed_from_u64(mix(seed, key, ANSWER_STREAM));

    let schemes: &[&str] = match work {
        Work::Film => &["imdb", "tmdb", "wikidata"],
        Work::Show => &["tvdb", "imdb", "tmdb", "wikidata"],
        Work::Anime => &["anidb", "anilist", "tvdb", "wikidata"],
        Work::Music => &["mbid", "discogs", "wikidata"],
        Work::Book => &["olid", "wikidata"],
        Work::Software => &["wikidata", "igdb"],
        Work::Game => &["igdb", "steam", "wikidata"],
    };
    let n = (*pick(&mut rng, &[(1usize, 30), (2, 40), (3, 30)])).min(schemes.len());
    let mut chosen: Vec<&str> = schemes.sample(&mut rng, n).copied().collect();
    chosen.sort_unstable();
    let ids = chosen
        .into_iter()
        .map(|scheme| ExternalId {
            scheme: scheme.to_owned(),
            value: id_value(&mut rng, scheme),
        })
        .collect();

    // Most catalogues hold the title the release names; some add a
    // subtitle or an article.
    let canonical = match rng.random_range(0..100) {
        0..65 => title.to_owned(),
        65..85 => format!("{title}: {}", latin_words(&mut rng, 1..4, false)),
        _ => format!("The {title}"),
    };
    let count = match *pick(&mut rng, &[(0u8, 15), (1, 30), (2, 30), (3, 18), (4, 7)]) {
        0 => 0,
        1 => rng.random_range(1..=3),
        2 => rng.random_range(4..=8),
        3 => rng.random_range(9..=16),
        _ => rng.random_range(17..=32),
    };
    let words = title.split(' ').count();
    let mut alt_titles: Vec<AltTitle> = Vec::with_capacity(count);
    for _ in 0..count {
        let alt = if rng.random_range(0..100) < 25 {
            // A Latin-script variant of the original title.
            let (language, t) = match rng.random_range(0..4) {
                0 => (
                    "en",
                    format!(
                        "{title} ({})",
                        ["US", "UK", "AU", "CA"].choose(&mut rng).unwrap()
                    ),
                ),
                1 => ("en-gb", title.to_uppercase()),
                2 => (
                    "und",
                    format!("{title} {}", latin_words(&mut rng, 1..3, false)),
                ),
                _ => ("en", latin_words(&mut rng, 1..5, false)),
            };
            AltTitle {
                language: language.to_owned(),
                title: t,
            }
        } else {
            let (language, script) = *pick(&mut rng, LANGUAGES);
            let len = (words + rng.random_range(0..3))
                .saturating_sub(1)
                .clamp(1, 8);
            AltTitle {
                language: language.to_owned(),
                title: script_words(&mut rng, script, len),
            }
        };
        if !alt_titles.contains(&alt) {
            alt_titles.push(alt);
        }
    }
    let genre_pool: &[&str] = match work {
        Work::Film | Work::Show | Work::Anime => &[
            "drama",
            "comedy",
            "action",
            "thriller",
            "science fiction",
            "horror",
            "romance",
            "documentary",
            "animation",
            "crime",
            "fantasy",
            "adventure",
        ],
        Work::Music => &[
            "rock",
            "electronic",
            "jazz",
            "hip hop",
            "classical",
            "pop",
            "metal",
            "folk",
        ],
        Work::Book => &[
            "fiction",
            "history",
            "science",
            "biography",
            "fantasy",
            "mystery",
        ],
        Work::Software | Work::Game => &[
            "strategy",
            "action",
            "role-playing",
            "simulation",
            "utility",
            "puzzle",
        ],
    };
    let g = rng.random_range(1..=3);
    let genres = genre_pool
        .sample(&mut rng, g)
        .map(|g| (*g).to_owned())
        .collect();
    let year = rng.random_range(1950..=2025);
    let confidence = 60 + (unit(seed, id, ANSWER_STREAM) * 41.0) as u8;
    Claims {
        ids,
        details: Some(Box::new(Details {
            canonical_title: Some(canonical),
            alt_titles,
            genres,
            year: Some(year),
            match_confidence: Some(confidence.min(100)),
            ..Details::default()
        })),
        ..Claims::default()
    }
}

/// A value in `scheme`'s one stored spelling (`claims::id_ok`).
fn id_value(rng: &mut ChaCha8Rng, scheme: &str) -> String {
    let n = |rng: &mut ChaCha8Rng, hi: u64| rng.random_range(1..hi).to_string();
    match scheme {
        "imdb" => {
            let v = rng.random_range(1..40_000_000u64);
            if v < 10_000_000 {
                format!("tt{v:07}")
            } else {
                format!("tt{v}")
            }
        }
        "tmdb" => n(rng, 1_400_000),
        "tvdb" => n(rng, 460_000),
        "anidb" => n(rng, 20_000),
        "anilist" => n(rng, 190_000),
        "igdb" => n(rng, 320_000),
        "steam" => n(rng, 3_400_000),
        "wikidata" => format!("Q{}", n(rng, 130_000_000)),
        "olid" => format!("OL{}W", n(rng, 40_000_000)),
        "discogs" => format!("{}{}", ["r", "m"].choose(rng).unwrap(), n(rng, 33_000_000)),
        "mbid" => {
            let h: String = (0..32)
                .map(|_| char::from(b"0123456789abcdef"[rng.random_range(0..16)]))
                .collect();
            format!(
                "{}-{}-{}-{}-{}",
                &h[..8],
                &h[8..12],
                &h[12..16],
                &h[16..20],
                &h[20..]
            )
        }
        other => unreachable!("not an enrichment scheme: {other}"),
    }
}

/// A script's alphabet, as Unicode ranges, and how long its words run.
#[derive(Debug, Clone, Copy)]
enum Script {
    /// Accented Latin: the European translations.
    Latin,
    Cyrillic,
    Greek,
    Arabic,
    Hebrew,
    Devanagari,
    Thai,
    /// Han characters from the generator's own set, 1–3 to a word.
    Han,
    /// Kana with some Han.
    Japanese,
    Hangul,
}

/// Translation languages, by how often catalogues hold them (tenths of a
/// percent of translated titles).
const LANGUAGES: &[((&str, Script), u32)] = &[
    (("de", Script::Latin), 80),
    (("fr", Script::Latin), 80),
    (("es", Script::Latin), 80),
    (("it", Script::Latin), 60),
    (("pt-br", Script::Latin), 60),
    (("pl", Script::Latin), 40),
    (("nl", Script::Latin), 30),
    (("sv", Script::Latin), 25),
    (("tr", Script::Latin), 25),
    (("cs", Script::Latin), 20),
    (("hu", Script::Latin), 20),
    (("ru", Script::Cyrillic), 80),
    (("uk", Script::Cyrillic), 30),
    (("bg", Script::Cyrillic), 15),
    (("el", Script::Greek), 20),
    (("ar", Script::Arabic), 30),
    (("fa", Script::Arabic), 15),
    (("he", Script::Hebrew), 20),
    (("hi", Script::Devanagari), 20),
    (("th", Script::Thai), 15),
    (("zh", Script::Han), 50),
    (("zh-hant", Script::Han), 30),
    (("ja", Script::Japanese), 60),
    (("ko", Script::Hangul), 40),
    (("und", Script::Latin), 30),
];

/// `n` words of `script` from its fixed vocabulary, space-separated (Han
/// and Japanese without spaces).
fn script_words(rng: &mut ChaCha8Rng, script: Script, n: usize) -> String {
    if let Script::Latin = script {
        return latin_words(rng, n..n + 1, true);
    }
    let vocab = &vocabularies()[script as usize];
    let words: Vec<&str> = (0..n)
        .map(|_| vocab.choose(rng).unwrap().as_str())
        .collect();
    match script {
        Script::Han | Script::Japanese => words.concat(),
        _ => words.join(" "),
    }
}

/// Capitalised words from the shared word list; with `accents`, some
/// from the accented Latin vocabulary instead.
fn latin_words(rng: &mut ChaCha8Rng, n: std::ops::Range<usize>, accents: bool) -> String {
    let n = rng.random_range(n);
    (0..n)
        .map(|_| {
            if accents && rng.random_range(0..2) == 0 {
                capitalize(vocabularies()[Script::Latin as usize].choose(rng).unwrap())
            } else {
                capitalize(WORDS.choose(rng).unwrap())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Each script's vocabulary: 600 words drawn once from a fixed seed, so
/// translated titles share words across the corpus as real ones do.
fn vocabularies() -> &'static [Vec<String>] {
    static V: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let mut rng = ChaCha8Rng::seed_from_u64(0x001d_3e11);
        let latin: Vec<char> = "abcdefghijklmnoprstuvzéèàüöäñçøłšžő".chars().collect();
        let range = |a: u32, b: u32| (a..=b).filter_map(char::from_u32).collect::<Vec<_>>();
        let han: Vec<char> = CJK.iter().flat_map(|s| s.chars()).collect();
        let mut japanese = range(0x3041, 0x3093);
        japanese.extend(range(0x30a1, 0x30f3));
        japanese.extend(&han);
        let alphabets: [(Vec<char>, std::ops::RangeInclusive<usize>); 10] = [
            (latin, 3..=9),
            (range(0x0430, 0x044f), 3..=9),
            (range(0x03b1, 0x03c9), 3..=9),
            (range(0x0627, 0x064a), 2..=7),
            (range(0x05d0, 0x05ea), 2..=6),
            (range(0x0915, 0x0939), 2..=6),
            (range(0x0e01, 0x0e2e), 2..=6),
            (han, 1..=3),
            (japanese, 2..=5),
            (range(0xac00, 0xd7a3), 1..=4),
        ];
        alphabets
            .into_iter()
            .map(|(chars, len)| {
                (0..600)
                    .map(|_| {
                        let n = rng.random_range(len.clone());
                        (0..n).map(|_| *chars.choose(&mut rng).unwrap()).collect()
                    })
                    .collect()
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_metainfo::Info;

    #[test]
    fn swarms_are_deterministic_and_heavy_tailed() {
        let n = 100_000u64;
        let all: Vec<_> = (1..=n).map(|id| swarm(9, id)).collect();
        assert_eq!(all, (1..=n).map(|id| swarm(9, id)).collect::<Vec<_>>());
        assert_ne!(all, (1..=n).map(|id| swarm(10, id)).collect::<Vec<_>>());
        let share = |f: &dyn Fn(&Option<lodestar_store::SwarmHot>) -> bool| {
            all.iter().filter(|s| f(s)).count() as f64 / n as f64
        };
        let never = share(&|s| s.is_none());
        let dead = share(&|s| s.is_some_and(|s| s.seeders == 0));
        assert!((0.28..0.32).contains(&never), "{never}");
        assert!((0.38..0.42).contains(&dead), "{dead}");
        let live: Vec<_> = all.iter().flatten().filter(|s| s.seeders > 0).collect();
        assert!(live.iter().all(|s| (1..=20).contains(&s.trackers)));
        assert!(live.iter().all(|s| s.dht <= 7_700));
        let max = live.iter().map(|s| s.seeders).max().unwrap();
        let median = {
            let mut v: Vec<_> = live.iter().map(|s| s.seeders).collect();
            v.sort_unstable();
            v[v.len() / 2]
        };
        assert!(median <= 3 && max >= 10_000, "median {median}, max {max}");
        for id in 1..1000 {
            let t = i64::from(last_seen(9, id));
            assert!((EPOCH - 365 * 86_400..=EPOCH).contains(&t));
        }
    }

    #[test]
    fn deterministic() {
        let a: Vec<_> = Generator::new(42).take(50).map(|t| t.info).collect();
        let b: Vec<_> = Generator::new(42).take(50).map(|t| t.info).collect();
        let c: Vec<_> = Generator::new(43).take(50).map(|t| t.info).collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn every_kind_parses_cleanly() {
        let mut seen = std::collections::HashSet::new();
        for t in Generator::new(7).take(5000) {
            let info = Info::parse(&t.info).unwrap_or_else(|e| panic!("{:?}: {e}", t.kind));
            assert!(
                info.warnings.is_empty(),
                "{:?}: {:?}",
                t.kind,
                info.warnings
            );
            assert!(t.discovered_at <= EPOCH);
            seen.insert(t.kind);
        }
        assert_eq!(seen.len(), MIX.len(), "all kinds generated");
    }

    #[test]
    fn mean_file_count_is_near_thirty() {
        let n = 20_000;
        let files: usize = Generator::new(1)
            .take(n)
            .map(|t| Info::parse(&t.info).unwrap().files.len())
            .sum();
        let mean = files as f64 / n as f64;
        assert!(
            (25.0..35.0).contains(&mean),
            "mean files per torrent: {mean}"
        );
    }

    #[test]
    fn hybrids_and_legacy_names_are_realistic() {
        let (mut hybrids, mut legacy, mut legacy_ok) = (0, 0, 0);
        for t in Generator::new(3).take(20_000) {
            let info = Info::parse(&t.info).unwrap();
            match t.kind {
                Kind::HybridV2 => {
                    assert!(info.v2.is_some());
                    assert!(
                        info.files
                            .iter()
                            .all(|f| f.is_padding() || f.pieces_root.is_some())
                    );
                    hybrids += 1;
                }
                Kind::CjkSeason
                    if !info.name.raw.is_ascii()
                        && std::str::from_utf8(&info.name.raw).is_err() =>
                {
                    legacy += 1;
                    legacy_ok += usize::from(info.name.decode().contains("国语中字"));
                }
                _ => {}
            }
        }
        assert!(hybrids > 0);
        // Charset detection on short CJK names isn't perfect (a few come out
        // as ISO-8859-5); raw bytes are kept so they can be re-decoded.
        assert!(
            legacy > 100 && legacy_ok * 100 >= legacy * 95,
            "{legacy_ok}/{legacy}"
        );
    }

    #[test]
    fn work_titles_stop_at_the_release_tail() {
        assert_eq!(work_title("Night.King.2019.1080p.WEB-DL-GRP"), "Night King");
        assert_eq!(work_title("Dark.Sea.S02E05.720p.HDTV"), "Dark Sea");
        assert_eq!(
            work_title("[SubsPlease] Iron Moon - 05 (1080p)"),
            "Iron Moon"
        );
        assert_eq!(work_title("abcdef0123"), "abcdef0123");
        assert_eq!(work_title(""), "Untitled");
        assert_eq!(work_title("国语中字 天地"), "国语中字 天地");
    }

    #[test]
    fn identified_shares_are_nested_and_near_their_percent() {
        let n = 50_000u64;
        let count = |p| (1..=n).filter(|&id| identified(5, id, p)).count() as f64 / n as f64;
        assert_eq!(count(0), 0.0);
        assert_eq!(count(100), 1.0);
        let at = count(REALISTIC_IDENTIFIED_PERCENT);
        assert!((at - 0.6).abs() < 0.01, "{at}");
        for id in 1..=n {
            if identified(5, id, 30) {
                assert!(identified(5, id, 31), "{id}");
            }
        }
    }

    #[test]
    fn answers_are_stored_form_and_realistic() {
        use lodestar_store::claims::{self, ENRICHMENT_SCHEMES, id_ok};
        let works = [
            Work::Film,
            Work::Show,
            Work::Anime,
            Work::Music,
            Work::Book,
            Work::Software,
            Work::Game,
        ];
        let (mut answers, mut ids, mut alts, mut scripts) =
            (0, 0, 0, std::collections::HashSet::new());
        let mut longest = 0;
        for id in 0..3_000u64 {
            let work = works[(id % 7) as usize];
            let title = format!("Night King {}", id % 400);
            let c = answer(11, id, work, &title);
            // Stored form: encoding validates every limit and spelling.
            let blob = claims::encode(&c, 3).unwrap_or_else(|e| panic!("{work:?} {id}: {e}"));
            assert_eq!(claims::decode(&blob).unwrap(), c);
            assert!((1..=3).contains(&c.ids.len()), "{:?}", c.ids);
            assert!(
                c.ids
                    .iter()
                    .all(|i| ENRICHMENT_SCHEMES.contains(&i.scheme.as_str())
                        && id_ok(&i.scheme, &i.value))
            );
            let d = c.details.as_ref().unwrap();
            assert!(d.match_confidence.is_some_and(|m| (60..=100).contains(&m)));
            assert!(d.alt_titles.len() <= claims::MAX_ALT_TITLES);
            assert!(d.canonical_title.is_some());
            for a in &d.alt_titles {
                scripts.insert(
                    a.title
                        .chars()
                        .find(|c| !c.is_ascii())
                        .map(|c| c as u32 >> 8),
                );
                longest = longest.max(a.title.chars().count());
            }
            answers += 1;
            ids += c.ids.len();
            alts += d.alt_titles.len();
            // The same work gets the same answer but its own confidence.
            let again = answer(11, id + 7_000, work, &title);
            assert_eq!(again.ids, c.ids);
            assert_eq!(again.details.as_ref().unwrap().alt_titles, d.alt_titles);
        }
        let (ids, alts) = (ids as f64 / answers as f64, alts as f64 / answers as f64);
        assert!((1.6..2.4).contains(&ids), "ids per answer {ids}");
        assert!(
            (4.0..9.0).contains(&alts),
            "alternative titles per answer {alts}"
        );
        assert!(scripts.len() >= 8, "scripts {scripts:?}");
        assert!(longest >= 20, "longest {longest}");
    }
}
