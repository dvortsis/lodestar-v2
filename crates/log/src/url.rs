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
                Some((name, _)) if credential(name) => {
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

/// `text` with every URL in it scrubbed and its query string dropped,
/// and every run that looks like an info hash or a session secret (32 or
/// more hex digits, or 32 base32 characters) hidden. For lines the code
/// didn't type: the third-party crates' and the not-yet-converted ones.
pub fn scrub_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = [rest.find("://"), find_ci(rest, "magnet:")]
        .into_iter()
        .flatten()
        .min()
    {
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
        // A raw userinfo may hold `,`, `'` or `)`, so the userinfo comes
        // off the run up to a hard stop first; `url_char` trims only what
        // follows it.
        let mut from = i;
        if rest[i..].starts_with("://") {
            let wide = rest[i + 3..]
                .char_indices()
                .find(|(_, c)| hard_stop(*c))
                .map_or(rest.len(), |(j, _)| i + 3 + j);
            if let (Some(at), _) = userinfo(&rest[i + 3..wide]) {
                from = i + 3 + at + 1;
            }
        }
        let end = rest[from..]
            .char_indices()
            .find(|(_, c)| !url_char(*c))
            .map_or(rest.len(), |(j, _)| from + j);
        out.push_str(&hide_runs(&redact_secrets(&rest[..start])));
        out.push_str(&scrub_with(&rest[start..end], Form::Absolute, true));
        rest = &rest[end..];
        if end == start {
            // Nothing consumed (a bare "://"): step past it.
            let step = rest.char_indices().nth(1).map_or(rest.len(), |(j, _)| j);
            out.push_str(&rest[..step]);
            rest = &rest[step..];
        }
    }
    out.push_str(&hide_runs(&redact_secrets(rest)));
    out
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
            let j = skip_space(b, sep + sep_len);
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

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
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
