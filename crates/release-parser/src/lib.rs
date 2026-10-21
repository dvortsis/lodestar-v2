//! Release-name parser (architecture §6 "Parsed release", §14 M3).
//!
//! Reads what a torrent or file name says about its content: the title,
//! year, season and episode, video and audio quality, languages, edition,
//! release group, and the music, software and book fields. One pure
//! function, [`parse`], serves the torrent name, the dominant files and the
//! top folder alike; picking the most informative of those (the
//! *effective title*) is classification's job.
//!
//! How it works:
//! - Split the name into words, remembering the separators and brackets
//!   between them.
//! - Find markers (`1080p`, `S01E02`, `WEB-DL`, `FRENCH`, …) with a
//!   hand-written lexicon and number matchers. No regex crate: names are
//!   short and the matchers are a few comparisons each.
//! - The title is the text before the first *strong* marker. *Weak*
//!   markers are words that also occur in titles (`Web`, `Extended`,
//!   `French`); they only count after the title, in brackets, in ALL CAPS,
//!   or directly before a strong marker.
//! - The release group is the `-GROUP` suffix, a leading `[Group]`, or a
//!   trailing bracket that isn't metadata.
//!
//! Input is untrusted. It's truncated to [`MAX_INPUT_BYTES`], word count is
//! capped, nothing allocates in proportion to anything but the input, and
//! parsing never panics (fuzzed by `fuzz/release`).

mod ambiguous;
mod lexicon;
mod parse;
mod token;

pub use ambiguous::is_uninformative;
pub use parse::parse;

/// Bumped whenever a change alters what any name parses to. It's part of
/// the index directory name (§8 "Index evolution"), so a new parser
/// rebuilds the index in the background.
pub const PARSER_VERSION: u32 = 1;

/// Longer names are cut at a char boundary before parsing. Real release
/// names are well under this; anything longer is a path or junk, and the
/// part that matters comes first.
pub const MAX_INPUT_BYTES: usize = 512;

/// An inclusive range of numbers: one season or `1-10`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub first: u16,
    pub last: u16,
}

impl Span {
    pub fn one(n: u16) -> Span {
        Span { first: n, last: n }
    }

    fn render(self) -> String {
        if self.first == self.last {
            self.first.to_string()
        } else {
            format!("{}-{}", self.first, self.last)
        }
    }
}

/// The air date of a date-based episode (talk shows, news).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

/// Audio channel layout, `5.1` as `Channels(5, 1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Channels(pub u8, pub u8);

macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident => $s:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name { $($var),* }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var),*];

            /// The canonical lowercase name, as indexed and as the query
            /// language accepts it.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$var => $s),* }
            }
        }
    };
}

str_enum! {
    Resolution {
        R480p => "480p", R576p => "576p", R720p => "720p", R1080i => "1080i",
        R1080p => "1080p", R1440p => "1440p", R2160p => "2160p", R4320p => "4320p",
    }
}

str_enum! {
    /// Where the video came from, roughly worst to best.
    Source {
        Cam => "cam", Telesync => "telesync", Telecine => "telecine",
        Screener => "screener", Workprint => "workprint", Vhs => "vhs", Tv => "tv",
        Dvd => "dvd", HdRip => "hdrip", BluRay => "bluray", Remux => "remux",
        WebDl => "web-dl", WebRip => "webrip",
    }
}

str_enum! {
    VideoCodec {
        H264 => "h264", H265 => "h265", Av1 => "av1", Vp9 => "vp9", Xvid => "xvid",
        Divx => "divx", Mpeg2 => "mpeg2", Vc1 => "vc1",
    }
}

str_enum! {
    Hdr { Hdr => "hdr", Hdr10 => "hdr10", Hdr10Plus => "hdr10+", DolbyVision => "dv", Hlg => "hlg" }
}

str_enum! {
    AudioCodec {
        Aac => "aac", Ac3 => "ac3", Eac3 => "eac3", Dts => "dts", DtsHd => "dts-hd",
        DtsHdMa => "dts-hd-ma", DtsX => "dts-x", TrueHd => "truehd", Atmos => "atmos",
        Flac => "flac", Mp3 => "mp3", Opus => "opus", Lpcm => "lpcm", Alac => "alac",
        Vorbis => "vorbis",
    }
}

str_enum! {
    Edition {
        Extended => "extended", DirectorsCut => "directors-cut", Unrated => "unrated",
        Uncut => "uncut", Uncensored => "uncensored", Remastered => "remastered",
        Theatrical => "theatrical", Imax => "imax", Criterion => "criterion",
        Special => "special", Collectors => "collectors", Anniversary => "anniversary",
        Ultimate => "ultimate", FinalCut => "final-cut", OpenMatte => "open-matte",
        Deluxe => "deluxe", Goty => "goty", Definitive => "definitive",
    }
}

str_enum! {
    Flag {
        Proper => "proper", Repack => "repack", Real => "real", Internal => "internal",
        Limited => "limited", ThreeD => "3d", Dubbed => "dubbed", Hardsub => "hardsub",
        Retail => "retail", Portable => "portable", Cracked => "cracked", Hybrid => "hybrid",
    }
}

/// A music bitrate: constant (`320kbps`) or a LAME VBR preset (`V0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bitrate {
    Kbps(u16),
    Vbr(u8),
}

/// Everything a name says about its content. Empty fields mean the name
/// didn't say, never "no".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Release {
    /// The title as written, with dots and underscores turned into spaces.
    /// Empty when the name is all metadata.
    pub title: String,
    /// Other titles given after ` / `, ` | ` or ` aka `.
    pub alt_titles: Vec<String>,
    pub year: Option<Span>,
    pub season: Option<Span>,
    pub episode: Option<Span>,
    /// An absolute episode number given beside a season and episode.
    pub absolute: Option<Span>,
    pub date: Option<Date>,
    pub resolution: Option<Resolution>,
    pub source: Option<Source>,
    /// Streaming service code (`amzn`, `nf`, …).
    pub service: Option<&'static str>,
    pub video_codec: Option<VideoCodec>,
    /// Video or audio bit depth (10 for video, 16/24 for music).
    pub bit_depth: Option<u8>,
    pub hdr: Vec<Hdr>,
    pub audio: Vec<AudioCodec>,
    pub channels: Option<Channels>,
    /// Audio languages as ISO 639-1 codes, plus `multi`.
    pub languages: Vec<&'static str>,
    pub subtitles: Vec<&'static str>,
    pub editions: Vec<Edition>,
    pub flags: Vec<Flag>,
    /// A season pack, complete series, collection or discography.
    pub pack: bool,
    pub group: Option<String>,
    /// File format, from the extension or a format word (`EPUB`, `NSP`).
    pub container: Option<&'static str>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub author: Option<String>,
    pub bitrate: Option<Bitrate>,
    pub sample_rate: Option<u32>,
    pub version: Option<String>,
    pub platforms: Vec<&'static str>,
    pub arch: Vec<&'static str>,
}

impl Release {
    /// Every field the name set, as `(key, value)` in a fixed order, with
    /// list values sorted and comma-joined. This is the golden corpus's
    /// format and a stable, flat view for logs and the API.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        fn list<T: Copy>(v: &[T], s: impl Fn(T) -> &'static str) -> Option<String> {
            if v.is_empty() {
                return None;
            }
            let mut names: Vec<&str> = v.iter().map(|&x| s(x)).collect();
            names.sort_unstable();
            names.dedup();
            Some(names.join(", "))
        }
        let mut out = Vec::new();
        let mut put = |k: &'static str, v: Option<String>| {
            if let Some(v) = v {
                out.push((k, v));
            }
        };
        put("title", Some(self.title.clone()).filter(|t| !t.is_empty()));
        put(
            "alt",
            (!self.alt_titles.is_empty()).then(|| {
                let mut a = self.alt_titles.clone();
                a.sort();
                a.join(", ")
            }),
        );
        put("year", self.year.map(Span::render));
        put("season", self.season.map(Span::render));
        put("episode", self.episode.map(Span::render));
        put("absolute", self.absolute.map(Span::render));
        put(
            "date",
            self.date
                .map(|d| format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)),
        );
        put("res", self.resolution.map(|r| r.as_str().to_owned()));
        put("source", self.source.map(|s| s.as_str().to_owned()));
        put("service", self.service.map(str::to_owned));
        put("vcodec", self.video_codec.map(|c| c.as_str().to_owned()));
        put("bits", self.bit_depth.map(|b| b.to_string()));
        put("hdr", list(&self.hdr, Hdr::as_str));
        put("acodec", list(&self.audio, AudioCodec::as_str));
        put(
            "channels",
            self.channels.map(|c| format!("{}.{}", c.0, c.1)),
        );
        put("lang", list(&self.languages, |l| l));
        put("subs", list(&self.subtitles, |l| l));
        put("edition", list(&self.editions, Edition::as_str));
        put("flags", list(&self.flags, Flag::as_str));
        put("pack", self.pack.then(|| "yes".to_owned()));
        put("group", self.group.clone());
        put("container", self.container.map(str::to_owned));
        put("artist", self.artist.clone());
        put("album", self.album.clone());
        put("author", self.author.clone());
        put(
            "bitrate",
            self.bitrate.map(|b| match b {
                Bitrate::Kbps(k) => format!("{k}kbps"),
                Bitrate::Vbr(v) => format!("v{v}"),
            }),
        );
        put("sample", self.sample_rate.map(render_sample_rate));
        put("version", self.version.clone());
        put("platform", list(&self.platforms, |p| p));
        put("arch", list(&self.arch, |a| a));
        out
    }

    /// How much the name told us, for picking the effective title among
    /// the torrent name, its folder and its dominant files: a real title
    /// plus identifying facts (year, episode) outweigh quality tags.
    pub fn informativeness(&self) -> u32 {
        let words = self.title.split_whitespace().count().min(4) as u32;
        let mut score = words * 2;
        score += 4 * u32::from(self.year.is_some());
        score +=
            4 * u32::from(self.season.is_some() || self.episode.is_some() || self.date.is_some());
        score += 2 * u32::from(self.artist.is_some() || self.author.is_some());
        let tech = [
            self.resolution.is_some(),
            self.source.is_some(),
            self.video_codec.is_some(),
            !self.audio.is_empty(),
            self.group.is_some(),
            self.version.is_some(),
        ];
        score + tech.iter().filter(|&&t| t).count() as u32
    }
}

fn render_sample_rate(hz: u32) -> String {
    if hz % 1000 == 0 {
        format!("{}kHz", hz / 1000)
    } else {
        format!("{}.{}kHz", hz / 1000, hz % 1000 / 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_render_canonically() {
        let r = Release {
            title: "Show".into(),
            season: Some(Span { first: 1, last: 3 }),
            audio: vec![AudioCodec::TrueHd, AudioCodec::Atmos],
            channels: Some(Channels(7, 1)),
            sample_rate: Some(44_100),
            bitrate: Some(Bitrate::Vbr(0)),
            pack: true,
            ..Release::default()
        };
        let f = r.fields();
        let get = |k| f.iter().find(|(key, _)| *key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("season"), Some("1-3"));
        assert_eq!(get("acodec"), Some("atmos, truehd"));
        assert_eq!(get("channels"), Some("7.1"));
        assert_eq!(get("sample"), Some("44.1kHz"));
        assert_eq!(get("bitrate"), Some("v0"));
        assert_eq!(get("pack"), Some("yes"));
        assert_eq!(get("year"), None);
    }

    #[test]
    fn enum_names_are_unique() {
        fn unique(names: Vec<&str>) {
            let mut sorted = names.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), names.len(), "{names:?}");
        }
        unique(Source::ALL.iter().map(|s| s.as_str()).collect());
        unique(AudioCodec::ALL.iter().map(|s| s.as_str()).collect());
        unique(Edition::ALL.iter().map(|s| s.as_str()).collect());
        unique(Flag::ALL.iter().map(|s| s.as_str()).collect());
    }
}
