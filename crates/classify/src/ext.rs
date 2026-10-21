//! The extension registry: which of the ten categories (§8) a file is in.

/// File categories, in index order for [`crate::Composition`]. The UI
/// colours them as six groups (§10.18); facets and filters keep all ten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Video,
    Audio,
    Image,
    Document,
    Ebook,
    Archive,
    Disc,
    Software,
    Subtitle,
    Other,
}

impl Category {
    pub const ALL: [Category; 10] = [
        Category::Video,
        Category::Audio,
        Category::Image,
        Category::Document,
        Category::Ebook,
        Category::Archive,
        Category::Disc,
        Category::Software,
        Category::Subtitle,
        Category::Other,
    ];

    /// The name used in index fields (`cat_bytes_video`) and queries.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Video => "video",
            Category::Audio => "audio",
            Category::Image => "image",
            Category::Document => "document",
            Category::Ebook => "ebook",
            Category::Archive => "archive",
            Category::Disc => "disc",
            Category::Software => "software",
            Category::Subtitle => "subtitle",
            Category::Other => "other",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// The lowercase extension of a file name, at most 8 bytes, in a stack
/// buffer so classifying 200k files doesn't allocate per file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ext {
    buf: [u8; 8],
    len: u8,
}

impl Ext {
    pub fn as_str(&self) -> &str {
        // Only ASCII bytes are ever stored.
        std::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or("")
    }
}

/// The extension of `file_name`, if it has a plausible one: ASCII
/// alphanumeric, 1–8 bytes, after a dot that isn't the first character
/// (`.nfo` alone is a hidden file, not an extension).
pub fn extension(file_name: &str) -> Option<Ext> {
    let dot = file_name.rfind('.')?;
    let raw = &file_name.as_bytes()[dot + 1..];
    if dot == 0 || raw.is_empty() || raw.len() > 8 || !raw.iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    let mut buf = [0u8; 8];
    for (b, r) in buf.iter_mut().zip(raw) {
        *b = r.to_ascii_lowercase();
    }
    Some(Ext {
        buf,
        len: raw.len() as u8,
    })
}

/// The category of a file name. Split-archive parts (`.r00`, `.001`,
/// `.z01`, `.part2.rar`) are archives.
pub fn category(file_name: &str) -> Category {
    match extension(file_name) {
        Some(ext) => category_of(ext.as_str()),
        None => Category::Other,
    }
}

pub fn category_of(ext: &str) -> Category {
    use Category::*;
    match ext {
        "mkv" | "mp4" | "m4v" | "avi" | "wmv" | "mov" | "mpg" | "mpeg" | "mpe" | "webm" | "flv"
        | "f4v" | "m2ts" | "mts" | "ts" | "vob" | "ogv" | "ogm" | "rm" | "rmvb" | "3gp" | "3g2"
        | "divx" | "asf" | "qt" | "m2v" | "mxf" | "y4m" | "evo" => Video,
        "mp3" | "flac" | "m4a" | "m4b" | "aac" | "ogg" | "oga" | "opus" | "wav" | "ape" | "wv"
        | "dsf" | "dff" | "wma" | "alac" | "aif" | "aiff" | "aifc" | "mka" | "ac3" | "dts"
        | "eac3" | "mpc" | "tta" | "tak" | "mid" | "midi" | "amr" | "ra" | "au" | "shn" => Audio,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "heic" | "heif"
        | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "orf" | "rw2" | "psd" | "svg" | "ico"
        | "jxl" | "avif" | "tga" => Image,
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "nfo" | "diz" | "xls" | "xlsx"
        | "ods" | "ppt" | "pptx" | "odp" | "csv" | "html" | "htm" | "xml" | "json" | "log"
        | "tex" | "ps" | "chm" => Document,
        "epub" | "mobi" | "azw" | "azw3" | "kf8" | "kfx" | "fb2" | "djvu" | "lit" | "pdb"
        | "lrf" | "ibooks" | "cbz" | "cbr" | "cb7" | "cbt" | "cba" => Ebook,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "tbz" | "xz" | "txz" | "zst"
        | "lz" | "lzma" | "cab" | "arj" | "lzh" | "ace" | "sit" => Archive,
        "iso" | "img" | "bin" | "cue" | "mdf" | "mds" | "nrg" | "cdi" | "ccd" | "toast" | "gdi"
        | "chd" | "cso" | "wbfs" | "nsp" | "xci" | "vpk" | "wad" => Disc,
        "exe" | "msi" | "msix" | "appx" | "apk" | "xapk" | "ipa" | "dmg" | "pkg" | "deb"
        | "rpm" | "appimage" | "snap" | "jar" | "bat" | "cmd" | "sh" | "ps1" | "com" | "scr"
        | "pif" | "vbs" | "js" | "hta" | "dll" | "sys" | "so" | "dylib" | "run" | "lnk" | "url"
        | "vst3" => Software,
        "srt" | "ass" | "ssa" | "sub" | "idx" | "vtt" | "sup" | "smi" | "sbv" | "ttml" | "lrc" => {
            Subtitle
        }
        _ if is_split_part(ext) => Archive,
        _ => Other,
    }
}

/// `r00`–`r99`, `z01`, `001`: parts of a split archive.
pub fn is_split_part(ext: &str) -> bool {
    let b = ext.as_bytes();
    match b {
        [b'r' | b'z', d1, d2] => d1.is_ascii_digit() && d2.is_ascii_digit(),
        [d1, d2, d3] => d1.is_ascii_digit() && d2.is_ascii_digit() && d3.is_ascii_digit(),
        _ => false,
    }
}

/// File types that run code or open a URL when double-clicked. In a
/// media torrent they are the classic malware lure (§6 junk score).
pub fn is_dangerous(ext: &str) -> bool {
    matches!(
        ext,
        "exe"
            | "msi"
            | "scr"
            | "pif"
            | "com"
            | "bat"
            | "cmd"
            | "vbs"
            | "js"
            | "hta"
            | "lnk"
            | "url"
            | "ps1"
            | "jar"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        for (name, cat) in [
            ("Movie.2019.1080p.mkv", Category::Video),
            ("01 - Track.FLAC", Category::Audio),
            ("cover.JPG", Category::Image),
            ("Movie.nfo", Category::Document),
            ("Book.epub", Category::Ebook),
            ("release.part02.rar", Category::Archive),
            ("release.r07", Category::Archive),
            ("release.7z.001", Category::Archive),
            ("game.iso", Category::Disc),
            ("setup.exe", Category::Software),
            ("Movie.en.srt", Category::Subtitle),
            ("checksums.sfv", Category::Other),
            ("README", Category::Other),
            (".hidden", Category::Other),
            ("weird.ext-ension", Category::Other),
        ] {
            assert_eq!(category(name), cat, "{name}");
        }
    }

    #[test]
    fn every_category_has_a_unique_name() {
        let mut names: Vec<&str> = Category::ALL.iter().map(|c| c.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 10);
        for (i, c) in Category::ALL.iter().enumerate() {
            assert_eq!(c.index(), i);
        }
    }

    #[test]
    fn extension_is_bounded_ascii() {
        assert_eq!(extension("a.MKV").unwrap().as_str(), "mkv");
        assert!(extension("a.ébc").is_none());
        assert!(extension("a.").is_none());
        assert!(extension("a.toolongext").is_none());
        assert!(extension("noext").is_none());
    }
}
