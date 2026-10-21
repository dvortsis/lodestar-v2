//! Marker words: what a lowercase word or phrase means, and how sure we
//! are that it's metadata rather than part of a title.
//!
//! *Strong* markers (`1080p`, `x264`, `BluRay`) end the title wherever
//! they appear. *Weak* ones (`Web`, `French`, `Extended`, `Complete`) are
//! ordinary words too, so they only count after the title, inside
//! brackets, in ALL CAPS, or directly before a strong marker (see
//! `parse`).

use crate::{AudioCodec, Bitrate, Date, Edition, Flag, Hdr, Resolution, Source, Span, VideoCodec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Strength {
    Weak,
    Strong,
}

use Strength::{Strong, Weak};

/// One fact a marker contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hit {
    Res(Resolution),
    /// `UHD`: 2160p unless the name gives a resolution.
    Uhd,
    Source(Source),
    Service(&'static str),
    VCodec(VideoCodec),
    Bits(u8),
    Hdr(Hdr),
    Audio(AudioCodec),
    Channels(u8, u8),
    Lang(&'static str),
    Subs(&'static str),
    Edition(Edition),
    Flag(Flag),
    Pack,
    Container(&'static str),
    Platform(&'static str),
    Arch(&'static str),
    Bitrate(Bitrate),
    Sample(u32),
    /// An audiobook or e-book hint without a file format.
    Book,
    /// Metadata with no field of its own (sizes, regions, CRCs).
    Noise,
    Year(Span),
    Season(Span),
    Episode(Span),
    /// ` - 05` in an anime name: the episode, or the absolute number when
    /// an `SxxEyy` is also given.
    DashEpisode(Span),
    Date(Date),
    /// Byte range of the version text in the name (`25.0.0.37`).
    Version(u32, u32),
}

type Entry = (Strength, &'static [Hit]);

/// Multi-word markers, matched before single words. Longest match wins.
pub(crate) const PHRASES: &[(&[&str], Strength, &[Hit])] = &[
    (&["web", "dl"], Strong, &[Hit::Source(Source::WebDl)]),
    (&["web", "rip"], Strong, &[Hit::Source(Source::WebRip)]),
    (&["blu", "ray"], Strong, &[Hit::Source(Source::BluRay)]),
    (&["bd", "rip"], Strong, &[Hit::Source(Source::BluRay)]),
    (&["br", "rip"], Strong, &[Hit::Source(Source::BluRay)]),
    (&["dvd", "rip"], Strong, &[Hit::Source(Source::Dvd)]),
    (&["dvd", "r"], Strong, &[Hit::Source(Source::Dvd)]),
    (&["hd", "rip"], Strong, &[Hit::Source(Source::HdRip)]),
    (&["hd", "cam"], Strong, &[Hit::Source(Source::Cam)]),
    (&["hd", "ts"], Strong, &[Hit::Source(Source::Telesync)]),
    (&["hd", "tv"], Strong, &[Hit::Source(Source::Tv)]),
    (&["h", "264"], Strong, &[Hit::VCodec(VideoCodec::H264)]),
    (&["x", "264"], Strong, &[Hit::VCodec(VideoCodec::H264)]),
    (&["h", "265"], Strong, &[Hit::VCodec(VideoCodec::H265)]),
    (&["x", "265"], Strong, &[Hit::VCodec(VideoCodec::H265)]),
    (&["mpeg", "2"], Strong, &[Hit::VCodec(VideoCodec::Mpeg2)]),
    (&["vc", "1"], Strong, &[Hit::VCodec(VideoCodec::Vc1)]),
    (
        &["dts", "hd", "ma"],
        Strong,
        &[Hit::Audio(AudioCodec::DtsHdMa)],
    ),
    (&["dts", "hdma"], Strong, &[Hit::Audio(AudioCodec::DtsHdMa)]),
    (&["dts", "ma"], Strong, &[Hit::Audio(AudioCodec::DtsHdMa)]),
    (&["dts", "hd"], Strong, &[Hit::Audio(AudioCodec::DtsHd)]),
    (&["dts", "x"], Strong, &[Hit::Audio(AudioCodec::DtsX)]),
    (&["dts", "es"], Strong, &[Hit::Audio(AudioCodec::Dts)]),
    (&["true", "hd"], Strong, &[Hit::Audio(AudioCodec::TrueHd)]),
    (&["e", "ac", "3"], Strong, &[Hit::Audio(AudioCodec::Eac3)]),
    (&["e", "ac3"], Strong, &[Hit::Audio(AudioCodec::Eac3)]),
    (&["he", "aac"], Strong, &[Hit::Audio(AudioCodec::Aac)]),
    (&["aac", "lc"], Strong, &[Hit::Audio(AudioCodec::Aac)]),
    (
        &["dolby", "atmos"],
        Strong,
        &[Hit::Audio(AudioCodec::Atmos)],
    ),
    (
        &["dolby", "digital"],
        Strong,
        &[Hit::Audio(AudioCodec::Ac3)],
    ),
    (&["dolby", "vision"], Strong, &[Hit::Hdr(Hdr::DolbyVision)]),
    (&["hdr10", "plus"], Strong, &[Hit::Hdr(Hdr::Hdr10Plus)]),
    (&["hdr", "10"], Strong, &[Hit::Hdr(Hdr::Hdr10)]),
    (&["dual", "audio"], Strong, &[Hit::Lang("multi")]),
    (&["multi", "audio"], Strong, &[Hit::Lang("multi")]),
    (&["multi", "sub"], Strong, &[Hit::Subs("multi")]),
    (&["multi", "subs"], Strong, &[Hit::Subs("multi")]),
    (&["multiple", "subtitle"], Strong, &[Hit::Subs("multi")]),
    (&["multiple", "subtitles"], Strong, &[Hit::Subs("multi")]),
    (&["multi", "subtitle"], Strong, &[Hit::Subs("multi")]),
    (
        &["directors", "cut"],
        Strong,
        &[Hit::Edition(Edition::DirectorsCut)],
    ),
    (
        &["director's", "cut"],
        Strong,
        &[Hit::Edition(Edition::DirectorsCut)],
    ),
    (
        &["director", "s", "cut"],
        Strong,
        &[Hit::Edition(Edition::DirectorsCut)],
    ),
    (
        &["extended", "cut"],
        Strong,
        &[Hit::Edition(Edition::Extended)],
    ),
    (
        &["extended", "edition"],
        Strong,
        &[Hit::Edition(Edition::Extended)],
    ),
    (
        &["special", "edition"],
        Strong,
        &[Hit::Edition(Edition::Special)],
    ),
    (
        &["collectors", "edition"],
        Strong,
        &[Hit::Edition(Edition::Collectors)],
    ),
    (
        &["collector's", "edition"],
        Strong,
        &[Hit::Edition(Edition::Collectors)],
    ),
    (
        &["anniversary", "edition"],
        Strong,
        &[Hit::Edition(Edition::Anniversary)],
    ),
    (
        &["ultimate", "edition"],
        Strong,
        &[Hit::Edition(Edition::Ultimate)],
    ),
    (
        &["ultimate", "cut"],
        Strong,
        &[Hit::Edition(Edition::Ultimate)],
    ),
    (
        &["final", "cut"],
        Strong,
        &[Hit::Edition(Edition::FinalCut)],
    ),
    (
        &["open", "matte"],
        Strong,
        &[Hit::Edition(Edition::OpenMatte)],
    ),
    (
        &["theatrical", "cut"],
        Strong,
        &[Hit::Edition(Edition::Theatrical)],
    ),
    (
        &["theatrical", "edition"],
        Strong,
        &[Hit::Edition(Edition::Theatrical)],
    ),
    (
        &["deluxe", "edition"],
        Strong,
        &[Hit::Edition(Edition::Deluxe)],
    ),
    (
        &["definitive", "edition"],
        Strong,
        &[Hit::Edition(Edition::Definitive)],
    ),
    (
        &["criterion", "collection"],
        Strong,
        &[Hit::Edition(Edition::Criterion)],
    ),
    (
        &["restored", "version"],
        Strong,
        &[Hit::Edition(Edition::Remastered)],
    ),
    (&["goty", "edition"], Strong, &[Hit::Edition(Edition::Goty)]),
    (
        &["game", "of", "the", "year", "edition"],
        Strong,
        &[Hit::Edition(Edition::Goty)],
    ),
    (
        &["game", "of", "the", "year"],
        Strong,
        &[Hit::Edition(Edition::Goty)],
    ),
    (&["complete", "series"], Strong, &[Hit::Pack]),
    (&["complete", "collection"], Strong, &[Hit::Pack]),
    (&["complete", "saga"], Strong, &[Hit::Pack]),
    (&["box", "set"], Weak, &[Hit::Pack]),
    (&["half", "sbs"], Strong, &[Hit::Flag(Flag::ThreeD)]),
    (&["half", "ou"], Strong, &[Hit::Flag(Flag::ThreeD)]),
    (&["full", "sbs"], Strong, &[Hit::Flag(Flag::ThreeD)]),
    (&["pre", "activated"], Strong, &[Hit::Flag(Flag::Cracked)]),
    (&["hi", "res"], Weak, &[Hit::Noise]),
];

/// What a single lowercase word means. `caps` says whether it was written
/// in ALL CAPS; a few words only count then (`REAL` but not `Real`).
pub(crate) fn word(low: &str, caps: bool) -> Option<Entry> {
    use AudioCodec as A;
    let e: Entry = match low {
        // Sources.
        "bluray" | "bdrip" | "brrip" | "bd25" | "bd50" | "bdmv" | "bdiso" => {
            (Strong, &[Hit::Source(Source::BluRay)])
        }
        "bd" => (Weak, &[Hit::Source(Source::BluRay)]),
        "remux" | "bdremux" => (Strong, &[Hit::Source(Source::Remux)]),
        "webdl" | "webdlrip" => (Strong, &[Hit::Source(Source::WebDl)]),
        "web" => (Weak, &[Hit::Source(Source::WebDl)]),
        "webrip" | "webcap" | "vodrip" => (Strong, &[Hit::Source(Source::WebRip)]),
        "hdtv" | "pdtv" | "sdtv" | "hdtvrip" | "tvrip" | "dsr" | "dsrip" | "satrip" | "dvbrip" => {
            (Strong, &[Hit::Source(Source::Tv)])
        }
        "ppv" | "ppvrip" => (Weak, &[Hit::Source(Source::Tv)]),
        "dvdrip" | "dvd" | "dvd5" | "dvd9" | "dvdr" | "dvdiso" => {
            (Strong, &[Hit::Source(Source::Dvd)])
        }
        "hdrip" => (Strong, &[Hit::Source(Source::HdRip)]),
        "camrip" | "hdcam" => (Strong, &[Hit::Source(Source::Cam)]),
        "cam" => (Weak, &[Hit::Source(Source::Cam)]),
        "hdts" | "telesync" | "pdvd" => (Strong, &[Hit::Source(Source::Telesync)]),
        "ts" => (Weak, &[Hit::Source(Source::Telesync)]),
        "hdtc" | "telecine" => (Strong, &[Hit::Source(Source::Telecine)]),
        "tc" => (Weak, &[Hit::Source(Source::Telecine)]),
        "screener" | "dvdscr" | "bdscr" | "webscr" => (Strong, &[Hit::Source(Source::Screener)]),
        "scr" => (Weak, &[Hit::Source(Source::Screener)]),
        "workprint" => (Strong, &[Hit::Source(Source::Workprint)]),
        "vhsrip" => (Strong, &[Hit::Source(Source::Vhs)]),
        "vhs" => (Weak, &[Hit::Source(Source::Vhs)]),
        "uhd" => (Strong, &[Hit::Uhd]),

        // Video.
        "x264" | "h264" | "avc" => (Strong, &[Hit::VCodec(VideoCodec::H264)]),
        "x265" | "h265" | "hevc" => (Strong, &[Hit::VCodec(VideoCodec::H265)]),
        "av1" => (Strong, &[Hit::VCodec(VideoCodec::Av1)]),
        "vp9" => (Strong, &[Hit::VCodec(VideoCodec::Vp9)]),
        "xvid" => (Strong, &[Hit::VCodec(VideoCodec::Xvid)]),
        "divx" => (Strong, &[Hit::VCodec(VideoCodec::Divx)]),
        "mpeg2" => (Strong, &[Hit::VCodec(VideoCodec::Mpeg2)]),
        "vc1" => (Strong, &[Hit::VCodec(VideoCodec::Vc1)]),
        "hi10p" | "hi10" => (Strong, &[Hit::Bits(10)]),
        "hdr" => (Strong, &[Hit::Hdr(Hdr::Hdr)]),
        "hdr10" => (Strong, &[Hit::Hdr(Hdr::Hdr10)]),
        "hdr10+" | "hdr10plus" => (Strong, &[Hit::Hdr(Hdr::Hdr10Plus)]),
        "dovi" => (Strong, &[Hit::Hdr(Hdr::DolbyVision)]),
        "dv" => (Weak, &[Hit::Hdr(Hdr::DolbyVision)]),
        "hlg" => (Strong, &[Hit::Hdr(Hdr::Hlg)]),

        // Audio.
        "aac" => (Strong, &[Hit::Audio(A::Aac)]),
        "ac3" => (Strong, &[Hit::Audio(A::Ac3)]),
        "dd" => (Weak, &[Hit::Audio(A::Ac3)]),
        "eac3" | "ddp" | "dd+" => (Strong, &[Hit::Audio(A::Eac3)]),
        "dts" => (Strong, &[Hit::Audio(A::Dts)]),
        "dtshd" => (Strong, &[Hit::Audio(A::DtsHd)]),
        "dtsx" => (Strong, &[Hit::Audio(A::DtsX)]),
        "truehd" => (Strong, &[Hit::Audio(A::TrueHd)]),
        "atmos" => (Strong, &[Hit::Audio(A::Atmos)]),
        "flac" => (Strong, &[Hit::Audio(A::Flac)]),
        "mp3" => (Strong, &[Hit::Audio(A::Mp3)]),
        "opus" => (Weak, &[Hit::Audio(A::Opus)]),
        "lpcm" | "pcm" => (Strong, &[Hit::Audio(A::Lpcm)]),
        "alac" => (Strong, &[Hit::Audio(A::Alac)]),
        "vorbis" | "ogg" => (Strong, &[Hit::Audio(A::Vorbis)]),
        "2ch" => (Strong, &[Hit::Channels(2, 0)]),
        "6ch" => (Strong, &[Hit::Channels(5, 1)]),
        "8ch" => (Strong, &[Hit::Channels(7, 1)]),
        "stereo" => (Weak, &[Hit::Channels(2, 0)]),
        "mono" => (Weak, &[Hit::Channels(1, 0)]),
        "v0" => (Strong, &[Hit::Bitrate(Bitrate::Vbr(0))]),

        // Editions.
        "extended" => (Weak, &[Hit::Edition(Edition::Extended)]),
        "unrated" => (Weak, &[Hit::Edition(Edition::Unrated)]),
        "uncut" => (Weak, &[Hit::Edition(Edition::Uncut)]),
        "uncensored" => (Weak, &[Hit::Edition(Edition::Uncensored)]),
        "remastered" | "remaster" | "restored" => (Weak, &[Hit::Edition(Edition::Remastered)]),
        "theatrical" => (Weak, &[Hit::Edition(Edition::Theatrical)]),
        "imax" => (Weak, &[Hit::Edition(Edition::Imax)]),
        "criterion" => (Weak, &[Hit::Edition(Edition::Criterion)]),
        "goty" => (Strong, &[Hit::Edition(Edition::Goty)]),

        // Release flags.
        "proper" => (Weak, &[Hit::Flag(Flag::Proper)]),
        "repack" | "rerip" => (Weak, &[Hit::Flag(Flag::Repack)]),
        "real" if caps => (Weak, &[Hit::Flag(Flag::Real)]),
        "internal" => (Weak, &[Hit::Flag(Flag::Internal)]),
        "limited" => (Weak, &[Hit::Flag(Flag::Limited)]),
        "3d" => (Weak, &[Hit::Flag(Flag::ThreeD)]),
        "hsbs" | "sbs" | "hou" | "htab" => (Strong, &[Hit::Flag(Flag::ThreeD)]),
        "dubbed" | "dub" => (Weak, &[Hit::Flag(Flag::Dubbed)]),
        "hc" | "hardsub" | "hardsubs" | "hardcoded" => (Weak, &[Hit::Flag(Flag::Hardsub)]),
        "retail" => (Weak, &[Hit::Flag(Flag::Retail)]),
        "portable" => (Weak, &[Hit::Flag(Flag::Portable)]),
        "hybrid" => (Weak, &[Hit::Flag(Flag::Hybrid)]),
        "keygen" | "keymaker" => (Strong, &[Hit::Flag(Flag::Cracked)]),
        "crack" | "cracked" | "patch" | "activator" | "k'ed" => (Weak, &[Hit::Flag(Flag::Cracked)]),
        "readnfo" | "nfofix" | "dirfix" | "syncfix" | "subfix" | "incl" | "update" | "dlc" => {
            (Weak, &[Hit::Noise])
        }

        // Packs.
        "complete" | "collection" | "trilogy" | "duology" | "quadrilogy" | "anthology"
        | "boxset" | "batch" => (Weak, &[Hit::Pack]),
        "discography" | "discografia" => (Strong, &[Hit::Pack]),

        // Languages and subtitles that aren't plain language names.
        "multi" | "multilingual" | "multilang" => (Strong, &[Hit::Lang("multi")]),
        "truefrench" | "vff" | "vfq" | "vfi" | "vf2" | "vof" => (Strong, &[Hit::Lang("fr")]),
        "vostfr" | "subfrench" => (Strong, &[Hit::Subs("fr")]),
        "esub" | "esubs" | "engsub" | "engsubs" => (Strong, &[Hit::Subs("en")]),
        "msub" | "msubs" | "multisub" | "multisubs" => (Strong, &[Hit::Subs("multi")]),
        "nlsub" | "nlsubs" => (Strong, &[Hit::Subs("nl")]),

        // Formats named in the text (the extension is handled separately).
        "mkv" | "mp4" | "avi" => (Weak, container(low)),
        "epub" | "mobi" | "azw3" | "azw" | "pdf" | "djvu" | "fb2" | "cbz" | "cbr" => {
            (Weak, container(low))
        }
        "iso" => (Weak, &[Hit::Container("iso")]),
        "m4b" => (Weak, &[Hit::Container("m4b"), Hit::Book]),
        "nsp" => (Strong, &[Hit::Container("nsp")]),
        "xci" => (Strong, &[Hit::Container("xci")]),
        "audiobook" | "unabridged" | "abridged" => (Weak, &[Hit::Book]),

        // Platforms and architectures.
        "windows" | "win" => (Weak, &[Hit::Platform("windows")]),
        "win64" | "win32" | "win10" | "win11" | "winall" => (Strong, &[Hit::Platform("windows")]),
        "macos" | "macosx" | "osx" => (Strong, &[Hit::Platform("macos")]),
        "mac" => (Weak, &[Hit::Platform("macos")]),
        "linux" => (Weak, &[Hit::Platform("linux")]),
        "android" => (Weak, &[Hit::Platform("android")]),
        "ios" => (Weak, &[Hit::Platform("ios")]),
        "ps5" => (Strong, &[Hit::Platform("ps5")]),
        "ps4" => (Strong, &[Hit::Platform("ps4")]),
        "ps3" => (Strong, &[Hit::Platform("ps3")]),
        "ps2" => (Strong, &[Hit::Platform("ps2")]),
        "psx" | "ps1" => (Strong, &[Hit::Platform("psx")]),
        "psp" => (Strong, &[Hit::Platform("psp")]),
        "psvita" | "vita" => (Strong, &[Hit::Platform("vita")]),
        "nsw" => (Strong, &[Hit::Platform("switch")]),
        "switch" => (Weak, &[Hit::Platform("switch")]),
        "xbox360" | "x360" => (Strong, &[Hit::Platform("xbox360")]),
        "xboxone" | "xone" => (Strong, &[Hit::Platform("xboxone")]),
        "xbox" => (Weak, &[Hit::Platform("xbox")]),
        "wii" => (Strong, &[Hit::Platform("wii")]),
        "wiiu" => (Strong, &[Hit::Platform("wiiu")]),
        "3ds" => (Strong, &[Hit::Platform("3ds")]),
        "nds" => (Strong, &[Hit::Platform("nds")]),
        "gba" => (Strong, &[Hit::Platform("gba")]),
        "n64" => (Strong, &[Hit::Platform("n64")]),
        "snes" => (Strong, &[Hit::Platform("snes")]),
        "nes" => (Weak, &[Hit::Platform("nes")]),
        "gamecube" | "ngc" => (Strong, &[Hit::Platform("gamecube")]),
        "dreamcast" => (Weak, &[Hit::Platform("dreamcast")]),
        "pc" => (Weak, &[Hit::Platform("pc")]),
        "x64" | "amd64" | "64bit" => (Strong, &[Hit::Arch("x64")]),
        "x86" | "i386" | "i686" | "32bit" => (Strong, &[Hit::Arch("x86")]),
        "arm64" | "aarch64" => (Strong, &[Hit::Arch("arm64")]),

        // Streaming services. Short codes, so only after the title.
        "amzn" => (Weak, &[Hit::Service("amzn")]),
        "nf" => (Weak, &[Hit::Service("nf")]),
        "dsnp" | "dsny" => (Weak, &[Hit::Service("dsnp")]),
        "hmax" => (Weak, &[Hit::Service("hmax")]),
        "max" => (Weak, &[Hit::Service("max")]),
        "atvp" => (Weak, &[Hit::Service("atvp")]),
        "hulu" => (Weak, &[Hit::Service("hulu")]),
        "pcok" => (Weak, &[Hit::Service("pcok")]),
        "pmtp" => (Weak, &[Hit::Service("pmtp")]),
        "cr" => (Weak, &[Hit::Service("cr")]),
        "ip" => (Weak, &[Hit::Service("ip")]),
        "stan" => (Weak, &[Hit::Service("stan")]),
        "crav" => (Weak, &[Hit::Service("crav")]),
        "funi" => (Weak, &[Hit::Service("funi")]),
        "hidi" => (Weak, &[Hit::Service("hidi")]),
        "mubi" => (Weak, &[Hit::Service("mubi")]),

        // Regions: metadata, but nothing we index.
        "usa" | "eur" | "pal" | "ntsc" => (Weak, &[Hit::Noise]),
        _ => return None,
    };
    Some(e)
}

fn container(low: &str) -> &'static [Hit] {
    match low {
        "mkv" => &[Hit::Container("mkv")],
        "mp4" => &[Hit::Container("mp4")],
        "avi" => &[Hit::Container("avi")],
        "epub" => &[Hit::Container("epub")],
        "mobi" => &[Hit::Container("mobi")],
        "azw3" => &[Hit::Container("azw3")],
        "azw" => &[Hit::Container("azw")],
        "pdf" => &[Hit::Container("pdf")],
        "djvu" => &[Hit::Container("djvu")],
        "fb2" => &[Hit::Container("fb2")],
        "cbz" => &[Hit::Container("cbz")],
        "cbr" => &[Hit::Container("cbr")],
        _ => &[Hit::Noise],
    }
}

/// Spoken-language words and the ISO 639-1 code they stand for. Two-letter
/// codes are left out: `IT`, `ES` and `DE` are far more often words.
pub(crate) fn lang(low: &str) -> Option<&'static str> {
    Some(match low {
        "english" | "eng" => "en",
        "french" | "fre" | "fra" | "francais" | "français" => "fr",
        "german" | "ger" | "deu" | "deutsch" => "de",
        "italian" | "ita" | "italiano" => "it",
        "spanish" | "spa" | "esp" | "castellano" | "espanol" | "español" | "latino" => "es",
        "portuguese" | "por" | "portugues" | "brazilian" | "dublado" => "pt",
        "russian" | "rus" => "ru",
        "ukrainian" | "ukr" => "uk",
        "polish" | "pldub" => "pl",
        "dutch" | "nld" | "flemish" => "nl",
        "swedish" | "swe" => "sv",
        "norwegian" => "no",
        "danish" => "da",
        "finnish" => "fi",
        "czech" | "cze" => "cs",
        "hungarian" | "hun" => "hu",
        "greek" | "gre" => "el",
        "turkish" | "tur" => "tr",
        "arabic" | "ara" => "ar",
        "hebrew" | "heb" => "he",
        "hindi" | "hin" => "hi",
        "tamil" | "tam" => "ta",
        "telugu" | "tel" => "te",
        "malayalam" => "ml",
        "kannada" => "kn",
        "bengali" => "bn",
        "japanese" | "jpn" | "jap" => "ja",
        "korean" | "kor" => "ko",
        "chinese" | "chi" | "chs" | "cht" | "zho" | "mandarin" | "cantonese" => "zh",
        "thai" | "tha" => "th",
        "vietnamese" | "vie" => "vi",
        "indonesian" => "id",
        "persian" | "farsi" => "fa",
        "romanian" | "rum" | "ron" => "ro",
        "bulgarian" | "bul" => "bg",
        "croatian" => "hr",
        "serbian" => "sr",
        _ => return None,
    })
}

/// Audio codecs that are written glued to a channel count: `DDP5.1`,
/// `AAC2.0`, `DD+5.1`, `FLAC2.0`.
pub(crate) fn channel_prefix(low: &str) -> Option<AudioCodec> {
    Some(match low {
        "dd" | "ac3" => AudioCodec::Ac3,
        "ddp" | "dd+" | "eac3" => AudioCodec::Eac3,
        "aac" => AudioCodec::Aac,
        "flac" => AudioCodec::Flac,
        "opus" => AudioCodec::Opus,
        "lpcm" | "pcm" => AudioCodec::Lpcm,
        "truehd" => AudioCodec::TrueHd,
        "dts" => AudioCodec::Dts,
        _ => return None,
    })
}

/// Top-level domains that mark a `www.site.tld` prefix.
pub(crate) fn is_tld(low: &str) -> bool {
    matches!(
        low,
        "com"
            | "net"
            | "org"
            | "info"
            | "to"
            | "me"
            | "cc"
            | "ws"
            | "ru"
            | "in"
            | "se"
            | "nz"
            | "io"
            | "tv"
            | "re"
            | "ag"
            | "gs"
            | "li"
            | "lol"
            | "xyz"
            | "cd"
            | "biz"
            | "co"
            | "pw"
            | "eu"
            | "uk"
            | "is"
            | "su"
            | "club"
            | "site"
            | "top"
    )
}

/// File extensions the parser strips and reports as the container.
pub(crate) fn extension(low: &str) -> Option<&'static str> {
    const KNOWN: &[&str] = &[
        "mkv", "mp4", "m4v", "avi", "wmv", "mov", "mpg", "mpeg", "webm", "flv", "m2ts", "vob",
        "ogm", "rmvb", "3gp", "mp3", "flac", "m4a", "m4b", "aac", "ogg", "opus", "wav", "ape",
        "wv", "dsf", "dff", "wma", "epub", "mobi", "azw3", "azw", "pdf", "djvu", "fb2", "cbz",
        "cbr", "cb7", "lit", "iso", "img", "zip", "rar", "7z", "exe", "msi", "dmg", "apk", "pkg",
        "nsp", "xci", "deb", "rpm", "srt", "ass", "ssa", "vtt", "nfo", "txt",
    ];
    KNOWN.iter().find(|&&k| k == low).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_are_lowercase_and_unique() {
        for (i, (words, _, _)) in PHRASES.iter().enumerate() {
            assert!(words.len() >= 2, "{words:?} is a single word");
            for w in words.iter() {
                assert_eq!(*w, w.to_lowercase());
            }
            assert!(
                PHRASES[i + 1..].iter().all(|(other, _, _)| other != words),
                "{words:?} listed twice"
            );
        }
    }

    #[test]
    fn caps_only_words() {
        assert!(word("real", false).is_none());
        assert!(word("real", true).is_some());
        assert_eq!(
            word("french", false),
            None,
            "languages are looked up by `lang`"
        );
        assert_eq!(lang("french"), Some("fr"));
    }
}
