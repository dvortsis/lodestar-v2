//! Structural fingerprints from the file tree alone (§6): disc layouts,
//! rip and scene conventions, game repacks. They feed the content type,
//! the junk score and the `has:` filters.

use crate::FileInfo;
use crate::ext::{self, Category};

/// A set of fingerprints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Structure(u32);

macro_rules! fingerprints {
    ($($(#[$m:meta])* $name:ident = $bit:literal, $key:literal;)*) => {
        impl Structure {
            $($(#[$m])* pub const $name: Structure = Structure(1 << $bit);)*

            /// Every fingerprint with its name, as `has:` and the API spell it.
            pub const NAMED: &'static [(Structure, &'static str)] = &[$((Structure::$name, $key)),*];
        }
    };
}

fingerprints! {
    /// `BDMV/` (a Blu-ray folder structure).
    BLURAY = 0, "bluray";
    /// `VIDEO_TS/` or `.vob` with `.ifo`.
    DVD = 1, "dvd";
    /// A `.cue` sheet beside lossless audio: an image rip.
    CUE = 2, "cue";
    SFV = 3, "sfv";
    PAR2 = 4, "par2";
    /// `.r00` or `.part2.rar` style split archives.
    MULTIPART = 5, "multipart";
    /// `steamapps/` or `.vpk` files.
    STEAM = 6, "steam";
    /// `setup.exe` beside `.bin` data parts (FitGirl/DODI-style repacks).
    REPACK = 7, "repack";
    /// A `Sample/` folder or a sample video.
    SAMPLE = 8, "sample";
    /// Subtitle files or a `Subs/` folder.
    SUBS = 9, "subs";
    /// A `Proof/` folder (scene proof scans).
    PROOF = 10, "proof";
    NFO = 11, "nfo";
    /// Two or more comic archives (`.cbz`/`.cbr`).
    COMICS = 12, "comics";
    /// Camera-named images (`IMG_1234.jpg`, `DSC_0001`, `DCIM/`).
    PHOTOS = 13, "photos";
}

impl Structure {
    pub const fn empty() -> Structure {
        Structure(0)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn from_bits(bits: u32) -> Structure {
        Structure(bits)
    }

    pub const fn contains(self, other: Structure) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Structure) {
        self.0 |= other.0;
    }

    /// The fingerprint called `name` (as `has:` spells it).
    pub fn from_name(name: &str) -> Option<Structure> {
        Self::NAMED
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(s, _)| *s)
    }

    /// The names of the fingerprints present, in a fixed order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMED
            .iter()
            .filter(move |(s, _)| self.contains(*s))
            .map(|(_, n)| *n)
    }

    pub fn scan(files: &[FileInfo]) -> Structure {
        let mut s = Structure::empty();
        let (mut cue, mut lossless, mut vob, mut ifo, mut setup, mut bin) =
            (false, false, false, false, false, false);
        let (mut rar_parts, mut comics, mut camera) = (0u32, 0u32, 0u32);
        for f in files.iter().filter(|f| !f.padding) {
            let Some((name, dirs)) = f.path.split_last() else {
                continue;
            };
            for d in dirs {
                let is = |w: &str| d.eq_ignore_ascii_case(w);
                if is("bdmv") {
                    s.insert(Structure::BLURAY);
                } else if is("video_ts") {
                    s.insert(Structure::DVD);
                } else if is("steamapps") {
                    s.insert(Structure::STEAM);
                } else if is("sample") || is("samples") {
                    s.insert(Structure::SAMPLE);
                } else if is("subs") || is("subtitles") || is("sub") {
                    s.insert(Structure::SUBS);
                } else if is("proof") {
                    s.insert(Structure::PROOF);
                } else if is("dcim") {
                    s.insert(Structure::PHOTOS);
                }
            }
            let ext = ext::extension(name);
            let e = ext.as_ref().map_or("", |e| e.as_str());
            match e {
                "cue" => cue = true,
                "flac" | "ape" | "wv" | "wav" | "tta" | "tak" => lossless = true,
                "vob" => vob = true,
                "ifo" => ifo = true,
                "bin" => bin = true,
                "sfv" => s.insert(Structure::SFV),
                "par2" => s.insert(Structure::PAR2),
                "nfo" => s.insert(Structure::NFO),
                "vpk" => s.insert(Structure::STEAM),
                "cbz" | "cbr" | "cb7" => comics += 1,
                "rar" if contains_ignore_case(name, ".part") => rar_parts += 1,
                _ => {}
            }
            let category = ext::category_of(e);
            // A numbered part (`.r00`, `.001`) means a split set on its own.
            if ext::is_split_part(e) {
                rar_parts += 2;
            }
            if category == Category::Subtitle {
                s.insert(Structure::SUBS);
            }
            if category == Category::Video && contains_ignore_case(name, "sample") {
                s.insert(Structure::SAMPLE);
            }
            if name.eq_ignore_ascii_case("setup.exe") {
                setup = true;
            }
            if category == Category::Image && camera_name(name) {
                camera += 1;
            }
        }
        if cue && lossless {
            s.insert(Structure::CUE);
        }
        if vob && ifo {
            s.insert(Structure::DVD);
        }
        if setup && bin {
            s.insert(Structure::REPACK);
        }
        if rar_parts >= 2 {
            s.insert(Structure::MULTIPART);
        }
        if comics >= 2 {
            s.insert(Structure::COMICS);
        }
        if camera >= 5 {
            s.insert(Structure::PHOTOS);
        }
        s
    }
}

fn contains_ignore_case(hay: &str, needle: &str) -> bool {
    hay.as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// `IMG_1234.jpg`, `DSC01234.JPG`, `PXL_20240101_…`, `P1010001.JPG`.
fn camera_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let rest = [
        "IMG_", "IMG-", "DSC_", "DSCN", "DSCF", "DSC", "PXL_", "GOPR", "DJI_", "P",
    ]
    .iter()
    .find_map(|p| upper.strip_prefix(p));
    rest.is_some_and(|r| r.bytes().take_while(u8::is_ascii_digit).count() >= 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<FileInfo> {
        paths
            .iter()
            .map(|p| FileInfo::new(p.split('/').map(str::to_owned).collect(), 1))
            .collect()
    }

    fn scan(paths: &[&str]) -> Vec<&'static str> {
        Structure::scan(&files(paths)).names().collect()
    }

    #[test]
    fn fingerprints() {
        assert_eq!(
            scan(&["Movie/BDMV/STREAM/00001.m2ts", "Movie/BDMV/index.bdmv"]),
            ["bluray"]
        );
        assert_eq!(scan(&["VIDEO_TS/VTS_01_1.VOB"]), ["dvd"]);
        assert_eq!(scan(&["VTS_01_1.vob", "VTS_01_0.IFO"]), ["dvd"]);
        assert_eq!(
            scan(&["Album/CD.cue", "Album/CD.flac", "Album/CD.log"]),
            ["cue"]
        );
        assert_eq!(scan(&["a.cue", "a.bin"]), Vec::<&str>::new());
        assert_eq!(
            scan(&[
                "Movie/movie.r00",
                "Movie/movie.r01",
                "Movie/movie.rar",
                "Movie/movie.sfv",
                "Movie/movie.nfo"
            ]),
            ["sfv", "multipart", "nfo"]
        );
        assert_eq!(scan(&["x.part1.rar", "x.part2.rar"]), ["multipart"]);
        assert_eq!(scan(&["only.rar"]), Vec::<&str>::new());
        assert_eq!(
            scan(&["Game/setup.exe", "Game/fg-01.bin", "Game/fg-02.bin"]),
            ["repack"]
        );
        assert_eq!(
            scan(&[
                "Movie/Sample/movie-sample.mkv",
                "Movie/movie.mkv",
                "Movie/Subs/movie.idx"
            ]),
            ["sample", "subs"]
        );
        assert_eq!(
            scan(&["Movie.2019.sample.mkv", "Proof/proof.jpg"]),
            ["sample", "proof"]
        );
        assert_eq!(scan(&["Vol 1.cbz", "Vol 2.cbr"]), ["comics"]);
        assert_eq!(
            scan(&[
                "IMG_0001.jpg",
                "IMG_0002.jpg",
                "IMG_0003.jpg",
                "DSC01234.JPG",
                "PXL_20240101_1.jpg"
            ]),
            ["photos"]
        );
        assert_eq!(scan(&["game/steamapps/common/x.vpk"]), ["steam"]);
    }

    #[test]
    fn padding_is_ignored() {
        let mut f = files(&["a.nfo"]);
        f[0].padding = true;
        assert_eq!(Structure::scan(&f), Structure::empty());
    }
}
