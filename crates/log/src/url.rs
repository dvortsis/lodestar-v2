//! Removing credentials from URLs, whatever the URL switch says (§12.1,
//! B12's "For the logging slice"): userinfo, the values of credential-like
//! query names (`apikey`, `token`, `passkey`…), passkey-like path segments
//! and fragments. [`scrub_text`] does the same to every URL in a free-text
//! message (the third-party crates' lines), dropping whole query strings,
//! and hides info-hash-like runs.

/// Which kind of URL a [`crate::Url`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// `scheme://host/path?query`.
    Absolute,
    /// A request target, `/path?query`.
    Request,
}

/// What a removed credential shows as.
pub const REDACTED: &str = "[redacted]";

/// Query names whose values are always removed. Matched without case;
/// any name that contains one of [`CREDENTIAL_PARTS`] is removed too.
pub const CREDENTIAL_NAMES: &[&str] = &[
    "apikey",
    "api_key",
    "api-key",
    "key",
    "token",
    "passkey",
    "pass",
    "password",
    "passwd",
    "pwd",
    "secret",
    "auth",
    "authkey",
    "sid",
    "session",
    "sig",
    "signature",
    "torrent_pass",
    "rsskey",
    "uid",
    "user",
    "username",
    "info_hash",
    "peer_id",
];

/// Parts of a query name that make it a credential (`x-api-key`,
/// `access_token`, `client_secret`…).
pub const CREDENTIAL_PARTS: &[&str] = &[
    "key",
    "token",
    "pass",
    "secret",
    "auth",
    "sess",
    "sig",
    "credential",
    "jwt",
    "pw",
    "cookie",
];

fn credential(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    CREDENTIAL_NAMES.contains(&lower.as_str()) || CREDENTIAL_PARTS.iter().any(|p| lower.contains(p))
}

/// A path segment that looks like a passkey or an info hash: 16 or more
/// characters of letters, digits, `-` and `_`, with at least one digit.
fn passkey_like(segment: &str) -> bool {
    segment.len() >= 16
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && segment.bytes().any(|b| b.is_ascii_digit())
}

/// `segment` with each passkey-like run between `.`, `:`, `,`, `=`, `+`
/// and `~` removed: `/dl/<passkey>.torrent` keeps `.torrent`.
fn redact_runs(segment: &str) -> String {
    if passkey_like(segment) {
        return REDACTED.to_string();
    }
    let mut out = String::with_capacity(segment.len());
    let mut run = String::new();
    for c in segment.chars() {
        if matches!(c, '.' | ':' | ',' | '=' | '+' | '~') {
            out.push_str(if passkey_like(&run) { REDACTED } else { &run });
            run.clear();
            out.push(c);
        } else {
            run.push(c);
        }
    }
    out.push_str(if passkey_like(&run) { REDACTED } else { &run });
    out
}

/// `url` without its credentials. Other query values stay.
pub fn scrub(url: &str, form: Form) -> String {
    scrub_with(url, form, false)
}

fn scrub_with(url: &str, form: Form, drop_query: bool) -> String {
    // An absolute-form request target (`GET http://u:p@host/ HTTP/1.1`,
    // as a proxy gets) has an authority, userinfo included.
    let form = match form {
        Form::Request
            if url
                .find("://")
                .is_some_and(|i| !url[..i].contains(['/', '?', '#'])) =>
        {
            Form::Absolute
        }
        f => f,
    };
    // A magnet link is all torrent: never shown.
    if url
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("magnet:"))
    {
        return format!("magnet:{REDACTED}");
    }
    let mut out = String::with_capacity(url.len());
    // The authority, userinfo included, comes off the raw URL first: a
    // password may hold `#` or `?` (`http://u:p#w@host/`).
    let rest = match form {
        Form::Absolute => match url.split_once("://") {
            Some((scheme, rest)) => {
                out.push_str(scheme);
                out.push_str("://");
                let (at, end) = userinfo(rest);
                let (authority, rest) = rest.split_at(end);
                let authority = match at {
                    // With a `/`, `?` or `#` before that `@`, what follows
                    // may not be the host (`https://u:pa/ss@host/x`, or
                    // `https://registry.example/@scope/pkg`): nothing more
                    // is shown rather than something misleading.
                    Some(at) if authority[..at].contains(['/', '?', '#']) => {
                        out.push_str(REDACTED);
                        return out;
                    }
                    Some(at) => {
                        out.push_str(REDACTED);
                        out.push('@');
                        &authority[at + 1..]
                    }
                    None => authority,
                };
                // A host with its query or fragment and no path.
                let host_end = authority.find(['?', '#']).unwrap_or(authority.len());
                out.push_str(&authority[..host_end]);
                if host_end < authority.len() {
                    // `host?query`: the query is the rest's start.
                    let mut joined = authority[host_end..].to_string();
                    joined.push_str(rest);
                    return finish(out, &joined, drop_query);
                }
                rest
            }
            None => url,
        },
        Form::Request => url,
    };
    finish(out, rest, drop_query)
}

/// `rest` (a path, query and fragment) appended to `out`, scrubbed.
fn finish(mut out: String, rest: &str, drop_query: bool) -> String {
    let rest = rest.split('#').next().unwrap_or(rest);
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    for (i, segment) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&redact_runs(segment));
    }
    if let Some(query) = query {
        if drop_query {
            return out;
        }
        out.push('?');
        // `&` and the older `;` both separate pairs; each keeps its own.
        let mut rest = query;
        while !rest.is_empty() {
            let end = rest.find(['&', ';']).unwrap_or(rest.len());
            let pair = &rest[..end];
            match pair.split_once('=') {
                // A credential's value, or a URL in a value (`?u=https://
                // u:p@h/`), whose userinfo would otherwise stay.
                Some((name, value)) if credential(name) || value.contains("://") => {
                    out.push_str(name);
                    out.push('=');
                    out.push_str(REDACTED);
                }
                Some((name, value)) => {
                    out.push_str(name);
                    out.push('=');
                    out.push_str(&redact_runs(value));
                }
                None => out.push_str(&redact_runs(pair)),
            }
            out.push_str(&rest[end..(end + 1).min(rest.len())]);
            rest = &rest[(end + 1).min(rest.len())..];
        }
    }
    out
}

/// Where an authority's userinfo ends and the authority itself ends, in
/// what follows `scheme://`. The userinfo runs to the last `@` before the
/// first `/` after an `@`, so a raw password holding `/` goes too; an `@`
/// in a path hides the host with it: never too little.
fn userinfo(rest: &str) -> (Option<usize>, usize) {
    match rest.find('@') {
        Some(first) => {
            let end = rest[first..].find('/').map_or(rest.len(), |j| first + j);
            (rest[..end].rfind('@'), end)
        }
        None => (None, rest.find('/').unwrap_or(rest.len())),
    }
}

/// Where a URL in free text can't go on, whatever comes before.
fn hard_stop(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '<' | '>' | '`')
}

fn url_char(c: char) -> bool {
    !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ']' | '}' | ',')
}

/// How far past `scheme://` free text is searched for the userinfo's `@`
/// (B-log1b): a raw password may hold a space, `"`, `<`, `>` or a
/// backtick, so the `@` may lie past a hard stop. The bound keeps
/// [`scrub_text`] linear.
pub const USERINFO_LOOKAHEAD: usize = 512;

/// The next match of `needle` at or after `from`, remembered so each
/// byte is searched once however many times it's asked.
struct Next {
    needle: &'static str,
    ci: bool,
    at: Option<usize>,
    searched: usize,
}

impl Next {
    fn new(needle: &'static str, ci: bool) -> Next {
        Next {
            needle,
            ci,
            at: None,
            searched: 0,
        }
    }

    fn from(&mut self, text: &str, from: usize) -> Option<usize> {
        if self.at.is_some_and(|at| at >= from) {
            return self.at;
        }
        if self.at.is_none() && self.searched > from {
            // Searched past `from` already and found nothing.
            return None;
        }
        let hay = &text.as_bytes()[from..];
        let found = match self.ci {
            true => find_ci_bytes(hay, self.needle.as_bytes()),
            false => text[from..].find(self.needle),
        };
        self.at = found.map(|j| from + j);
        self.searched = text.len();
        self.at
    }
}

/// `text` with every URL in it scrubbed and its query string dropped,
/// and every run that looks like an info hash or a session secret (32 or
/// more hex digits, or 32 base32 characters) hidden. For lines the code
/// didn't type: the third-party crates' and the not-yet-converted ones.
///
/// One forward pass: each needle's next match is remembered, and the
/// userinfo's look-ahead is bounded, so a line of `a://` or `magnet:`
/// repeats costs time in proportion to its length.
pub fn scrub_text(text: &str) -> String {
    let debug = scrub_debug_forms(text);
    let text: &str = &debug;
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    let mut scheme = Next::new("://", false);
    let mut magnet = Next::new("magnet:", true);
    while pos < text.len() {
        let Some(i) = [scheme.from(text, pos), magnet.from(text, pos)]
            .into_iter()
            .flatten()
            .min()
        else {
            break;
        };
        let rest = &text[pos..];
        let i = i - pos;
        // Back up over the scheme.
        let start = if rest[i..].starts_with("://") {
            rest[..i]
                .char_indices()
                .rev()
                .take_while(|(_, c)| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                .last()
                .map_or(i, |(j, _)| j)
        } else {
            i
        };
        let mut from = i;
        if rest[i..].starts_with("://") {
            let next = scheme
                .from(text, pos + i + 3)
                .map_or(rest.len(), |n| n - pos)
                - (i + 3);
            let at = userinfo_in_text(&rest[i + 3..], next);
            if let Some(at) = at {
                from = i + 3 + at + 1;
            }
        }
        let end = rest[from..]
            .char_indices()
            .find(|(_, c)| !url_char(*c))
            .map_or(rest.len(), |(j, _)| from + j);
        out.push_str(&hide_runs(&redact_secrets(&rest[..start])));
        out.push_str(&scrub_with(&rest[start..end], Form::Absolute, true));
        pos += end;
        if end == start {
            // Nothing consumed (a bare "://"): step past it.
            let step = text[pos..]
                .char_indices()
                .nth(1)
                .map_or(text.len() - pos, |(j, _)| j);
            out.push_str(&text[pos..pos + step]);
            pos += step;
        }
    }
    out.push_str(&hide_runs(&redact_secrets(&text[pos..])));
    out
}

/// Where the userinfo's `@` is in what follows `scheme://` in free text,
/// if there is one. `next` is where the next `://` starts in `after` (or
/// its length): no search goes past it, so each byte is searched for one
/// URL only. Two rules, the wider wins ("never too little"):
/// - up to a hard stop, the URL rules' [`userinfo`] (a raw `,`, `'` or
///   `)` stays inside the password, however long, and a `/` before the
///   `@` makes the URL ambiguous, so it's hidden whole);
/// - within [`USERINFO_LOOKAHEAD`] bytes on the same line, the last `@`
///   before any `/`, so a raw space, `"`, `<`, `>` or backtick in a
///   password goes too. `see https://a.example and me@x` loses `a.example
///   and me` with it: over-redaction is accepted.
fn userinfo_in_text(after: &str, next: usize) -> Option<usize> {
    let after = &after[..next];
    let hard = after.find(hard_stop).unwrap_or(after.len());
    let narrow = userinfo(&after[..hard]).0;
    let mut bound = after.len().min(USERINFO_LOOKAHEAD);
    while !after.is_char_boundary(bound) {
        bound -= 1;
    }
    // One scan: the window ends at the first `/` or line break.
    let window = &after[..bound];
    let window = &window[..window.find(['/', '\n', '\r']).unwrap_or(window.len())];
    let wide = window.rfind('@');
    narrow.max(wide)
}

/// The `Debug` forms of `url::Url` (and `reqwest::Url`, the same type)
/// carry their parts as fields with no `://` (`Url { scheme: "https",
/// username: "u", password: Some("p"), …, path: "/ann/<passkey>", query:
/// Some("passkey=k"), fragment: Some("f") }`): the password, username,
/// query and fragment go, and a path keeps only what the URL rules keep.
fn scrub_debug_forms(text: &str) -> std::borrow::Cow<'_, str> {
    // The field, whether its value is an `Option`, whether it's the path.
    const FIELDS: &[(&str, bool, bool)] = &[
        ("password: ", true, false),
        ("username: ", false, false),
        ("query: ", true, false),
        ("fragment: ", true, false),
        ("path: ", false, true),
    ];
    if !text.contains(": Some(") && !text.contains(": \"") {
        return text.into();
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        let at_word = i == 0 || !name_char(b[i - 1]);
        let field = at_word
            .then(|| {
                FIELDS
                    .iter()
                    .find(|(f, ..)| b[i..].starts_with(f.as_bytes()))
            })
            .flatten();
        let Some(&(f, option, path)) = field else {
            i += 1;
            continue;
        };
        // `"…"`, or `Some("…")`, pretty-printed (`{:#?}`) or not.
        let mut from = i + f.len();
        if option {
            if !b[from..].starts_with(b"Some(") {
                i = from;
                continue;
            }
            from = skip_space(b, from + 5);
        }
        if b.get(from) != Some(&b'"') {
            i = from;
            continue;
        }
        let from = from + 1;
        // To the closing quote, past escaped ones (`Debug` escapes them).
        let mut to = from;
        while to < b.len() && b[to] != b'"' {
            to += if b[to] == b'\\' { 2 } else { 1 };
        }
        let to = to.min(b.len());
        out.push_str(&text[copied..from]);
        match path {
            true => {
                let mut scrubbed = String::new();
                for (k, segment) in text[from..to].split('/').enumerate() {
                    if k > 0 {
                        scrubbed.push('/');
                    }
                    scrubbed.push_str(&redact_runs(segment));
                }
                out.push_str(&scrubbed);
            }
            false => out.push_str(REDACTED),
        }
        copied = to;
        i = to.max(from);
    }
    out.push_str(&text[copied..]);
    out.into()
}

fn token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'~' | b'+' | b'/' | b'=')
}

fn name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

/// Secrets in free text with no URL around them: Lodestar's own keys and
/// tokens (`lsk_…`, `lsf_…`), `Bearer …`, and the value after a
/// credential-like name and `=` or `:` (`X-Api-Key: …`, `/feed?apikey=…`,
/// `token = …`), quoted or not (`password: "…"`, `{"x-api-key": "…"}`,
/// `token='…'`).
fn redact_secrets(text: &str) -> String {
    let b = text.as_bytes();
    let stop = |from: usize| {
        (from..b.len())
            .find(|&k| {
                b[k].is_ascii_whitespace()
                    || matches!(b[k], b'&' | b';' | b',' | b'"' | b'\'' | b')')
            })
            .unwrap_or(b.len())
    };
    // Byte ranges to replace, in order; every bound is next to ASCII, so
    // each is a char boundary.
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let at_word = i == 0 || !name_char(b[i - 1]);
        if !at_word || !name_char(b[i]) {
            i += 1;
            continue;
        }
        let name_end = (i..b.len()).find(|&j| !name_char(b[j])).unwrap_or(b.len());
        if b[i..].starts_with(b"lsk_") || b[i..].starts_with(b"lsf_") {
            cuts.push((i, name_end));
            i = name_end;
            continue;
        }
        let word = &b[i..name_end];
        if word.eq_ignore_ascii_case(b"bearer")
            && b.get(name_end).is_some_and(|c| c.is_ascii_whitespace())
        {
            let start = skip_space(b, name_end);
            let end = (start..b.len())
                .find(|&j| !token_char(b[j]))
                .unwrap_or(b.len());
            if end > start {
                cuts.push((start, end));
            }
            i = end.max(start);
            continue;
        }
        // The name's closing quote (`"x-api-key": …`), then the separator,
        // with any space around it.
        let mut sep = name_end;
        if matches!(b.get(sep), Some(b'"' | b'\'')) {
            sep += 1;
        }
        sep = skip_space(b, sep);
        // `=`, `:`, or a URL-encoded `%3D` / `%3A`.
        let sep_len = match b.get(sep) {
            Some(b'=' | b':') => 1,
            Some(b'%')
                if b.get(sep + 1) == Some(&b'3')
                    && matches!(b.get(sep + 2), Some(b'D' | b'd' | b'A' | b'a')) =>
            {
                3
            }
            _ => 0,
        };
        if sep_len > 0 && credential(&text[i..name_end]) {
            let mut j = skip_space(b, sep + sep_len);
            // A `Debug` form's `Some("…")`: the quoted value inside.
            if b[j..].starts_with(b"Some(")
                && matches!(b.get(skip_space(b, j + 5)), Some(b'"' | b'\''))
            {
                j = skip_space(b, j + 5);
            }
            let (j, end) = match b.get(j) {
                // A quoted value runs to its closing quote, past escaped
                // ones (`"a\"b"`).
                Some(&q @ (b'"' | b'\'')) => {
                    let from = j + 1;
                    let mut to = from;
                    while to < b.len() && b[to] != q {
                        to += if b[to] == b'\\' { 2 } else { 1 };
                    }
                    (from, to.min(b.len()))
                }
                _ => {
                    let mut end = stop(j);
                    // `Authorization: <scheme> <credentials>`, whatever
                    // the scheme (`Bearer`, `Basic`, `token`): both go.
                    let scheme = &b[j..end];
                    let authorization = word.len() >= 13
                        && word[word.len() - 13..].eq_ignore_ascii_case(b"authorization");
                    if (authorization
                        || scheme.eq_ignore_ascii_case(b"bearer")
                        || scheme.eq_ignore_ascii_case(b"basic"))
                        && !scheme.is_empty()
                        && scheme
                            .iter()
                            .all(|c| c.is_ascii_alphanumeric() || *c == b'-')
                        && b.get(end).is_some_and(|c| c.is_ascii_whitespace())
                    {
                        let next = skip_space(b, end);
                        if next < b.len() {
                            end = stop(next);
                        }
                    }
                    (j, end)
                }
            };
            if end > j {
                cuts.push((j, end));
            }
            i = end.max(sep + sep_len);
            continue;
        }
        i = name_end;
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (from, to) in cuts {
        out.push_str(&text[copied..from]);
        out.push_str(REDACTED);
        copied = to;
    }
    out.push_str(&text[copied..]);
    out
}

/// The first index at or after `from` that isn't ASCII whitespace.
fn skip_space(b: &[u8], from: usize) -> usize {
    (from..b.len())
        .find(|&k| !b[k].is_ascii_whitespace())
        .unwrap_or(b.len())
}

fn find_ci_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

/// Hides runs of 32+ hex digits and of exactly 32 base32 characters.
fn hide_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        let hex = word.len() >= 32 && word.bytes().all(|b| b.is_ascii_hexdigit());
        let base32 = word.len() == 32
            && word
                .bytes()
                .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b));
        if hex || base32 {
            out.push_str(crate::HIDDEN);
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_removed_and_the_rest_kept() {
        assert_eq!(
            scrub(
                "https://idx.example/api?t=search&apikey=abc&cat=2000",
                Form::Absolute
            ),
            "https://idx.example/api?t=search&apikey=[redacted]&cat=2000"
        );
        assert_eq!(
            scrub("/api/v1/feeds/x.rss?token=lsf_abc&limit=5", Form::Request),
            "/api/v1/feeds/x.rss?token=[redacted]&limit=5"
        );
        assert_eq!(
            scrub(
                "http://admin:hunter2@192.168.1.5:8080/api/v2",
                Form::Absolute
            ),
            "http://[redacted]@192.168.1.5:8080/api/v2"
        );
        assert_eq!(
            scrub(
                "https://t.example/0f1e2d3c4b5a69788796a5b4c3d2e1f0/announce",
                Form::Absolute
            ),
            "https://t.example/[redacted]/announce"
        );
        assert_eq!(
            scrub(
                "https://t.example/announce?passkey=xyz#frag",
                Form::Absolute
            ),
            "https://t.example/announce?passkey=[redacted]"
        );
        for name in [
            "X-Api-Key",
            "access_token",
            "client_secret",
            "Torrent_Pass",
            "SID",
        ] {
            let s = scrub(&format!("https://h/p?{name}=v1"), Form::Absolute);
            assert_eq!(s, format!("https://h/p?{name}=[redacted]"));
        }
        assert_eq!(
            scrub("magnet:?xt=urn:btih:abc", Form::Absolute),
            "magnet:[redacted]"
        );
        // Words in a path stay.
        assert_eq!(
            scrub("https://h/announcements/latest", Form::Absolute),
            "https://h/announcements/latest"
        );
        // A raw password holding `@` goes whole; one holding `/`, `?` or
        // `#` takes the rest with it, since what follows may not be the host.
        assert_eq!(
            scrub("https://u:pa/ss@host/x", Form::Absolute),
            "https://[redacted]"
        );
        assert_eq!(
            scrub("http://u:p#w@host/", Form::Absolute),
            "http://[redacted]"
        );
        assert_eq!(
            scrub("https://registry.npmjs.org/@scope/pkg", Form::Absolute),
            "https://[redacted]"
        );
        assert_eq!(
            scrub("https://h.example/s?q=a@b", Form::Absolute),
            "https://[redacted]"
        );
        assert_eq!(
            scrub("https://u:p@a@s@host:81/x?y=1", Form::Absolute),
            "https://[redacted]@host:81/x?y=1"
        );
        assert_eq!(
            scrub("https://u:pa/ss@host", Form::Absolute),
            "https://[redacted]"
        );
        // No `@`: the host stays.
        assert_eq!(
            scrub("https://host/a/b", Form::Absolute),
            "https://host/a/b"
        );
    }

    #[test]
    fn free_text_loses_queries_and_hash_like_runs() {
        let hash = "c9e15763f722f23e98a29decdfae341b98d53056";
        let text = format!(
            "error sending request for url (https://u:p@t.example/abcdef0123456789abcdef/announce?passkey=s3cr3t&info_hash=%AB): timed out; got {hash} from \"magnet:?xt=urn:btih:{hash}\""
        );
        let s = scrub_text(&text);
        for secret in ["u:p", "s3cr3t", "abcdef0123456789abcdef", hash, "%AB"] {
            assert!(!s.contains(secret), "{secret} in {s}");
        }
        assert!(s.starts_with("error sending request for url (https://[redacted]@t.example/[redacted]/announce): timed out"), "{s}");
        assert!(s.contains("magnet:[redacted]"), "{s}");
        for (text, secret) in [
            (
                "got key lsk_AbCdEfGh_0123456789abcdefghijklmnopqrstuvwxyzABCDEFG for x",
                "lsk_AbCd",
            ),
            ("token lsf_AbCdEfGh_secret-part ok", "lsf_AbCd"),
            ("Authorization: Bearer abc.def-ghi rejected", "abc.def"),
            ("header X-Api-Key: s3cr3tvalue was wrong", "s3cr3tvalue"),
            ("GET /api/v1/feeds/x.rss?token=t0ken123&limit=5", "t0ken123"),
            ("apikey=k3y passkey: pk", "k3y"),
        ] {
            let s = scrub_text(text);
            assert!(!s.contains(secret), "{secret} in {s}");
        }
        assert!(scrub_text("passkey: pk").ends_with("[redacted]"));
        // Quoted values, space around the separator, any space after Bearer.
        for (text, secret) in [
            (r#"password: "hunter 2" rejected"#, "hunter 2"),
            (r#"api_key: "k3y""#, "k3y"),
            (r#"{"x-api-key": "k3y", "n": 1}"#, "k3y"),
            ("token='t0k' then", "t0k"),
            ("token = t0k then", "t0k"),
            ("password =\th2", "h2"),
            ("Authorization: Bearer\t abc.def", "abc.def"),
            ("Bearer  abc.def", "abc.def"),
            ("Authorization:   Bearer    abc.def", "abc.def"),
            ("credentials: c3d", "c3d"),
            ("aws_credential=c3d", "c3d"),
            ("Authorization: token ghp_s3cr3t rejected", "ghp_s3cr3t"),
            ("authorization: Digest d1g3st", "d1g3st"),
            (r#"password: "a\"b s3cr3t" next"#, "s3cr3t"),
            ("jwt=eyJ0", "eyJ0"),
            ("pw: hunter2", "hunter2"),
            ("cookie: sid=abc123", "abc123"),
            ("set token%3Ds3cr3t%26x", "s3cr3t"),
            ("api_key%3as3cr3t", "s3cr3t"),
        ] {
            let s = scrub_text(text);
            assert!(!s.contains(secret), "{secret} in {s}");
        }
        assert_eq!(
            scrub_text(r#"{"x-api-key": "k3y", "n": 1}"#),
            r#"{"x-api-key": "[redacted]", "n": 1}"#
        );
        assert_eq!(scrub_text("token = t0k then"), "token = [redacted] then");
        assert_eq!(
            scrub_text("Authorization: token ghp_x rejected"),
            "Authorization: [redacted] rejected"
        );
        // A raw userinfo holding `,`, `'` or `)` isn't cut short.
        for (text, secret) in [
            ("err for url (https://u:p,w@host/x)", "p,w"),
            ("proxy socks5://user:pa'ss@h:1080 failed", "pa'ss"),
            ("got https://u:p)w@h/ back", "p)w"),
        ] {
            let s = scrub_text(text);
            assert!(
                !s.contains(secret) && s.contains("[redacted]@h"),
                "{secret} in {s}"
            );
        }
        assert_eq!(
            scrub_text("err for url (https://u:p,w@host/x)"),
            "err for url (https://[redacted]@host/x)"
        );
        // Without userinfo, `url_char` still ends a URL.
        assert_eq!(
            scrub_text("see (https://h.example/x), then"),
            "see (https://h.example/x), then"
        );
        assert_eq!(scrub_text("limit=5 and size: 7"), "limit=5 and size: 7");
        let base32 = "ZHQVOY7XELZD5GFCTXW6X2RUDOMNKMCW";
        assert_eq!(scrub_text(&format!("x {base32} y")), "x [hidden] y");
        // Ordinary text is untouched, a stray "://" included.
        assert_eq!(scrub_text("a :// b 12345"), "a :// b 12345");
        assert_eq!(
            scrub_text("first run: open http://localhost:8191/ and enter setup token ABCD-1234"),
            "first run: open http://localhost:8191/ and enter setup token ABCD-1234"
        );
    }

    #[test]
    fn a_raw_hard_stop_in_a_password_goes_whole() {
        for c in [' ', '"', '<', '>', '`'] {
            let url = format!("https://u:pa{c}ss@host/x");
            let s = scrub_text(&format!("err for {url} then"));
            assert!(!s.contains("ss@") && !s.contains("pa"), "{c:?}: {s}");
            assert!(s.contains("[redacted]@host/x"), "{c:?}: {s}");
            for form in [Form::Absolute, Form::Request] {
                let s = scrub(&url, form);
                assert_eq!(s, "https://[redacted]@host/x", "{c:?} {form:?}");
            }
            // A target that isn't absolute keeps the request rules, its
            // query's credentials included.
            let s = scrub("x?token=S3cr3t&u=http://h", Form::Request);
            assert!(!s.contains("S3cr3t"), "{s}");
            // A request target that holds a URL in its query.
            let s = scrub(&format!("/go?to={url}&n=1"), Form::Request);
            assert!(!s.contains("pa") && s.ends_with("to=[redacted]&n=1"), "{s}");
        }
        // The `@` may sit past several hard stops, within the look-ahead.
        let s = scrub_text("https://u:a b\"c<d>e`f@host/x");
        assert_eq!(s, "https://[redacted]@host/x");
        // A password holding an `@` and a space: the last `@` wins.
        let s = scrub_text("x https://u:a@b c@host/p y");
        assert_eq!(s, "x https://[redacted]@host/p y");
        // Past the look-ahead, or on the next line, an `@` isn't userinfo.
        let far = format!("https://h.example {} me@x", "w".repeat(USERINFO_LOOKAHEAD));
        assert!(scrub_text(&far).starts_with("https://h.example "));
        assert_eq!(
            scrub_text("https://h.example\nme@x"),
            "https://h.example\nme@x"
        );
        // A long raw password holding a URL-ending `)` still goes whole,
        // past the look-ahead (the narrow rule runs to a hard stop).
        let long = format!(
            "x https://u:{}){}@host/x y",
            "p".repeat(600),
            "q".repeat(10)
        );
        assert_eq!(scrub_text(&long), "x https://[redacted]@host/x y");
        // The typed wrappers, as a line renders them.
        for c in [' ', '"', '<', '>', '`'] {
            for (url, form) in [
                (format!("https://u:pa{c}ss@host/x"), Form::Absolute),
                (format!("http://u:pa{c}ss@host/x?apikey=k"), Form::Request),
            ] {
                let v = match form {
                    Form::Absolute => crate::Url::new(&url),
                    Form::Request => crate::Url::request(&url),
                };
                let shown = crate::IntoValue::<crate::kind::Url>::into_value(v)
                    .render(crate::Mask::NONE.with(crate::Kind::Url), false, None)
                    .to_string();
                assert!(
                    !shown.contains("pa") && !shown.contains("=k"),
                    "{c:?}: {shown}"
                );
                assert!(shown.contains("[redacted]@host/x"), "{c:?}: {shown}");
            }
        }
        // Over-redaction of an ambiguous line is accepted.
        assert_eq!(
            scrub_text("see https://a.example and me@x.org"),
            "see https://[redacted]@x.org"
        );
        // A `/` before the `@` ends the wide rule: the path stays.
        assert_eq!(
            scrub_text("see https://a.example/p and me@x"),
            "see https://a.example/p and me@x"
        );
    }

    #[test]
    fn debug_forms_of_url_lose_their_secrets() {
        let passkey = "0123456789abcdef0123";
        let parsed = ::url::Url::parse(&format!(
            "https://us3r:p4ss@h.example/ann/{passkey}?passkey=k3y#fr4g"
        ))
        .unwrap();
        let debug = format!("{parsed:?}");
        assert!(debug.contains("p4ss") && !debug.contains("://"), "{debug}");
        for text in [
            format!("request to {debug} failed"),
            format!("{:#?}", parsed),
            format!("x {:?}", Some(&parsed)),
        ] {
            let s = scrub_text(&text);
            for secret in ["us3r", "p4ss", passkey, "k3y", "fr4g"] {
                assert!(!s.contains(secret), "{secret} in {s}");
            }
            assert!(s.contains("h.example"), "{s}");
        }
        let s = scrub_text(&debug);
        assert!(s.contains(r#"path: "/ann/[redacted]""#), "{s}");
        assert!(s.contains(r#"query: Some("[redacted]")"#), "{s}");
        assert!(s.contains(r#"password: Some("[redacted]")"#), "{s}");
        // A path without a passkey-like run stays.
        let plain = format!("{:?}", ::url::Url::parse("https://h/announce").unwrap());
        assert!(scrub_text(&plain).contains(r#"path: "/announce""#));
    }

    /// The time for `n` bytes of `unit` repeated: the least of a few runs.
    fn time_for(unit: &str, n: usize) -> std::time::Duration {
        let text = unit.repeat(n / unit.len());
        (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                std::hint::black_box(scrub_text(std::hint::black_box(&text)));
                t.elapsed()
            })
            .min()
            .unwrap()
    }

    #[test]
    fn scrub_text_is_linear() {
        // Linear is about 16x from 64 KiB to 1 MiB, quadratic about 256x:
        // the ratio is what's checked, never the time alone.
        for unit in [
            "a://",
            "magnet:",
            "token=",
            "@",
            "://u:p@",
            "a:// ",
            "x magnet:? ",
        ] {
            let small = time_for(unit, 64 << 10).max(std::time::Duration::from_micros(50));
            let large = time_for(unit, 1 << 20);
            let ratio = large.as_secs_f64() / small.as_secs_f64();
            assert!(
                ratio <= 30.0,
                "{unit:?}: {large:?} for 1 MiB, {small:?} for 64 KiB ({ratio:.1}x)"
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn planted_secrets_never_get_through(
            before in "[ -~]{0,40}",
            after in "[ -~]{0,40}",
            scheme in "(https?|udp|ftp)",
            secret in "[A-Za-z0-9]{6,20}",
        ) {
            let secret = format!("Zq{secret}9");
            for text in [
                format!("{before}{scheme}://u:{secret}@host.example/x{after}"),
                format!("{before}{scheme}://host.example/a?passkey={secret}{after}"),
                format!("{before}{scheme}://host.example/a?x=1&passkey={secret}&y{after}"),
            ] {
                let s = scrub_text(&text);
                proptest::prop_assert!(!s.contains(&secret), "{} -> {}", text, s);
            }
        }

        #[test]
        fn scrub_text_never_panics(text in "\\PC{0,200}") {
            let _ = scrub_text(&text);
            let _ = scrub(&text, Form::Absolute);
            let _ = scrub(&text, Form::Request);
        }
    }

    #[test]
    fn scrubbing_never_panics_on_odd_input() {
        for s in [
            "",
            "?",
            "#",
            "://",
            "a://",
            "magnet:",
            "ß://ü?ä=ö#",
            "http://[::1]:80/?a",
            "&&==??",
            "token=\"",
            "token: '",
            "key =",
            "Bearer ",
            "a://@",
            "https://@/@",
        ] {
            let _ = scrub(s, Form::Absolute);
            let _ = scrub(s, Form::Request);
            let _ = scrub_text(s);
        }
    }
}
