//! Announce definitions and the matcher (architecture §18.2 rank 12,
//! §18.7; plan `docs/plan/2026-11-05-v07-m7.md` IRC-1 and Q7).
//!
//! A private tracker's IRC announce channel posts one line per new
//! upload. A [`Definition`] says, for one tracker, which network and
//! channels to read, which nicks announce, how to read their lines and
//! where the `.torrent` lives. A [`Matcher`] turns a [`Message`] from
//! [`crate::Events`] into an [`Announce`]: a name, a size, the tracker's
//! category and its torrent id. IRC-2 turns those into claims (`name`,
//! `size` and the details section's `source_category`), every one
//! `private_source`.
//!
//! **The format is Lodestar's own** (plan Q7): a short text file of
//! `key = value` lines, described in `data/announce/README.md`.
//! Definitions are written from each tracker's published announce
//! format; nothing is copied from another project's definitions. A
//! definition may come from the user, so [`Definition::parse`] reads
//! untrusted text: every size is bounded, nothing panics, and it's
//! fuzzed (`fuzz/fuzz_targets/irc_announce.rs`).
//!
//! **Patterns are regular expressions with named captures,** compiled
//! with the `regex` crate's size limits ([`PATTERN_SIZE_LIMIT`],
//! [`PATTERN_DFA_LIMIT`], [`PATTERN_NEST_LIMIT`]). Matching is linear in
//! the line's length, and a line is at most one IRC line, so a hostile
//! pattern or line costs a bounded amount of time and memory.
//!
//! **Passkeyed URLs are never stored or logged.** An [`Announce`] holds
//! the torrent id, never a URL. [`Definition::torrent_url`] fills the
//! definition's URL template with the user's passkey only when the
//! `.torrent` is about to be fetched, and returns a [`SecretUrl`]: wiped
//! on drop, not `Clone`, and redacted in `Debug`. A template with what
//! looks like a literal passkey in it is refused ([`DefinitionError::LiteralSecret`]),
//! so a pasted personal link can't end up saved in a definition, and a
//! captured field holding a URL makes the line a [`Reject::Url`], so no
//! URL reaches a claim through a pattern.
//!
//! **Server text is cleaned first.** The announce text and every
//! captured field go through [`line::clean_text`] (mIRC formatting,
//! control and format characters removed) before anything is matched or
//! returned, as IRC-0's decisions require of every consumer of `Events`.
//! The matcher keeps no per-message state: it holds only its compiled
//! definitions, so it adds nothing to a client's event-queue budget.

use std::fmt;

use lodestar_integrations::Secret;
use regex::{Regex, RegexBuilder};
use zeroize::Zeroizing;

use crate::client::{valid_channel, valid_nick};
use crate::line::{self, Message, irc_eq};

/// The format version a definition declares on its first line.
pub const FORMAT_VERSION: u32 = 1;
/// The longest definition file accepted.
pub const MAX_DEFINITION_BYTES: usize = 16 * 1024;
/// The longest line in a definition file.
pub const MAX_DEFINITION_LINE: usize = 2048;
/// The longest tracker id (`[a-z0-9-]`).
pub const MAX_ID: usize = 32;
/// The longest display name.
pub const MAX_DISPLAY_NAME: usize = 64;
/// The longest network host name.
pub const MAX_NETWORK: usize = 253;
/// Channels per definition.
pub const MAX_CHANNELS: usize = 8;
/// Announcer nicks per definition, and announcer hosts.
pub const MAX_ANNOUNCERS: usize = 8;
/// The longest announcer host (a host name, address or cloak).
pub const MAX_ANNOUNCER_HOST: usize = 253;
/// Line patterns per definition.
pub const MAX_PATTERNS: usize = 4;
/// The longest pattern source.
pub const MAX_PATTERN: usize = 1024;
/// The compiled program's limit per pattern (`regex`'s `size_limit`).
pub const PATTERN_SIZE_LIMIT: usize = 128 * 1024;
/// The lazy DFA's cache limit per pattern (`regex`'s `dfa_size_limit`).
pub const PATTERN_DFA_LIMIT: usize = 128 * 1024;
/// The deepest nesting a pattern may have (`regex`'s `nest_limit`).
pub const PATTERN_NEST_LIMIT: u32 = 32;
/// The longest URL template.
pub const MAX_URL_TEMPLATE: usize = 512;
/// Definitions in one [`Matcher`].
pub const MAX_DEFINITIONS: usize = 32;
/// Patterns across every definition in one [`Matcher`]. With the limits
/// above, a matcher's compiled programs hold at most 64 × 128 KiB = 8 MiB,
/// and each pattern's lazy-DFA cache at most 128 KiB for each thread
/// matching with it at once: bounded, whatever the definitions say.
pub const MAX_MATCHER_PATTERNS: usize = 64;
/// The longest announced name kept (the claims' name limit).
pub const MAX_NAME: usize = 1024;
/// The longest category kept (the details section's category limit).
pub const MAX_CATEGORY: usize = 256;
/// The longest torrent id kept.
pub const MAX_TORRENT_ID: usize = 64;
/// The largest size an announce may state: 1 PiB. Anything larger is a
/// mistake or a lie, and is dropped (the announce is kept without it).
pub const MAX_SIZE: u64 = 1 << 50;
/// A run of this many letters and digits in a URL template outside a
/// placeholder looks like a pasted passkey and is refused.
pub const LITERAL_SECRET_RUN: usize = 20;
/// As many hex digits in a run that may also hold `-` (a UUID, or a hex
/// key written in groups) look like a pasted passkey too.
pub const LITERAL_SECRET_HEX: usize = 32;

/// The capture names a pattern may use. `name` is required.
pub const CAPTURES: [&str; 4] = ["name", "size", "category", "id"];

/// Why a definition can't be used. No variant carries the definition's
/// text, which may be the user's and may hold a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionError {
    /// Over [`MAX_DEFINITION_BYTES`].
    TooLong,
    /// A line over [`MAX_DEFINITION_LINE`] (1-based line number).
    LineTooLong(usize),
    /// The first line isn't `lodestar-announce = 1`.
    NotADefinition,
    /// The format version isn't [`FORMAT_VERSION`].
    UnsupportedVersion,
    /// A line isn't a comment, blank, or `key = value`.
    Syntax(usize),
    /// A key the format doesn't have.
    UnknownKey(usize),
    /// A key that may appear once appears again.
    Repeated(&'static str),
    /// A required key is missing.
    Missing(&'static str),
    /// A value is invalid for its key.
    Invalid(&'static str),
    /// More than the limit of a repeatable key.
    TooMany(&'static str),
    /// A pattern doesn't compile within the limits, or isn't valid.
    Pattern(usize, PatternError),
    /// The URL template has a run of letters and digits, or of hex
    /// digits that may hold `-`, that looks like a passkey (use
    /// `{passkey}`).
    LiteralSecret,
}

impl fmt::Display for DefinitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DefinitionError::TooLong => write!(f, "definition over {MAX_DEFINITION_BYTES} bytes"),
            DefinitionError::LineTooLong(n) => write!(f, "line {n} is too long"),
            DefinitionError::NotADefinition => {
                write!(f, "the first line must be `lodestar-announce = 1`")
            }
            DefinitionError::UnsupportedVersion => write!(f, "unsupported format version"),
            DefinitionError::Syntax(n) => write!(f, "line {n} isn't `key = value`"),
            DefinitionError::UnknownKey(n) => write!(f, "line {n} has an unknown key"),
            DefinitionError::Repeated(k) => write!(f, "`{k}` appears more than once"),
            DefinitionError::Missing(k) => write!(f, "`{k}` is missing"),
            DefinitionError::Invalid(k) => write!(f, "`{k}` is invalid"),
            DefinitionError::TooMany(k) => write!(f, "too many `{k}` lines"),
            DefinitionError::Pattern(n, e) => write!(f, "pattern {n}: {e}"),
            DefinitionError::LiteralSecret => write!(
                f,
                "`torrent-url` looks like it holds a passkey; use `{{passkey}}` instead"
            ),
        }
    }
}

impl std::error::Error for DefinitionError {}

/// Why a pattern can't be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternError {
    /// Over [`MAX_PATTERN`] bytes.
    TooLong,
    /// Not a valid regular expression.
    Syntax,
    /// Over the compiled size or nesting limits.
    TooBig,
    /// No `name` capture.
    NoName,
    /// A named capture other than [`CAPTURES`].
    UnknownCapture,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PatternError::TooLong => "too long",
            PatternError::Syntax => "not a valid regular expression",
            PatternError::TooBig => "too large or too deeply nested",
            PatternError::NoName => "has no `name` capture",
            PatternError::UnknownCapture => "has a capture other than name, size, category or id",
        })
    }
}

/// One part of a URL template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Id,
    Name,
    Passkey,
}

/// A tracker's announce definition, validated and compiled.
#[derive(Clone)]
pub struct Definition {
    id: String,
    name: String,
    network: String,
    channels: Vec<String>,
    announcers: Vec<String>,
    announcer_hosts: Vec<String>,
    patterns: Vec<Regex>,
    url: Option<Template>,
}

/// A parsed `torrent-url`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Template {
    /// The host name, without the port.
    host: String,
    parts: Vec<Part>,
}

/// The template is shown only by its placeholders; it holds no secret,
/// but nothing about it is needed in a log.
impl fmt::Debug for Definition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Definition")
            .field("id", &self.id)
            .field("network", &self.network)
            .field("channels", &self.channels.len())
            .field("announcers", &self.announcers.len())
            .field("announcer_hosts", &self.announcer_hosts.len())
            .field("patterns", &self.patterns.len())
            .field("torrent_url", &self.url.is_some())
            .finish()
    }
}

impl Definition {
    /// Parses and compiles a definition file. See `data/announce/README.md`
    /// for the format.
    pub fn parse(text: &str) -> Result<Definition, DefinitionError> {
        if text.len() > MAX_DEFINITION_BYTES {
            return Err(DefinitionError::TooLong);
        }
        let mut seen_header = false;
        let mut id = None;
        let mut name = None;
        let mut network = None;
        let mut url = None;
        let mut channels: Vec<String> = Vec::new();
        let mut announcers: Vec<String> = Vec::new();
        let mut announcer_hosts: Vec<String> = Vec::new();
        let mut patterns: Vec<Regex> = Vec::new();
        // A byte-order mark, as some editors save one.
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        for (i, raw) in text.split('\n').enumerate() {
            let n = i + 1;
            if raw.len() > MAX_DEFINITION_LINE {
                return Err(DefinitionError::LineTooLong(n));
            }
            let raw = raw.strip_suffix('\r').unwrap_or(raw);
            let trimmed = raw.trim_matches([' ', '\t']);
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                return Err(DefinitionError::Syntax(n));
            };
            let key = key.trim_matches([' ', '\t']);
            let value = value.trim_matches([' ', '\t']);
            if !seen_header {
                if key != "lodestar-announce" {
                    return Err(DefinitionError::NotADefinition);
                }
                if value != FORMAT_VERSION.to_string() {
                    return Err(DefinitionError::UnsupportedVersion);
                }
                seen_header = true;
                continue;
            }
            // Values are one line of visible text: no control or format
            // character (a tab inside a value included), and no line or
            // paragraph separator.
            if value.chars().any(|c| {
                c.is_control() || line::is_format_char(c) || matches!(c, '\u{2028}' | '\u{2029}')
            }) {
                return Err(DefinitionError::Invalid(static_key(key).unwrap_or("value")));
            }
            match key {
                "lodestar-announce" => return Err(DefinitionError::Repeated("lodestar-announce")),
                "id" => set_once(&mut id, "id", value, valid_id)?,
                "name" => set_once(&mut name, "name", value, valid_display_name)?,
                "network" => set_once(&mut network, "network", value, valid_host)?,
                "channel" => {
                    if !valid_channel(value) {
                        return Err(DefinitionError::Invalid("channel"));
                    }
                    if !channels.iter().any(|c| irc_eq(c, value)) {
                        if channels.len() == MAX_CHANNELS {
                            return Err(DefinitionError::TooMany("channel"));
                        }
                        channels.push(value.to_owned());
                    }
                }
                "announcer" => {
                    if !valid_nick(value) {
                        return Err(DefinitionError::Invalid("announcer"));
                    }
                    if !announcers.iter().any(|a| irc_eq(a, value)) {
                        if announcers.len() == MAX_ANNOUNCERS {
                            return Err(DefinitionError::TooMany("announcer"));
                        }
                        announcers.push(value.to_owned());
                    }
                }
                "announcer-host" => {
                    if !valid_announcer_host(value) {
                        return Err(DefinitionError::Invalid("announcer-host"));
                    }
                    if !announcer_hosts
                        .iter()
                        .any(|h| h.eq_ignore_ascii_case(value))
                    {
                        if announcer_hosts.len() == MAX_ANNOUNCERS {
                            return Err(DefinitionError::TooMany("announcer-host"));
                        }
                        announcer_hosts.push(value.to_owned());
                    }
                }
                "pattern" => {
                    if patterns.len() == MAX_PATTERNS {
                        return Err(DefinitionError::TooMany("pattern"));
                    }
                    let re = compile(value)
                        .map_err(|e| DefinitionError::Pattern(patterns.len() + 1, e))?;
                    patterns.push(re);
                }
                "torrent-url" => {
                    if url.is_some() {
                        return Err(DefinitionError::Repeated("torrent-url"));
                    }
                    url = Some(parse_template(value)?);
                }
                _ => return Err(DefinitionError::UnknownKey(n)),
            }
        }
        if !seen_header {
            return Err(DefinitionError::NotADefinition);
        }
        let id = id.ok_or(DefinitionError::Missing("id"))?;
        let name = name.ok_or(DefinitionError::Missing("name"))?;
        let network = network.ok_or(DefinitionError::Missing("network"))?;
        if channels.is_empty() {
            return Err(DefinitionError::Missing("channel"));
        }
        if announcers.is_empty() {
            return Err(DefinitionError::Missing("announcer"));
        }
        if patterns.is_empty() {
            return Err(DefinitionError::Missing("pattern"));
        }
        // A template that uses `{id}` needs every pattern to capture it,
        // or some announces could never be fetched.
        if let Some(t) = &url
            && t.parts.contains(&Part::Id)
            && patterns
                .iter()
                .any(|re| !re.capture_names().flatten().any(|c| c == "id"))
        {
            return Err(DefinitionError::Invalid("torrent-url"));
        }
        Ok(Definition {
            id,
            name,
            network,
            channels,
            announcers,
            announcer_hosts,
            patterns,
            url,
        })
    }

    /// The tracker id (`[a-z0-9-]`, at most [`MAX_ID`] bytes).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The tracker's display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The IRC network's host name.
    pub fn network(&self) -> &str {
        &self.network
    }

    /// The channels to join.
    pub fn channels(&self) -> &[String] {
        &self.channels
    }

    /// The nicks whose lines are announces.
    pub fn announcers(&self) -> &[String] {
        &self.announcers
    }

    /// The hosts an announcer's messages must come from; empty when the
    /// definition names none, and any host will do.
    pub fn announcer_hosts(&self) -> &[String] {
        &self.announcer_hosts
    }

    /// Whether the definition has a `torrent-url`.
    pub fn has_torrent_url(&self) -> bool {
        self.url.is_some()
    }

    /// The `torrent-url`'s host name, without the port: where a fetch
    /// would go, for an egress or allow-list check before the URL is made.
    pub fn torrent_url_host(&self) -> Option<&str> {
        self.url.as_ref().map(|t| t.host.as_str())
    }

    /// Whether the `torrent-url` needs the user's passkey.
    pub fn needs_passkey(&self) -> bool {
        self.url
            .as_ref()
            .is_some_and(|t| t.parts.contains(&Part::Passkey))
    }

    /// The `.torrent`'s URL for an announce this definition matched,
    /// with the user's passkey filled in. Call it only when the file is
    /// about to be fetched, and never store or log the result.
    ///
    /// `None` when the definition has no template, the announce came from
    /// another definition, the template needs a value the announce lacks
    /// or has one that is only dots (it would climb the URL's path), or
    /// it needs a passkey and none (or an empty one) was given.
    pub fn torrent_url(&self, announce: &Announce, passkey: Option<&Secret>) -> Option<SecretUrl> {
        let template = self.url.as_ref()?;
        if announce.definition != self.id {
            return None;
        }
        let value = |part: &Part| -> Option<Option<&str>> {
            Some(match part {
                Part::Literal(_) => None,
                Part::Id => Some(announce.torrent_id.as_deref()?),
                Part::Name => Some(announce.name.as_str()),
                Part::Passkey => Some(passkey.filter(|k| !k.is_empty())?.expose()),
            })
        };
        // The exact length first, so the buffer is allocated once and
        // never grows: growing would leave an unwiped copy of the passkey.
        let mut len = 0;
        for part in &template.parts {
            len += match (part, value(part)?) {
                (Part::Literal(s), _) => s.len(),
                (_, Some(v)) if v.bytes().all(|b| b == b'.') => return None,
                (_, Some(v)) => encoded_len(v),
                (_, None) => 0,
            };
        }
        let mut out = Zeroizing::new(String::with_capacity(len));
        for part in &template.parts {
            match (part, value(part)?) {
                (Part::Literal(s), _) => out.push_str(s),
                (_, Some(v)) => percent_encode(v, &mut out),
                (_, None) => {}
            }
        }
        debug_assert_eq!(out.len(), len);
        Some(SecretUrl(out))
    }
}

fn static_key(key: &str) -> Option<&'static str> {
    [
        "id",
        "name",
        "network",
        "channel",
        "announcer",
        "announcer-host",
        "pattern",
        "torrent-url",
    ]
    .into_iter()
    .find(|k| *k == key)
}

fn set_once(
    slot: &mut Option<String>,
    key: &'static str,
    value: &str,
    valid: fn(&str) -> bool,
) -> Result<(), DefinitionError> {
    if slot.is_some() {
        return Err(DefinitionError::Repeated(key));
    }
    if !valid(value) {
        return Err(DefinitionError::Invalid(key));
    }
    *slot = Some(value.to_owned());
    Ok(())
}

fn valid_id(s: &str) -> bool {
    (1..=MAX_ID).contains(&s.len())
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_display_name(s: &str) -> bool {
    (1..=MAX_DISPLAY_NAME).contains(&s.len())
}

/// The host part of an announcer's `nick!user@host`: a host name, an
/// address or a network's cloak (`user/announcer`), so printable ASCII
/// with no space, `!`, `@` or `*`.
fn valid_announcer_host(s: &str) -> bool {
    (1..=MAX_ANNOUNCER_HOST).contains(&s.len())
        // `*` too: a host is matched exactly, so a wildcard would silently
        // never match.
        && s.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'!' | b'@' | b'*'))
}

/// A DNS host name: labels of letters, digits and `-`, separated by dots.
fn valid_host(s: &str) -> bool {
    (1..=MAX_NETWORK).contains(&s.len())
        && s.split('.').all(|l| {
            (1..=63).contains(&l.len())
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn compile(src: &str) -> Result<Regex, PatternError> {
    if src.len() > MAX_PATTERN {
        return Err(PatternError::TooLong);
    }
    let re = RegexBuilder::new(src)
        .size_limit(PATTERN_SIZE_LIMIT)
        .dfa_size_limit(PATTERN_DFA_LIMIT)
        .nest_limit(PATTERN_NEST_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => PatternError::TooBig,
            // `regex` reports going over the nest limit as a syntax error.
            _ => PatternError::Syntax,
        })?;
    let mut has_name = false;
    for c in re.capture_names().flatten() {
        if !CAPTURES.contains(&c) {
            return Err(PatternError::UnknownCapture);
        }
        has_name |= c == "name";
    }
    if !has_name {
        return Err(PatternError::NoName);
    }
    Ok(re)
}

/// `https://host[:port]/...` with `{id}`, `{name}` and `{passkey}`
/// placeholders after the host. `{{` and `}}` aren't supported: a URL
/// has no need of a literal brace.
fn parse_template(t: &str) -> Result<Template, DefinitionError> {
    let bad = DefinitionError::Invalid("torrent-url");
    if t.len() > MAX_URL_TEMPLATE || t.contains([' ', '"', '<', '>', '\\', '^', '`', '|']) {
        return Err(bad);
    }
    let rest = t.strip_prefix("https://").ok_or(bad)?;
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..host_end];
    // No user info (a password could hide there) and no placeholder in
    // the host: the template decides where the passkey goes, never the
    // announce.
    if authority.contains(['@', '{', '}']) {
        return Err(bad);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    // A port is 1–65535, written as plain digits.
    let bad_port = |p: &str| {
        !p.bytes().all(|b| b.is_ascii_digit()) || p.parse::<u16>().map_or(true, |n| n == 0)
    };
    if !valid_host(host) || port.is_some_and(bad_port) {
        return Err(bad);
    }
    let mut parts = vec![Part::Literal(format!("https://{authority}"))];
    let mut path = &rest[host_end..];
    while !path.is_empty() {
        match path.find(['{', '}']) {
            None => {
                push_literal(&mut parts, path);
                break;
            }
            Some(i) if path.as_bytes()[i] == b'}' => return Err(bad),
            Some(i) => {
                push_literal(&mut parts, &path[..i]);
                let after = &path[i + 1..];
                let end = after.find('}').ok_or(bad)?;
                parts.push(match &after[..end] {
                    "id" => Part::Id,
                    "name" => Part::Name,
                    "passkey" => Part::Passkey,
                    _ => return Err(bad),
                });
                path = &after[end + 1..];
            }
        }
    }
    if parts.iter().filter(|p| matches!(p, Part::Passkey)).count() > 1 {
        return Err(bad);
    }
    // A key pasted in as a subdomain (`https://<key>.tracker.example`).
    if host
        .split('.')
        .any(|l| longest_hex_run(l) >= LITERAL_SECRET_HEX)
    {
        return Err(DefinitionError::LiteralSecret);
    }
    for p in &parts[1..] {
        if let Part::Literal(s) = p
            && (longest_alnum_run(s) >= LITERAL_SECRET_RUN
                || longest_hex_run(s) >= LITERAL_SECRET_HEX)
        {
            return Err(DefinitionError::LiteralSecret);
        }
    }
    Ok(Template {
        host: host.to_owned(),
        parts,
    })
}

fn push_literal(parts: &mut Vec<Part>, s: &str) {
    if !s.is_empty() {
        parts.push(Part::Literal(s.to_owned()));
    }
}

fn longest_alnum_run(s: &str) -> usize {
    let mut best = 0;
    let mut run = 0;
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// The most hex digits in a run of hex digits and `-`.
fn longest_hex_run(s: &str) -> usize {
    let mut best = 0;
    let mut run = 0;
    for b in s.bytes() {
        if b.is_ascii_hexdigit() {
            run += 1;
            best = best.max(run);
        } else if b != b'-' {
            run = 0;
        }
    }
    best
}

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// The length of `s` after [`percent_encode`].
fn encoded_len(s: &str) -> usize {
    s.bytes().map(|b| if unreserved(b) { 1 } else { 3 }).sum()
}

/// RFC 3986 unreserved characters as they are, every other byte as `%XX`.
fn percent_encode(s: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in s.as_bytes() {
        if unreserved(b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0xF)]));
        }
    }
}

/// A `.torrent` URL with the user's passkey in it. Wiped on drop, not
/// `Clone`, no `Display`, and `Debug` prints nothing of it: hand it to
/// the fetch and let it go.
pub struct SecretUrl(Zeroizing<String>);

impl SecretUrl {
    /// The URL, for the request only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretUrl(<redacted>)")
    }
}

/// What one announce line says. Every text field is cleaned server text
/// ([`line::clean_text`]): no formatting, control or format character,
/// and no URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announce {
    /// The [`Definition::id`] that matched.
    pub definition: String,
    /// The release name, 1–[`MAX_NAME`] bytes.
    pub name: String,
    /// The size the tracker states, in bytes, when the line has one that
    /// reads as a size up to [`MAX_SIZE`].
    pub size: Option<u64>,
    /// The tracker's own category, 1–[`MAX_CATEGORY`] bytes.
    pub category: Option<String>,
    /// The tracker's torrent id (letters, digits, `-`, `_` and `.`, at
    /// most [`MAX_TORRENT_ID`] bytes), used only to fill the URL template.
    pub torrent_id: Option<String>,
}

/// Why a line from an announcer wasn't an announce. No variant carries
/// the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// The name was empty after cleaning, or over [`MAX_NAME`].
    Name,
    /// A captured field held a URL (`://`).
    Url,
    /// The torrent id was missing or invalid, and the definition's
    /// template needs it.
    TorrentId,
}

/// What the matcher made of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not a channel message from an announcer of any definition.
    Ignored,
    /// From an announcer, in its channel, but no pattern matched (a
    /// chat line, or a format the definition doesn't know yet).
    NoMatch,
    /// A pattern matched but the result can't be used.
    Rejected(Reject),
    Announce(Announce),
}

/// Why definitions can't share a matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatcherError {
    /// Over [`MAX_DEFINITIONS`].
    TooManyDefinitions,
    /// Over [`MAX_MATCHER_PATTERNS`] patterns in all.
    TooManyPatterns,
    /// Two definitions have the same id.
    DuplicateId,
}

impl fmt::Display for MatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MatcherError::TooManyDefinitions => "too many definitions for one network",
            MatcherError::TooManyPatterns => "too many patterns for one network",
            MatcherError::DuplicateId => "two definitions have the same id",
        })
    }
}

impl std::error::Error for MatcherError {}

/// Reads one connection's messages against the definitions for its
/// network.
#[derive(Debug, Clone)]
pub struct Matcher {
    defs: Vec<Definition>,
}

impl Matcher {
    /// A matcher over `defs`. It doesn't check that they share a network:
    /// the caller builds one matcher per connection from the definitions
    /// whose [`Definition::network`] that connection serves.
    pub fn new(defs: Vec<Definition>) -> Result<Matcher, MatcherError> {
        if defs.len() > MAX_DEFINITIONS {
            return Err(MatcherError::TooManyDefinitions);
        }
        if defs.iter().map(|d| d.patterns.len()).sum::<usize>() > MAX_MATCHER_PATTERNS {
            return Err(MatcherError::TooManyPatterns);
        }
        for (i, d) in defs.iter().enumerate() {
            if defs[..i].iter().any(|e| e.id == d.id) {
                return Err(MatcherError::DuplicateId);
            }
        }
        Ok(Matcher { defs })
    }

    /// The definitions, in the order given.
    pub fn definitions(&self) -> &[Definition] {
        &self.defs
    }

    /// The definition with this id.
    pub fn definition(&self, id: &str) -> Option<&Definition> {
        self.defs.iter().find(|d| d.id == id)
    }

    /// Reads one message. Only a `PRIVMSG` or `NOTICE` to a definition's
    /// channel from one of its announcers can be an announce; the first
    /// such definition, and its first matching pattern, decides.
    pub fn read(&self, msg: &Message) -> Outcome {
        if msg.command != "PRIVMSG" && msg.command != "NOTICE" {
            return Outcome::Ignored;
        }
        if msg.ctcp().is_some() {
            return Outcome::Ignored;
        }
        let (Some(target), Some(text), Some(nick)) = (msg.param(0), msg.param(1), msg.nick())
        else {
            return Outcome::Ignored;
        };
        // An announcer is an RFC 2812 nick, which has no dot, so a server
        // name can never pass for one. A definition that names hosts also
        // needs the message's host (the part after `@`) to equal one of
        // them, ignoring case.
        let host = msg
            .source
            .as_deref()
            .and_then(|s| s.split_once('@'))
            .map(|(_, h)| h);
        let Some(def) = self.defs.iter().find(|d| {
            d.channels.iter().any(|c| irc_eq(c, target))
                && d.announcers.iter().any(|a| irc_eq(a, nick))
                && (d.announcer_hosts.is_empty()
                    || host.is_some_and(|h| {
                        d.announcer_hosts.iter().any(|a| a.eq_ignore_ascii_case(h))
                    }))
        }) else {
            return Outcome::Ignored;
        };
        let text = line::clean_text(text);
        for re in &def.patterns {
            if let Some(caps) = re.captures(&text) {
                return build(def, &caps);
            }
        }
        Outcome::NoMatch
    }
}

fn build(def: &Definition, caps: &regex::Captures<'_>) -> Outcome {
    let field = |k: &str| caps.name(k).map(|m| m.as_str().trim_matches(' '));
    let fields = [field("name"), field("size"), field("category"), field("id")];
    if fields.iter().flatten().any(|f| f.contains("://")) {
        return Outcome::Rejected(Reject::Url);
    }
    let [name, size, category, id] = fields;
    let name = name.unwrap_or("");
    if name.is_empty() || name.len() > MAX_NAME {
        return Outcome::Rejected(Reject::Name);
    }
    let torrent_id = id.filter(|i| valid_torrent_id(i)).map(str::to_owned);
    if torrent_id.is_none()
        && def
            .url
            .as_ref()
            .is_some_and(|t| t.parts.contains(&Part::Id))
    {
        return Outcome::Rejected(Reject::TorrentId);
    }
    let category = category
        .filter(|c| !c.is_empty())
        .map(|c| cut(c, MAX_CATEGORY).to_owned());
    Outcome::Announce(Announce {
        definition: def.id.clone(),
        name: name.to_owned(),
        size: size.and_then(parse_size),
        category,
        torrent_id,
    })
}

fn valid_torrent_id(s: &str) -> bool {
    (1..=MAX_TORRENT_ID).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        // `.` and `..` would climb the URL's path.
        && !s.bytes().all(|b| b == b'.')
}

/// The longest prefix of `s` of at most `max` bytes that ends on a
/// character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A size as trackers write it: a number (`1.37`, `700`, `1,024.5`, or a
/// decimal comma such as `1,37`), optional spaces, and a unit: `B`, the
/// SI units `KB`, `MB`, `GB`, `TB` (powers of 1000) or the binary units
/// `KiB`, `MiB`, `GiB`, `TiB` (powers of 1024), in any case. A bare
/// number is bytes. A no-break space counts as a space. `None` for
/// anything else, or over [`MAX_SIZE`].
pub fn parse_size(s: &str) -> Option<u64> {
    const SPACES: [char; 2] = [' ', '\u{a0}'];
    let s = s.trim_matches(SPACES);
    if s.is_empty() || s.len() > 32 {
        return None;
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let unit = unit.trim_start_matches(SPACES);
    let mult: u64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => 1000,
        "mb" => 1000_u64.pow(2),
        "gb" => 1000_u64.pow(3),
        "tb" => 1000_u64.pow(4),
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        _ => return None,
    };
    // A comma with exactly three digits after it, and no dot after it,
    // groups thousands; any other comma is a decimal comma.
    let normalised: String = if num.contains(',') {
        // `0,500` is half a unit: nobody groups thousands after a zero.
        let decimal_comma = num.matches(',').count() == 1
            && !num.contains('.')
            && (num.starts_with("0,") || num.rsplit(',').next().is_some_and(|t| t.len() != 3));
        if decimal_comma {
            num.replace(',', ".")
        } else {
            let groups: Vec<&str> = num.split(',').collect();
            let first_ok =
                (1..=3).contains(&groups[0].len()) && groups[0].bytes().all(|b| b.is_ascii_digit());
            let rest_ok = groups[1..].iter().enumerate().all(|(i, g)| {
                let g = if i == groups.len() - 2 {
                    g.split('.').next().unwrap_or("")
                } else {
                    g
                };
                g.len() == 3
            });
            if !first_ok || !rest_ok {
                return None;
            }
            num.replace(',', "")
        }
    } else {
        num.to_owned()
    };
    let (int, frac) = match normalised.split_once('.') {
        Some((i, f)) => (i, f),
        None => (normalised.as_str(), ""),
    };
    if int.is_empty()
        || frac.contains('.')
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || int.len() > 16
    {
        return None;
    }
    // Exact integer arithmetic: whole part times the unit, plus the
    // fraction's first 18 digits scaled, rounded down (10^18 × 2^40 fits
    // a u128 with room to spare).
    let whole: u64 = int.parse().ok()?;
    let mut bytes = whole.checked_mul(mult)?;
    if !frac.is_empty() {
        let digits = &frac[..frac.len().min(18)];
        let f: u128 = digits.parse().ok()?;
        let scale = 10_u128.pow(u32::try_from(digits.len()).ok()?);
        let part = u64::try_from(f * u128::from(mult) / scale).ok()?;
        bytes = bytes.checked_add(part)?;
    }
    (bytes <= MAX_SIZE).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEF: &str = "\
# A fictional tracker, for tests.
lodestar-announce = 1
id = example
name = Example Tracker
network = irc.example.net
channel = #announce
announcer = Announcer
pattern = ^New: (?P<name>.+?) \\[(?P<category>[^\\]]+)\\] (?P<size>[0-9.,]+ ?[KMGT]i?B) id=(?P<id>[0-9]+)$
torrent-url = https://example.net/dl/{id}/{passkey}/{name}.torrent
";

    #[test]
    fn literal_secret_run_is_refused() {
        let t = DEF.replace("{passkey}", "0123456789abcdef0123456789abcdef");
        assert_eq!(
            Definition::parse(&t).unwrap_err(),
            DefinitionError::LiteralSecret
        );
        // Ordinary path words are fine.
        let t = DEF.replace("/dl/", "/download-torrent-file/");
        assert!(Definition::parse(&t).is_ok());
    }

    #[test]
    fn templates() {
        let ok = [
            "https://t.example/dl/{id}",
            "https://t.example:8443/dl.php?id={id}&key={passkey}",
            "https://t.example/{name}.torrent",
        ];
        for t in ok {
            assert!(parse_template(t).is_ok(), "{t}");
        }
        let bad = [
            "http://t.example/{id}",
            "https://{passkey}.example/x",
            "https://user:pw@t.example/{id}",
            "https://t.example/{nope}",
            "https://t.example/{id",
            "https://t.example/id}",
            "https://t.example/{passkey}/{passkey}",
            "https://t.example:99999x/",
            "https://t.example:0/x",
            "https://t.example:65536/x",
            "https://t.example:/x",
            "https://t.example/{id}/123e4567-e89b-12d3-a456-426614174000",
            "https://t example/",
            "https://-bad.example/",
            "ftp://t.example/",
        ];
        for t in bad {
            assert!(parse_template(t).is_err(), "{t}");
        }
    }

    #[test]
    fn sizes() {
        let cases = [
            ("1.37 GiB", Some(1_471_026_298)),
            ("700 MB", Some(700_000_000)),
            ("700MB", Some(700_000_000)),
            ("1,024.5 MiB", Some(1_074_266_112)),
            ("1,37 GB", Some(1_370_000_000)),
            ("1,234 MB", Some(1_234_000_000)),
            ("12345", Some(12_345)),
            ("0.5 KiB", Some(512)),
            ("2 tib", Some(2 << 40)),
            ("1024 TiB", Some(1 << 50)),
            ("1025 TiB", None),
            ("", None),
            ("GB", None),
            ("1.2.3 GB", None),
            ("1 PB", None),
            ("1,23,4 MB", None),
            ("99999999999999999 GB", None),
            ("1.9999999999999 B", Some(1)),
        ];
        for (s, want) in cases {
            assert_eq!(parse_size(s), want, "{s}");
        }
    }

    #[test]
    fn cut_keeps_char_boundaries() {
        assert_eq!(cut("aé", 2), "a");
        assert_eq!(cut("abc", 5), "abc");
    }
}
