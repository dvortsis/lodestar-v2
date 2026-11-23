//! IRC lines: framing, parsing and encoding (RFC 1459 and RFC 2812
//! framing, IRCv3 message tags).
//!
//! Everything here reads untrusted server bytes, so every size is bounded
//! and nothing panics on hostile input:
//!
//! - **A line** is at most [`MAX_LINE_BYTES`] including its `LF`: a tags
//!   section of at most [`MAX_TAGS_BYTES`] (IRCv3's limit for what a server
//!   sends, `@` and the trailing space included) and a body of at most
//!   [`MAX_BODY_BYTES`] including `CR LF` (RFC 1459). [`LineBuffer`] never
//!   holds more than one line's worth: a longer line is reported once as
//!   [`Framed::Overlong`] and discarded up to its `LF`, never accumulated.
//! - **Tags** are capped at [`MAX_TAGS`]; later ones are ignored, as are
//!   tags whose key isn't a valid IRCv3 key. A repeated key keeps the last
//!   value (IRCv3).
//! - **Parameters** are capped at [`MAX_PARAMS`]: as RFC 2812 says, the
//!   fifteenth takes the rest of the line, so a long line can't make more.
//! - **Text** is UTF-8 when the tags section (and, separately, the body)
//!   is valid UTF-8, and Latin-1 otherwise, so every byte maps to a
//!   character and nothing is lost.
//!   `NUL`, and `CR` anywhere but before the `LF`, make the line invalid.

use std::fmt;

use zeroize::Zeroizing;

/// The longest tags section a server may send, `@` and the space after it
/// included (IRCv3 message-tags).
pub const MAX_TAGS_BYTES: usize = 8191;
/// The longest body (source, command and parameters), `CR LF` included
/// (RFC 1459 §2.3). Outgoing lines have the same limit.
pub const MAX_BODY_BYTES: usize = 512;
/// The longest line [`LineBuffer`] passes on, its `LF` included.
pub const MAX_LINE_BYTES: usize = MAX_TAGS_BYTES + MAX_BODY_BYTES;
/// The most parameters a message has (RFC 2812 §2.3.1).
pub const MAX_PARAMS: usize = 15;
/// The most tags kept from one line; later ones are ignored.
pub const MAX_TAGS: usize = 64;
/// The longest command name accepted.
pub const MAX_COMMAND: usize = 32;

/// One IRCv3 message tag, its value unescaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub key: String,
    /// `None` for a tag with no `=`; an empty value is `Some("")`.
    pub value: Option<String>,
}

/// A parsed IRC message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub tags: Vec<Tag>,
    /// The source (prefix) without its `:`: `nick!user@host` or a server
    /// name.
    pub source: Option<String>,
    /// The command: uppercase letters, or three digits for a numeric reply.
    pub command: String,
    /// At most [`MAX_PARAMS`]; the last may be empty or contain spaces.
    pub params: Vec<String>,
}

impl Message {
    /// The nick of the source: everything before the first `!` or `@`.
    /// For a server source that's the server's name.
    pub fn nick(&self) -> Option<&str> {
        let s = self.source.as_deref()?;
        Some(s.split(['!', '@']).next().unwrap_or(s))
    }

    /// The `i`th parameter.
    pub fn param(&self, i: usize) -> Option<&str> {
        self.params.get(i).map(String::as_str)
    }

    /// The numeric reply code, for a three-digit command.
    pub fn numeric(&self) -> Option<u16> {
        let b = self.command.as_bytes();
        if b.len() == 3 && b.iter().all(u8::is_ascii_digit) {
            self.command.parse().ok()
        } else {
            None
        }
    }

    /// A tag's value (`Some("")` for a tag without one).
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.key == key)
            .map(|t| t.value.as_deref().unwrap_or(""))
    }

    /// The CTCP body when this is a `PRIVMSG` or `NOTICE` whose text starts
    /// with `\x01` (CTCP, including DCC and `ACTION`).
    pub fn ctcp(&self) -> Option<&str> {
        if self.command != "PRIVMSG" && self.command != "NOTICE" {
            return None;
        }
        let text = self.params.get(1)?;
        let body = text.strip_prefix('\u{1}')?;
        Some(body.strip_suffix('\u{1}').unwrap_or(body))
    }
}

/// Why a line isn't a valid IRC message. No variant carries line content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// The tags section is over [`MAX_TAGS_BYTES`].
    TagsTooLong,
    /// The body is over [`MAX_BODY_BYTES`].
    TooLong,
    /// A `NUL`, or a `CR` or `LF` inside the line.
    BadByte,
    /// A `:` with no source after it, or a tags section with nothing after.
    BadSource,
    /// No command.
    NoCommand,
    /// The command isn't letters or three digits.
    BadCommand,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ParseError::TagsTooLong => "the tags section is too long",
            ParseError::TooLong => "the line is too long",
            ParseError::BadByte => "the line has a NUL, CR or LF in it",
            ParseError::BadSource => "the source is empty",
            ParseError::NoCommand => "the line has no command",
            ParseError::BadCommand => "the command isn't valid",
        })
    }
}

impl std::error::Error for ParseError {}

/// Parses one line, without its `LF` (a final `CR` is dropped). A blank
/// line is `Ok(None)`: RFC 2812 says to ignore it.
pub fn parse(line: &[u8]) -> Result<Option<Message>, ParseError> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.iter().any(|&b| matches!(b, 0 | b'\r' | b'\n')) {
        return Err(ParseError::BadByte);
    }
    // Byte limits apply to the raw line, before any decoding.
    let (tags_raw, body) = match line.strip_prefix(b"@") {
        Some(rest) => {
            let end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
            // `@`, the tags and one space.
            if end + 2 > MAX_TAGS_BYTES {
                return Err(ParseError::TagsTooLong);
            }
            (Some(&rest[..end]), rest.get(end..).unwrap_or_default())
        }
        None => (None, line),
    };
    let body = trim_spaces(body);
    if body.len() + 2 > MAX_BODY_BYTES {
        return Err(ParseError::TooLong);
    }
    if body.is_empty() {
        return if tags_raw.is_some() {
            Err(ParseError::BadSource)
        } else {
            Ok(None)
        };
    }
    let tags = tags_raw.map(|t| parse_tags(&decode(t))).unwrap_or_default();
    let body = decode(body);
    let mut rest = body.as_str();

    let source = match rest.strip_prefix(':') {
        Some(r) => {
            let end = r.find(' ').unwrap_or(r.len());
            if end == 0 {
                return Err(ParseError::BadSource);
            }
            let source = r[..end].to_owned();
            rest = r[end..].trim_start_matches(' ');
            Some(source)
        }
        None => None,
    };

    let end = rest.find(' ').unwrap_or(rest.len());
    let command = &rest[..end];
    if command.is_empty() {
        return Err(ParseError::NoCommand);
    }
    if !valid_command(command) {
        return Err(ParseError::BadCommand);
    }
    let command = command.to_ascii_uppercase();
    rest = rest[end..].trim_start_matches(' ');

    let mut params = Vec::new();
    while !rest.is_empty() {
        if let Some(trailing) = rest.strip_prefix(':') {
            params.push(trailing.to_owned());
            break;
        }
        if params.len() == MAX_PARAMS - 1 {
            // RFC 2812: the fifteenth parameter is the rest of the line.
            params.push(rest.to_owned());
            break;
        }
        let end = rest.find(' ').unwrap_or(rest.len());
        params.push(rest[..end].to_owned());
        rest = rest[end..].trim_start_matches(' ');
    }

    Ok(Some(Message {
        tags,
        source,
        command,
        params,
    }))
}

/// Drops leading spaces, and trailing spaces unless they're part of a
/// trailing parameter (`... :text  ` keeps its spaces).
fn trim_spaces(body: &[u8]) -> &[u8] {
    let start = body.iter().position(|&b| b != b' ').unwrap_or(body.len());
    let body = &body[start..];
    // A `" :"` starts a trailing parameter; spaces after it are content.
    if body.windows(2).any(|w| w == b" :") {
        return body;
    }
    let end = body.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    &body[..end]
}

/// UTF-8 when the bytes are valid UTF-8, Latin-1 otherwise.
fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

fn valid_command(c: &str) -> bool {
    let b = c.as_bytes();
    (b.len() == 3 && b.iter().all(u8::is_ascii_digit))
        || (b.len() <= MAX_COMMAND && b.iter().all(u8::is_ascii_alphabetic))
}

fn parse_tags(raw: &str) -> Vec<Tag> {
    let mut tags: Vec<Tag> = Vec::new();
    for item in raw.split(';') {
        let (key, value) = match item.split_once('=') {
            Some((k, v)) => (k, Some(unescape_tag(v))),
            None => (item, None),
        };
        if !valid_tag_key(key) {
            continue;
        }
        if let Some(t) = tags.iter_mut().find(|t| t.key == key) {
            t.value = value;
        } else if tags.len() < MAX_TAGS {
            tags.push(Tag {
                key: key.to_owned(),
                value,
            });
        }
    }
    tags
}

/// `[+][vendor/]name`: letters, digits, `-`, and `.` or `/` in the vendor.
fn valid_tag_key(key: &str) -> bool {
    let key = key.strip_prefix('+').unwrap_or(key);
    !key.is_empty()
        && !key.ends_with('/')
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'/'))
}

/// IRCv3 tag-value unescaping: `\:` `;`, `\s` space, `\\` `\`, `\r` CR,
/// `\n` LF; any other escaped character stands for itself, and a lone
/// final `\` is dropped.
pub fn unescape_tag(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// The inverse of [`unescape_tag`].
pub fn escape_tag(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            ';' => out.push_str("\\:"),
            ' ' => out.push_str("\\s"),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// Why an outgoing line can't be sent. No variant carries the content,
/// which may be a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// The command isn't letters or three digits.
    BadCommand,
    /// More than [`MAX_PARAMS`] parameters.
    TooManyParams,
    /// A parameter has a `NUL`, `CR` or `LF`.
    BadByte,
    /// A parameter other than the last is empty, has a space or starts
    /// with `:`.
    BadMiddle,
    /// The line would be over [`MAX_BODY_BYTES`].
    TooLong,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EncodeError::BadCommand => "the command isn't valid",
            EncodeError::TooManyParams => "too many parameters",
            EncodeError::BadByte => "a parameter has a NUL, CR or LF in it",
            EncodeError::BadMiddle => {
                "a parameter before the last is empty, has a space or starts with ':'"
            }
            EncodeError::TooLong => "the line would be too long",
        })
    }
}

impl std::error::Error for EncodeError {}

/// Encodes one outgoing line, `CR LF` included. The last parameter is sent
/// as a trailing parameter (`:`) when it must be: empty, with a space, or
/// starting with `:`. Every other parameter must be a plain middle one, so
/// no parameter can smuggle in a second command or extra parameters.
///
/// The buffer is allocated once at its final size and wiped when dropped,
/// since lines can carry credentials (SASL, `PASS`, `IDENTIFY`).
pub fn encode(command: &str, params: &[&str]) -> Result<Zeroizing<Vec<u8>>, EncodeError> {
    if !valid_command(command) {
        return Err(EncodeError::BadCommand);
    }
    if params.len() > MAX_PARAMS {
        return Err(EncodeError::TooManyParams);
    }
    let mut len = command.len() + 2;
    let mut colon = false;
    for (i, p) in params.iter().enumerate() {
        if p.bytes().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
            return Err(EncodeError::BadByte);
        }
        let plain = !p.is_empty() && !p.contains(' ') && !p.starts_with(':');
        if i + 1 < params.len() {
            if !plain {
                return Err(EncodeError::BadMiddle);
            }
        } else {
            colon = !plain;
        }
        len += 1 + p.len();
    }
    if colon {
        len += 1;
    }
    if len > MAX_BODY_BYTES {
        return Err(EncodeError::TooLong);
    }
    let mut out = Zeroizing::new(Vec::with_capacity(len));
    out.extend_from_slice(command.as_bytes());
    for (i, p) in params.iter().enumerate() {
        out.push(b' ');
        if colon && i + 1 == params.len() {
            out.push(b':');
        }
        out.extend_from_slice(p.as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    debug_assert_eq!(out.len(), len);
    Ok(out)
}

/// What [`LineBuffer::feed`] hands back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framed<'a> {
    /// A complete line, without its `LF` (a `CR` before it is kept).
    Line(&'a [u8]),
    /// A line longer than [`MAX_LINE_BYTES`]: reported once and discarded
    /// up to its `LF`.
    Overlong,
}

/// Splits a byte stream into lines, holding at most one line's worth
/// ([`MAX_LINE_BYTES`]) at any time.
#[derive(Debug)]
pub struct LineBuffer {
    buf: Vec<u8>,
    /// Inside an over-long line: bytes are dropped until the next `LF`.
    discarding: bool,
}

impl Default for LineBuffer {
    fn default() -> Self {
        LineBuffer::new()
    }
}

impl LineBuffer {
    pub fn new() -> LineBuffer {
        LineBuffer {
            buf: Vec::new(),
            discarding: false,
        }
    }

    /// The bytes held for an unfinished line; never over
    /// [`MAX_LINE_BYTES`].
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Feeds bytes read from the stream, calling `on` for each complete
    /// line and each over-long one, in order.
    pub fn feed(&mut self, mut data: &[u8], mut on: impl FnMut(Framed<'_>)) {
        // A line's content, without its LF.
        const MAX_CONTENT: usize = MAX_LINE_BYTES - 1;
        while !data.is_empty() {
            let Some(i) = data.iter().position(|&b| b == b'\n') else {
                if !self.discarding {
                    if self.buf.len() + data.len() > MAX_CONTENT {
                        self.buf.clear();
                        self.discarding = true;
                        on(Framed::Overlong);
                    } else {
                        self.buf.extend_from_slice(data);
                    }
                }
                return;
            };
            let chunk = &data[..i];
            data = &data[i + 1..];
            if self.discarding {
                self.discarding = false;
            } else if self.buf.len() + chunk.len() > MAX_CONTENT {
                self.buf.clear();
                on(Framed::Overlong);
            } else if self.buf.is_empty() {
                on(Framed::Line(chunk));
            } else {
                self.buf.extend_from_slice(chunk);
                on(Framed::Line(&self.buf));
                self.buf.clear();
            }
        }
    }
}

/// One entry of a `CAP LS`, `ACK`, `NAK`, `NEW` or `DEL` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapEntry<'a> {
    pub name: &'a str,
    /// The value after `=` (`sasl=PLAIN,EXTERNAL`).
    pub value: Option<&'a str>,
    /// A `-` prefix: the capability is being turned off (`CAP ACK :-x`).
    pub removed: bool,
}

/// The entries of a capability list. Lazy, so a long list costs nothing
/// beyond the line it came in.
pub fn cap_entries(list: &str) -> impl Iterator<Item = CapEntry<'_>> {
    list.split(' ').filter(|s| !s.is_empty()).map(|item| {
        let (removed, item) = match item.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, item),
        };
        // CAP 3.1's `~` and `=` modifiers are obsolete; read past them.
        let item = item.trim_start_matches(['~', '=']);
        let (name, value) = match item.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (item, None),
        };
        CapEntry {
            name,
            value,
            removed,
        }
    })
}

/// Whether a `sasl` capability's value (absent, or a comma list of
/// mechanisms) allows `mechanism`. An absent value means the server didn't
/// say (CAP 3.1), so the mechanism is tried.
pub fn sasl_allows(value: Option<&str>, mechanism: &str) -> bool {
    match value {
        None | Some("") => true,
        Some(v) => v.split(',').any(|m| m.eq_ignore_ascii_case(mechanism)),
    }
}

/// Removes mIRC formatting: bold, italics, underline, strike-through,
/// monospace, reverse, reset, and colour codes with their digits
/// (`\x03NN[,NN]`) or hex colours (`\x04RRGGBB[,RRGGBB]`). The result is
/// never longer than the input.
pub fn strip_formatting(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{2}' | '\u{1d}' | '\u{1f}' | '\u{1e}' | '\u{11}' | '\u{16}' | '\u{f}' => {}
            '\u{3}' => {
                if take_digits(&mut chars, 2) > 0 && chars.peek() == Some(&',') {
                    let mut ahead = chars.clone();
                    ahead.next();
                    if ahead.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                        take_digits(&mut chars, 2);
                    }
                }
            }
            '\u{4}' => {
                if take_hex(&mut chars) && chars.peek() == Some(&',') {
                    let mut ahead = chars.clone();
                    ahead.next();
                    if take_hex(&mut ahead) {
                        chars = ahead;
                    }
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, max: usize) -> usize {
    let mut n = 0;
    while n < max && chars.peek().is_some_and(char::is_ascii_digit) {
        chars.next();
        n += 1;
    }
    n
}

/// Six hex digits, consumed only when all six are there.
fn take_hex(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let ahead = chars.clone();
    if ahead.take(6).filter(char::is_ascii_hexdigit).count() == 6 {
        for _ in 0..6 {
            chars.next();
        }
        true
    } else {
        false
    }
}

/// Whether two nicks or channel names are equal under RFC 1459 case
/// mapping (`A-Z` and `[]\~` fold to `a-z` and `{}|^`), the default when a
/// server doesn't say otherwise.
pub fn irc_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| fold(x) == fold(y))
}

fn fold(b: u8) -> u8 {
    match b {
        b'A'..=b'Z' => b + 32,
        b'[' => b'{',
        b']' => b'}',
        b'\\' => b'|',
        b'~' => b'^',
        b => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn p(line: &str) -> Message {
        parse(line.as_bytes()).unwrap().unwrap()
    }

    /// Golden lines: synthetic, covering each part of the grammar.
    #[test]
    fn golden_lines() {
        let m = p(":irc.example.test 001 lode :Welcome to the network, lode\r");
        assert_eq!(m.source.as_deref(), Some("irc.example.test"));
        assert_eq!(m.command, "001");
        assert_eq!(m.numeric(), Some(1));
        assert_eq!(m.params, ["lode", "Welcome to the network, lode"]);

        let m = p("PING :token-1");
        assert_eq!(m.source, None);
        assert_eq!(m.params, ["token-1"]);

        let m = p(
            ":Announcer!bot@services.example.test PRIVMSG #announce :New: Some.Release.2026 [1.2 GB]",
        );
        assert_eq!(m.nick(), Some("Announcer"));
        assert_eq!(m.params[0], "#announce");
        assert_eq!(m.params[1], "New: Some.Release.2026 [1.2 GB]");

        let m = p("@time=2026-11-23T10:00:00.000Z;msgid=abc\\s1;+draft/x :n!u@h privmsg #c hi");
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.tag("time"), Some("2026-11-23T10:00:00.000Z"));
        assert_eq!(m.tag("msgid"), Some("abc 1"));
        assert_eq!(m.tag("+draft/x"), Some(""));
        assert_eq!(m.params, ["#c", "hi"]);

        let m = p(":srv CAP * LS * :multi-prefix sasl=PLAIN,EXTERNAL");
        assert_eq!(
            m.params,
            ["*", "LS", "*", "multi-prefix sasl=PLAIN,EXTERNAL"]
        );

        // Several spaces between parameters, a trailing parameter with
        // spaces kept, an empty trailing parameter.
        let m = p(":s   NOTICE   *   :  spaced  ");
        assert_eq!(m.params, ["*", "  spaced  "]);
        let m = p("AWAY :");
        assert_eq!(m.params, [""]);
        let m = p("JOIN #a   ");
        assert_eq!(m.params, ["#a"]);

        // Fifteen parameters: the last takes the rest.
        let m = p("CMD 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 :17");
        assert_eq!(m.params.len(), 15);
        assert_eq!(m.params[14], "15 16 :17");

        // Latin-1 when not UTF-8.
        let m = parse(b":n PRIVMSG #c :caf\xe9").unwrap().unwrap();
        assert_eq!(m.params[1], "café");
    }

    #[test]
    fn invalid_lines() {
        assert_eq!(parse(b""), Ok(None));
        assert_eq!(parse(b"\r"), Ok(None));
        assert_eq!(parse(b"   "), Ok(None));
        assert_eq!(parse(b"@a=b"), Err(ParseError::BadSource));
        assert_eq!(parse(b": PING"), Err(ParseError::BadSource));
        assert_eq!(parse(b":src"), Err(ParseError::NoCommand));
        assert_eq!(parse(b"PI1NG x"), Err(ParseError::BadCommand));
        assert_eq!(parse(b"1234 x"), Err(ParseError::BadCommand));
        assert_eq!(parse(b"PING a\0b"), Err(ParseError::BadByte));
        assert_eq!(parse(b"PING a\rb"), Err(ParseError::BadByte));
        let long = format!("PRIVMSG #c :{}", "x".repeat(500));
        assert_eq!(parse(long.as_bytes()), Err(ParseError::TooLong));
        let ok = format!("PRIVMSG #c :{}", "x".repeat(510 - 12));
        assert!(parse(ok.as_bytes()).unwrap().is_some());
        let tags = format!("@k={} PING", "v".repeat(MAX_TAGS_BYTES));
        assert_eq!(parse(tags.as_bytes()), Err(ParseError::TagsTooLong));
    }

    #[test]
    fn tags_are_capped_deduplicated_and_validated() {
        let many: Vec<String> = (0..200).map(|i| format!("k{i}=v")).collect();
        let line = format!("@{} PING", many.join(";"));
        let m = p(&line);
        assert_eq!(m.tags.len(), MAX_TAGS);
        let m = p("@a=1;a=2;bad!key;=x;ok/;v.example/k=\\ PING");
        assert_eq!(m.tag("a"), Some("2"));
        assert_eq!(m.tags.len(), 2);
        assert_eq!(m.tag("v.example/k"), Some(""));
    }

    #[test]
    fn tag_escapes() {
        assert_eq!(
            unescape_tag("a\\:b\\sc\\\\d\\re\\nf\\xg\\"),
            "a;b c\\d\re\nfxg"
        );
        assert_eq!(escape_tag("a;b c\\d\re\n"), "a\\:b\\sc\\\\d\\re\\n");
    }

    #[test]
    fn encoding_refuses_injection() {
        assert_eq!(
            &encode("PRIVMSG", &["#c", "hello there"]).unwrap()[..],
            b"PRIVMSG #c :hello there\r\n"
        );
        assert_eq!(&encode("NICK", &["lode"]).unwrap()[..], b"NICK lode\r\n");
        assert_eq!(&encode("PONG", &[""]).unwrap()[..], b"PONG :\r\n");
        assert_eq!(&encode("PONG", &[":x"]).unwrap()[..], b"PONG ::x\r\n");
        assert_eq!(
            encode("PRIVMSG", &["#c", "a\r\nQUIT"]),
            Err(EncodeError::BadByte)
        );
        assert_eq!(
            encode("PRIVMSG", &["#c x", "t"]),
            Err(EncodeError::BadMiddle)
        );
        assert_eq!(encode("PRIVMSG", &[":c", "t"]), Err(EncodeError::BadMiddle));
        assert_eq!(encode("PRIVMSG", &["", "t"]), Err(EncodeError::BadMiddle));
        assert_eq!(encode("PRIV MSG", &[]), Err(EncodeError::BadCommand));
        assert_eq!(encode("X", &["a"; 16]), Err(EncodeError::TooManyParams));
        let text = "x".repeat(500);
        assert_eq!(encode("PRIVMSG", &["#c", &text]), Err(EncodeError::TooLong));
        let fits = "x".repeat(512 - "PRIVMSG #c \r\n".len());
        assert_eq!(encode("PRIVMSG", &["#c", &fits]).unwrap().len(), 512);
    }

    #[test]
    fn line_buffer_frames_and_drops_overlong_lines() {
        let mut lb = LineBuffer::new();
        let mut got = Vec::new();
        let mut push = |f: Framed<'_>| {
            got.push(match f {
                Framed::Line(l) => String::from_utf8_lossy(l).into_owned(),
                Framed::Overlong => "<overlong>".to_owned(),
            })
        };
        lb.feed(b"PING a\r\nPI", &mut push);
        lb.feed(b"NG b\r\n", &mut push);
        let big = vec![b'x'; MAX_LINE_BYTES * 3];
        lb.feed(&big, &mut push);
        assert_eq!(lb.pending(), 0);
        lb.feed(&big, &mut push);
        lb.feed(b"tail\r\nPING c\n", &mut push);
        // Exactly at the limit, LF included: kept.
        let mut exact = vec![b'y'; MAX_LINE_BYTES - 1];
        exact.push(b'\n');
        lb.feed(&exact[..100], &mut push);
        lb.feed(&exact[100..], &mut push);
        // One over, split across reads.
        let mut over = vec![b'z'; MAX_LINE_BYTES];
        over.push(b'\n');
        lb.feed(&over[..MAX_LINE_BYTES - 1], &mut push);
        lb.feed(&over[MAX_LINE_BYTES - 1..], &mut push);
        lb.feed(b"PING d\n", &mut push);
        assert_eq!(got.len(), 7);
        assert_eq!(&got[..4], ["PING a\r", "PING b\r", "<overlong>", "PING c"]);
        assert_eq!(got[4].len(), MAX_LINE_BYTES - 1);
        assert_eq!(&got[5..], ["<overlong>", "PING d"]);
    }

    #[test]
    fn caps_and_sasl_mechanisms() {
        let caps: Vec<_> =
            cap_entries(" multi-prefix  sasl=PLAIN,EXTERNAL -away-notify ~ack ").collect();
        assert_eq!(caps.len(), 4);
        assert_eq!(caps[1].name, "sasl");
        assert_eq!(caps[1].value, Some("PLAIN,EXTERNAL"));
        assert!(caps[2].removed);
        assert_eq!(caps[3].name, "ack");
        assert!(sasl_allows(Some("EXTERNAL,plain"), "PLAIN"));
        assert!(!sasl_allows(Some("EXTERNAL"), "PLAIN"));
        assert!(sasl_allows(None, "PLAIN"));
    }

    #[test]
    fn formatting_is_stripped() {
        assert_eq!(
            strip_formatting("\u{2}New\u{2}: \u{3}04,01Red\u{3} \u{3}9x \u{3},5y"),
            "New: Red x ,5y"
        );
        assert_eq!(
            strip_formatting("\u{4}FF0000,00FF00hex\u{4}12345 z"),
            "hex12345 z"
        );
        assert_eq!(strip_formatting("a\u{3}12,b"), "a,b");
        assert_eq!(
            strip_formatting("\u{1d}\u{1f}\u{1e}\u{11}\u{16}\u{f}plain"),
            "plain"
        );
    }

    #[test]
    fn case_mapping() {
        assert!(irc_eq("#Chan[1]", "#chan{1}"));
        assert!(irc_eq("Nick\\~", "nick|^"));
        assert!(!irc_eq("#a", "#b"));
        assert!(!irc_eq("#a", "#aa"));
    }

    fn middle() -> impl Strategy<Value = String> {
        "[A-Za-z0-9#&!.\\-\\[\\]{}|^_*=,]{1,20}"
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// Hostile bytes: never a panic, and anything parsed stays within
        /// the limits.
        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..9000)) {
            if let Ok(Some(m)) = parse(&bytes) {
                prop_assert!(m.params.len() <= MAX_PARAMS);
                prop_assert!(m.tags.len() <= MAX_TAGS);
                prop_assert!(valid_command(&m.command));
                // Latin-1 at most doubles the bytes.
                let total: usize = m.params.iter().map(String::len).sum();
                prop_assert!(total <= 2 * MAX_BODY_BYTES);
            }
        }

        /// IRC-shaped input that's more likely to reach deep paths.
        #[test]
        fn parse_never_panics_on_irc_shapes(s in "(@[a-z=;\\\\ ]{0,30} )?(:[a-z!@.]{0,10} )?[A-Z0-9]{0,4}( [:a-z ]{0,12}){0,20}\r?") {
            let _ = parse(s.as_bytes());
        }

        /// What `encode` writes, `parse` reads back unchanged.
        #[test]
        fn encode_parse_roundtrip(
            cmd in "[A-Z]{1,10}",
            mids in proptest::collection::vec(middle(), 0..14),
            last in proptest::option::of("[ -~]{0,40}"),
        ) {
            let mut params: Vec<&str> = mids.iter().map(String::as_str).collect();
            if let Some(l) = &last { params.push(l); }
            match encode(&cmd, &params) {
                Ok(line) => {
                    let m = parse(&line[..line.len() - 1]).unwrap().unwrap();
                    prop_assert_eq!(m.command, cmd);
                    prop_assert_eq!(m.params, params);
                }
                Err(e) => prop_assert_eq!(e, EncodeError::TooLong),
            }
        }

        /// Framing doesn't depend on how the stream was cut, and the buffer
        /// never holds more than a line.
        #[test]
        fn line_buffer_is_split_independent(
            data in proptest::collection::vec(prop_oneof![Just(b'\n'), Just(b'a'), any::<u8>()], 0..30000),
            cuts in proptest::collection::vec(1usize..12000, 0..8),
        ) {
            let collect = |pieces: Vec<&[u8]>| {
                let mut lb = LineBuffer::new();
                let mut out = Vec::new();
                for piece in pieces {
                    lb.feed(piece, |f| out.push(match f {
                        Framed::Line(l) => Some(l.to_vec()),
                        Framed::Overlong => None,
                    }));
                    assert!(lb.pending() < MAX_LINE_BYTES);
                }
                out
            };
            let whole = collect(vec![&data]);
            let mut pieces = Vec::new();
            let mut rest: &[u8] = &data;
            for c in cuts {
                let c = c.min(rest.len());
                let (a, b) = rest.split_at(c);
                pieces.push(a);
                rest = b;
            }
            pieces.push(rest);
            prop_assert_eq!(whole, collect(pieces));
        }

        #[test]
        fn tag_escape_roundtrip(v in "\\PC{0,40}|[;\\\\ \r\n a]{0,20}") {
            prop_assert_eq!(unescape_tag(&escape_tag(&v)), v);
        }

        #[test]
        fn strip_formatting_never_grows(s in "[\u{2}\u{3}\u{4}\u{f}0-9a-fA-F,x ]{0,60}|\\PC{0,40}") {
            let out = strip_formatting(&s);
            prop_assert!(out.len() <= s.len());
            let codes = ['\u{2}', '\u{3}', '\u{4}', '\u{f}'];
            prop_assert!(!out.contains(codes));
        }
    }
}
