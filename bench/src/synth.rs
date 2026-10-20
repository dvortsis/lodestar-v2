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

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_metainfo::Info;

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
}
