use std::borrow::Cow;
use std::fmt;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

/// Every encoding `encoding_rs` knows, in a fixed order. A position in this
/// table is a [`TextEncoding`] id and is persisted in the store, so the
/// table is **append-only**.
static ENCODINGS: [&Encoding; 40] = [
    encoding_rs::UTF_8,
    encoding_rs::GBK,
    encoding_rs::GB18030,
    encoding_rs::BIG5,
    encoding_rs::SHIFT_JIS,
    encoding_rs::EUC_JP,
    encoding_rs::ISO_2022_JP,
    encoding_rs::EUC_KR,
    encoding_rs::WINDOWS_1250,
    encoding_rs::WINDOWS_1251,
    encoding_rs::WINDOWS_1252,
    encoding_rs::WINDOWS_1253,
    encoding_rs::WINDOWS_1254,
    encoding_rs::WINDOWS_1255,
    encoding_rs::WINDOWS_1256,
    encoding_rs::WINDOWS_1257,
    encoding_rs::WINDOWS_1258,
    encoding_rs::WINDOWS_874,
    encoding_rs::KOI8_R,
    encoding_rs::KOI8_U,
    encoding_rs::IBM866,
    encoding_rs::ISO_8859_2,
    encoding_rs::ISO_8859_3,
    encoding_rs::ISO_8859_4,
    encoding_rs::ISO_8859_5,
    encoding_rs::ISO_8859_6,
    encoding_rs::ISO_8859_7,
    encoding_rs::ISO_8859_8,
    encoding_rs::ISO_8859_8_I,
    encoding_rs::ISO_8859_10,
    encoding_rs::ISO_8859_13,
    encoding_rs::ISO_8859_14,
    encoding_rs::ISO_8859_15,
    encoding_rs::ISO_8859_16,
    encoding_rs::MACINTOSH,
    encoding_rs::X_MAC_CYRILLIC,
    encoding_rs::UTF_16BE,
    encoding_rs::UTF_16LE,
    encoding_rs::X_USER_DEFINED,
    encoding_rs::REPLACEMENT,
];

/// The character encoding raw name/path bytes are in, with a stable
/// one-byte id for storage.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextEncoding(u8);

impl TextEncoding {
    pub const UTF8: TextEncoding = TextEncoding(0);

    pub fn id(self) -> u8 {
        self.0
    }

    pub fn from_id(id: u8) -> Option<TextEncoding> {
        (usize::from(id) < ENCODINGS.len()).then_some(TextEncoding(id))
    }

    pub fn encoding(self) -> &'static Encoding {
        ENCODINGS[usize::from(self.0)]
    }

    pub fn from_encoding(encoding: &'static Encoding) -> TextEncoding {
        let id = ENCODINGS
            .iter()
            .position(|e| *e == encoding)
            .expect("ENCODINGS lists every encoding_rs encoding");
        TextEncoding(id as u8)
    }

    /// Looks up a WHATWG label such as `"gbk"` or `"shift_jis"`.
    pub fn for_label(label: &[u8]) -> Option<TextEncoding> {
        Encoding::for_label(label).map(TextEncoding::from_encoding)
    }

    pub fn name(self) -> &'static str {
        self.encoding().name()
    }

    /// Decodes raw bytes, replacing malformed sequences with U+FFFD.
    pub fn decode(self, raw: &[u8]) -> Cow<'_, str> {
        if self == TextEncoding::UTF8 {
            String::from_utf8_lossy(raw)
        } else {
            self.encoding().decode_without_bom_handling(raw).0
        }
    }

    /// Picks one encoding for a set of raw strings from the same torrent.
    /// Valid UTF-8 wins outright; otherwise an explicit hint (a `.torrent`'s
    /// `encoding` key), otherwise statistical detection over all the text at
    /// once, which is far more reliable than guessing string by string.
    pub(crate) fn detect<'b>(
        chunks: impl Iterator<Item = &'b [u8]> + Clone,
        hint: Option<TextEncoding>,
    ) -> TextEncoding {
        if chunks.clone().all(|c| std::str::from_utf8(c).is_ok()) {
            return TextEncoding::UTF8;
        }
        if let Some(hint) = hint.filter(|h| *h != TextEncoding::UTF8) {
            return hint;
        }
        // Detection quality plateaus long before this; it bounds the work on
        // torrents with huge file lists.
        const MAX_FED: usize = 64 * 1024;
        let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
        let mut fed = 0;
        for chunk in chunks {
            if fed >= MAX_FED {
                break;
            }
            detector.feed(chunk, false);
            detector.feed(b"\n", false);
            fed += chunk.len() + 1;
        }
        detector.feed(b"", true);
        TextEncoding::from_encoding(detector.guess(None, Utf8Detection::Deny))
    }

    /// Encodes `text` into this encoding only if that is lossless: every
    /// character is representable and decoding gives `text` back. Encodings
    /// `encoding_rs` can't write (UTF-16, replacement) never qualify.
    pub(crate) fn encode_exact(self, text: &str) -> Option<Vec<u8>> {
        let encoding = self.encoding();
        let (raw, used, unmappable) = encoding.encode(text);
        (!unmappable && used == encoding && self.decode(&raw) == text).then(|| raw.into_owned())
    }
}

impl fmt::Debug for TextEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TextEncoding({})", self.name())
    }
}

/// Raw bytes plus the encoding they are in.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Text {
    pub raw: Vec<u8>,
    pub encoding: TextEncoding,
}

impl Text {
    pub fn decode(&self) -> Cow<'_, str> {
        self.encoding.decode(&self.raw)
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} ({})", self.decode(), self.encoding.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_complete() {
        assert_eq!(TextEncoding::UTF8.encoding(), encoding_rs::UTF_8);
        assert_eq!(TextEncoding::from_id(1).unwrap().name(), "GBK");
        assert!(TextEncoding::from_id(ENCODINGS.len() as u8).is_none());
        for (i, e) in ENCODINGS.iter().enumerate() {
            assert_eq!(TextEncoding::from_encoding(e).id() as usize, i);
        }
    }

    #[test]
    fn detects_gbk() {
        let (raw, _, _) = encoding_rs::GBK.encode("中文电影合集 第二季 高清版本");
        let enc = TextEncoding::detect([&raw[..]].into_iter(), None);
        assert_eq!(enc.decode(&raw), "中文电影合集 第二季 高清版本");
    }

    #[test]
    fn detects_shift_jis() {
        let (raw, _, _) = encoding_rs::SHIFT_JIS.encode("日本語のファイル名とフォルダ");
        let enc = TextEncoding::detect([&raw[..]].into_iter(), None);
        assert_eq!(enc.decode(&raw), "日本語のファイル名とフォルダ");
    }

    #[test]
    fn encode_exact_rejects_lossy_targets() {
        let gbk = TextEncoding::for_label(b"gbk").unwrap();
        let raw = gbk.encode_exact("第一集.mkv").unwrap();
        assert_eq!(gbk.decode(&raw), "第一集.mkv");
        assert_eq!(gbk.encode_exact("ascii").unwrap(), b"ascii");
        // Hangul syllables outside GBK, and encoders that write UTF-8.
        assert!(gbk.encode_exact("한국어").is_none());
        let utf16 = TextEncoding::from_encoding(encoding_rs::UTF_16LE);
        assert!(utf16.encode_exact("a").is_none());
        let cp1252 = TextEncoding::for_label(b"windows-1252").unwrap();
        assert!(cp1252.encode_exact("中").is_none());
    }

    #[test]
    fn utf8_wins_and_hint_applies() {
        let utf8 = "Ünïcödé".as_bytes();
        assert_eq!(
            TextEncoding::detect([utf8].into_iter(), TextEncoding::for_label(b"gbk")),
            TextEncoding::UTF8
        );
        let latin1 = b"caf\xe9";
        let hinted = TextEncoding::detect(
            [&latin1[..]].into_iter(),
            TextEncoding::for_label(b"windows-1252"),
        );
        assert_eq!(hinted.decode(latin1), "café");
    }
}
