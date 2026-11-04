//! A bounded JSON reader for the JSON sources' parsers (architecture
//! §18.9): [`archive_org`](crate::archive_org) and the enrichment parsers
//! that follow it.
//!
//! It's written here rather than taken from `serde_json`, so every cap
//! holds while reading, not after a whole tree is built. A parser walks
//! the document itself: it opens the objects and arrays it knows with
//! [`Walk::object`] and [`Walk::array`], reads the values it keeps with
//! [`Reader::text`] and [`Reader::number`], and passes over the rest with
//! [`Reader::skip`], which is iterative and bounded by
//! [`Limits::max_depth`]. A string is copied only when the parser asks for
//! it, and its raw length is checked against [`Limits::max_text`] first;
//! decoding only shrinks text.
//!
//! It follows RFC 8259: strict number grammar, no raw control characters
//! in strings, only the defined escapes, and no trailing commas, single
//! quotes or text after the root ([`Reader::end`]). A lone UTF-16
//! surrogate escape is only scanned, unless the string is decoded
//! ([`TextError::Malformed`]). Keys are compared as written, still
//! escaped. The input is already UTF-8 (`&str`), so it never splits a
//! character.
//!
//! A structural fault ends the read with a [`Stop`]: `Malformed` for bad
//! JSON or input cut short, `Depth` past [`Limits::max_depth`] open
//! containers, `Elements` past [`Limits::max_values`] values. The parser
//! keeps what it finished before it. Nothing here panics or allocates more
//! than the string being decoded and one stack entry per open container.

pub use crate::common::Stop;

/// The reader's caps. The parser using it picks the values and documents
/// why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values read, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Elements in one array that [`Reader::skip`] passes over before it
    /// reports the array as over-long. Not a stop: the parser decides.
    pub max_array: usize,
    /// Raw bytes of a string [`Reader::text`] decodes.
    pub max_text: usize,
}

/// A structural fault ends the read.
pub type R<T> = Result<T, Stop>;

/// Why [`Reader::text`] read a string but didn't return it. The read goes
/// on; the parser decides what the fault spoils.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextError {
    /// Longer than [`Limits::max_text`], raw.
    OverCap,
    /// A lone UTF-16 surrogate escape.
    Malformed,
}

/// A cursor over one JSON document.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    s: &'a str,
    i: usize,
    l: Limits,
    depth: usize,
    values: usize,
}

impl<'a> Reader<'a> {
    pub fn new(s: &'a str, limits: Limits) -> Self {
        Reader {
            s,
            i: 0,
            l: limits,
            depth: 0,
            values: 0,
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.l
    }

    /// The next byte after whitespace, without taking it.
    pub fn peek(&mut self) -> Option<u8> {
        let b = self.s.as_bytes();
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = b.get(self.i) {
            self.i += 1;
        }
        b.get(self.i).copied()
    }

    /// Takes `c`, which must be next.
    pub fn eat(&mut self, c: u8) -> R<()> {
        if self.peek() != Some(c) {
            return Err(Stop::Malformed);
        }
        self.i += 1;
        Ok(())
    }

    /// Counts one value against [`Limits::max_values`]. [`Walk::object`],
    /// [`Walk::array`], [`Self::skip`] and [`Self::text`] count their
    /// own; a parser that reads a [`Self::number`] or [`Self::string`]
    /// as a value counts it here.
    pub fn value(&mut self) -> R<()> {
        self.values += 1;
        if self.values > self.l.max_values {
            return Err(Stop::Elements);
        }
        Ok(())
    }

    /// Opens a container whose `c` is next.
    fn open(&mut self, c: u8) -> R<()> {
        self.eat(c)?;
        self.depth += 1;
        if self.depth > self.l.max_depth {
            return Err(Stop::Depth);
        }
        Ok(())
    }

    /// Closes the container if `c` is next.
    fn close(&mut self, c: u8) -> bool {
        if self.peek() != Some(c) {
            return false;
        }
        self.i += 1;
        self.depth -= 1;
        true
    }

    /// After an element: `,` (true), or the closing `c` (false).
    fn next(&mut self, c: u8) -> R<bool> {
        if self.peek() == Some(b',') {
            self.i += 1;
            return Ok(true);
        }
        if self.close(c) {
            return Ok(false);
        }
        Err(Stop::Malformed)
    }

    /// A string that is next, still escaped, without its quotes. Escapes
    /// are checked, not decoded ([`decode`]); the text is valid UTF-8
    /// already. It isn't counted as a value.
    pub fn string(&mut self) -> R<&'a str> {
        self.eat(b'"')?;
        let (s, start) = (self.s, self.i);
        let b = s.as_bytes();
        loop {
            match b.get(self.i) {
                Some(b'"') => {
                    let raw = s.get(start..self.i).ok_or(Stop::Malformed)?;
                    self.i += 1;
                    return Ok(raw);
                }
                Some(b'\\') => match b.get(self.i + 1) {
                    Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.i += 2,
                    Some(b'u')
                        if b.get(self.i + 2..self.i + 6)
                            .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) =>
                    {
                        self.i += 6
                    }
                    _ => return Err(Stop::Malformed),
                },
                Some(&c) if c >= 0x20 => self.i += 1,
                _ => return Err(Stop::Malformed),
            }
        }
    }

    /// A number that is next, as written (RFC 8259 grammar). It isn't
    /// counted as a value.
    pub fn number(&mut self) -> R<&'a str> {
        let (s, start) = (self.s, self.i);
        let b = s.as_bytes();
        let digits = |i: &mut usize| {
            let from = *i;
            while b.get(*i).is_some_and(u8::is_ascii_digit) {
                *i += 1;
            }
            *i > from
        };
        let mut i = start;
        if b.get(i) == Some(&b'-') {
            i += 1;
        }
        match b.get(i) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => {
                digits(&mut i);
            }
            _ => return Err(Stop::Malformed),
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            if !digits(&mut i) {
                return Err(Stop::Malformed);
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(b.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            if !digits(&mut i) {
                return Err(Stop::Malformed);
            }
        }
        self.i = i;
        s.get(start..i).ok_or(Stop::Malformed)
    }

    fn literal(&mut self, word: &str) -> R<()> {
        if !self.s.get(self.i..).is_some_and(|r| r.starts_with(word)) {
            return Err(Stop::Malformed);
        }
        self.i += word.len();
        Ok(())
    }

    /// Reads past the next value without keeping it. True when an array
    /// in it is longer than [`Limits::max_array`]. Iterative: its stack
    /// is bounded by [`Limits::max_depth`].
    pub fn skip(&mut self) -> R<bool> {
        let mut over = false;
        // One entry per open container: whether it's an array, and its
        // elements so far.
        let mut stack: Vec<(bool, usize)> = Vec::new();
        loop {
            self.value()?;
            let opened = match self.peek() {
                Some(b'{') => {
                    self.open(b'{')?;
                    !self.close(b'}') && {
                        stack.push((false, 1));
                        true
                    }
                }
                Some(b'[') => {
                    self.open(b'[')?;
                    !self.close(b']') && {
                        stack.push((true, 1));
                        over |= self.l.max_array == 0;
                        true
                    }
                }
                Some(b'"') => self.string().map(|_| false)?,
                Some(b't') => self.literal("true").map(|_| false)?,
                Some(b'f') => self.literal("false").map(|_| false)?,
                Some(b'n') => self.literal("null").map(|_| false)?,
                Some(b'-' | b'0'..=b'9') => self.number().map(|_| false)?,
                _ => return Err(Stop::Malformed),
            };
            if opened {
                if stack.last().is_some_and(|t| !t.0) {
                    self.string()?;
                    self.eat(b':')?;
                }
                continue;
            }
            // A value ended: close what ends with it, then go on to the
            // next element of the innermost open container.
            loop {
                let Some(&(is_array, n)) = stack.last() else {
                    return Ok(over);
                };
                let close = if is_array { b']' } else { b'}' };
                if !self.next(close)? {
                    stack.pop();
                    continue;
                }
                if is_array {
                    over |= n >= self.l.max_array;
                } else {
                    self.string()?;
                    self.eat(b':')?;
                }
                if let Some(t) = stack.last_mut() {
                    t.1 = n + 1;
                }
                break;
            }
        }
    }

    /// A string value that is next, counted and decoded. One over
    /// [`Limits::max_text`] raw isn't decoded; one with a lone surrogate
    /// doesn't decode. Either way the string is read past.
    pub fn text(&mut self) -> R<Result<String, TextError>> {
        self.value()?;
        let raw = self.string()?;
        if raw.len() > self.l.max_text {
            return Ok(Err(TextError::OverCap));
        }
        Ok(decode(raw).ok_or(TextError::Malformed))
    }

    /// Nothing but whitespace is left: the end of the document.
    pub fn end(&mut self) -> R<()> {
        match self.peek() {
            Some(_) => Err(Stop::Malformed),
            None => Ok(()),
        }
    }
}

/// Walking objects and arrays, for [`Reader`] and for a parser that holds
/// one, so each element's callback gets the parser's own state too.
pub trait Walk<'a>: Sized {
    fn json(&mut self) -> &mut Reader<'a>;

    /// An object that is next, handing each key (still escaped) to `f`,
    /// which must read the value.
    fn object(&mut self, mut f: impl FnMut(&mut Self, &'a str) -> R<()>) -> R<()> {
        let j = self.json();
        j.value()?;
        j.open(b'{')?;
        if j.close(b'}') {
            return Ok(());
        }
        loop {
            let j = self.json();
            let key = j.string()?;
            j.eat(b':')?;
            f(self, key)?;
            if !self.json().next(b'}')? {
                return Ok(());
            }
        }
    }

    /// An array that is next, handing each element's index to `f`, which
    /// must read the element.
    fn array(&mut self, mut f: impl FnMut(&mut Self, usize) -> R<()>) -> R<()> {
        let j = self.json();
        j.value()?;
        j.open(b'[')?;
        if j.close(b']') {
            return Ok(());
        }
        for n in 0.. {
            f(self, n)?;
            if !self.json().next(b']')? {
                break;
            }
        }
        Ok(())
    }
}

impl<'a> Walk<'a> for Reader<'a> {
    fn json(&mut self) -> &mut Reader<'a> {
        self
    }
}

/// A JSON string's text without its escapes; `None` for a lone surrogate
/// or an escape [`Reader::string`] would refuse. Never longer than `raw`.
pub fn decode(raw: &str) -> Option<String> {
    if !raw.contains('\\') {
        return Some(raw.to_owned());
    }
    let b = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let (mut i, mut run) = (0, 0);
    while let Some(&c) = b.get(i) {
        if c != b'\\' {
            i += 1;
            continue;
        }
        out.push_str(raw.get(run..i)?);
        let ch = match b.get(i + 1)? {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let hi = hex4(raw.get(i + 2..i + 6)?)?;
                i += 6;
                let cp = match hi {
                    0xD800..=0xDBFF => {
                        if raw.get(i..i + 2)? != "\\u" {
                            return None;
                        }
                        let lo = hex4(raw.get(i + 2..i + 6)?)?;
                        if !(0xDC00..=0xDFFF).contains(&lo) {
                            return None;
                        }
                        i += 6;
                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                    }
                    0xDC00..=0xDFFF => return None,
                    _ => hi,
                };
                out.push(char::from_u32(cp)?);
                run = i;
                continue;
            }
            _ => return None,
        };
        out.push(ch);
        i += 2;
        run = i;
    }
    out.push_str(raw.get(run..)?);
    Some(out)
}

pub(crate) fn hex4(s: &str) -> Option<u32> {
    (s.len() == 4 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(s, 16).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: Limits = Limits {
        max_depth: 16,
        max_values: 1000,
        max_array: 100,
        max_text: 64,
    };

    fn skip_all(s: &str, l: Limits) -> R<bool> {
        let mut r = Reader::new(s, l);
        let over = r.skip()?;
        r.end()?;
        Ok(over)
    }

    #[test]
    fn decodes_escapes() {
        let u = |s: &str| s.replace('U', "\\u");
        assert_eq!(
            decode(&u(r"aU00e9\n\/\\U0022")).as_deref(),
            Some("a\u{e9}\n/\\\"")
        );
        assert_eq!(decode(&u("Ud83dUde00")).as_deref(), Some("\u{1F600}"));
        for bad in ["Ud83d", "Ude00", "Ud83dx", "Ud83dU0041", r"\x"] {
            assert_eq!(decode(&u(bad)), None, "{bad}");
        }
        let raw = u("U0041Ud83dUde00");
        assert!(decode(&raw).unwrap().len() <= raw.len());
        assert_eq!(hex4("+fff"), None);
        assert_eq!(hex4("00e9"), Some(0xe9));
    }

    #[test]
    fn skips_well_formed_values() {
        for ok in [
            "null",
            " true ",
            "false",
            "0",
            "-0.5e+10",
            "1E3",
            r#""a\"b\u00e9""#,
            "[]",
            "{}",
            r#"{"a":[1,{"b":null}],"c":"d"}"#,
            "[[[]],[{}]]",
        ] {
            assert_eq!(skip_all(ok, WIDE), Ok(false), "{ok}");
        }
    }

    #[test]
    fn refuses_malformed_json() {
        for bad in [
            "",
            "[1,]",
            "{\"a\":1,}",
            "{'a':1}",
            "[1 2]",
            "{\"a\" 1}",
            "{1:2}",
            "01",
            "1.",
            ".5",
            "-",
            "1e",
            "+1",
            "tru",
            "nul",
            "\"a",
            "\"\\x\"",
            "\"\\u12g4\"",
            "\"a\u{1}b\"",
            "[",
            "{\"a\":",
            "1 2",
            "[] x",
            "NaN",
        ] {
            assert_eq!(skip_all(bad, WIDE), Err(Stop::Malformed), "{bad:?}");
        }
    }

    #[test]
    fn depth_cap() {
        let l = Limits {
            max_depth: 3,
            ..WIDE
        };
        assert_eq!(skip_all("[[[1]]]", l), Ok(false));
        assert_eq!(skip_all("[[[[1]]]]", l), Err(Stop::Depth));
        assert_eq!(skip_all(r#"{"a":{"b":{"c":{}}}}"#, l), Err(Stop::Depth));
        // Depth is what's open at once, not how many opened.
        assert_eq!(skip_all("[[[]],[[]],[[]]]", l), Ok(false));
        // Walked containers count against the same cap.
        let mut r = Reader::new("[[[[]]]]", l);
        let got = r.array(|r, _| r.array(|r, _| r.skip().map(drop)));
        assert_eq!(got, Err(Stop::Depth));
        // Very deep input stops at the cap without recursion.
        let deep = "[".repeat(100_000);
        assert_eq!(skip_all(&deep, WIDE), Err(Stop::Depth));
    }

    #[test]
    fn value_cap() {
        let l = Limits {
            max_values: 4,
            ..WIDE
        };
        // The array and its three elements.
        assert_eq!(skip_all("[1,2,3]", l), Ok(false));
        assert_eq!(skip_all("[1,2,3,4]", l), Err(Stop::Elements));
        // Keys aren't values.
        assert_eq!(skip_all(r#"{"a":1,"b":2,"c":3}"#, l), Ok(false));
        let mut r = Reader::new(r#"["a","b","c","d"]"#, l);
        let got = r.array(|r, _| r.text().map(drop));
        assert_eq!(got, Err(Stop::Elements));
    }

    #[test]
    fn array_cap_is_reported_not_stopped() {
        let l = Limits {
            max_array: 2,
            ..WIDE
        };
        assert_eq!(skip_all("[1,2]", l), Ok(false));
        assert_eq!(skip_all("[1,2,3]", l), Ok(true));
        assert_eq!(skip_all(r#"{"a":[[1,2,3]]}"#, l), Ok(true));
        let zero = Limits {
            max_array: 0,
            ..WIDE
        };
        assert_eq!(skip_all("[]", zero), Ok(false));
        assert_eq!(skip_all("[1]", zero), Ok(true));
    }

    #[test]
    fn text_cap_and_decoding() {
        let l = Limits {
            max_text: 6,
            ..WIDE
        };
        let s = r#"["abcdef","abcdefg","\u00e9","\u00e9\u00e9","\ud83d",1]"#;
        let mut r = Reader::new(s, l);
        let mut got = Vec::new();
        r.array(|r, n| {
            if n < 5 {
                got.push(r.text()?);
                Ok(())
            } else {
                r.skip().map(drop)
            }
        })
        .unwrap();
        r.end().unwrap();
        assert_eq!(
            got,
            [
                Ok("abcdef".to_owned()),
                Err(TextError::OverCap),
                Ok("\u{e9}".to_owned()),
                // Twelve raw bytes, two decoded: the cap is on the raw text.
                Err(TextError::OverCap),
                Err(TextError::Malformed),
            ]
        );
        // A text read is a string or the read stops.
        assert_eq!(Reader::new("1", l).text(), Err(Stop::Malformed));
    }

    #[test]
    fn walks_objects_with_raw_keys() {
        let mut r = Reader::new(r#" { "a" : 1 , "b\u0063" : [ ] , "d": {} } "#, WIDE);
        let mut keys = Vec::new();
        r.object(|r, k| {
            keys.push(k);
            r.skip().map(drop)
        })
        .unwrap();
        r.end().unwrap();
        assert_eq!(keys, ["a", r"b\u0063", "d"]);
        let mut n = Reader::new("[-1.5e3]", WIDE);
        let mut num = "";
        n.array(|r, _| {
            num = r.number()?;
            Ok(())
        })
        .unwrap();
        assert_eq!(num, "-1.5e3");
    }

    #[test]
    fn a_callback_that_reads_nothing_is_malformed() {
        let mut r = Reader::new(r#"{"a":1}"#, WIDE);
        assert_eq!(r.object(|_, _| Ok(())), Err(Stop::Malformed));
    }
}
