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
pub const CREDENTIAL_PARTS: &[&str] = &["key", "token", "pass", "secret", "auth", "sess", "sig"];

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
    let url = url.split('#').next().unwrap_or(url);
    let (head, query) = match url.split_once('?') {
        Some((h, q)) => (h, Some(q)),
        None => (url, None),
    };
    let mut out = String::with_capacity(url.len());
    let path = match form {
        Form::Absolute => match head.split_once("://") {
            Some((scheme, rest)) => {
                out.push_str(scheme);
                out.push_str("://");
                let (authority, path) = match rest.find('/') {
                    Some(i) => (&rest[..i], &rest[i..]),
                    None => (rest, ""),
                };
                match authority.rsplit_once('@') {
                    Some((_, host)) => {
                        out.push_str(REDACTED);
                        out.push('@');
                        out.push_str(host);
                    }
                    None => out.push_str(authority),
                }
                path
            }
            None => head,
        },
        Form::Request => head,
    };
    for (i, segment) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        if passkey_like(segment) {
            out.push_str(REDACTED);
        } else {
            out.push_str(segment);
        }
    }
    if let Some(query) = query {
        if drop_query {
            return out;
        }
        out.push('?');
        for (i, pair) in query.split('&').enumerate() {
            if i > 0 {
                out.push('&');
            }
            match pair.split_once('=') {
                Some((name, _)) if credential(name) => {
                    out.push_str(name);
                    out.push('=');
                    out.push_str(REDACTED);
                }
                Some((name, value)) if passkey_like(value) => {
                    out.push_str(name);
                    out.push('=');
                    out.push_str(REDACTED);
                }
                _ if passkey_like(pair) => out.push_str(REDACTED),
                _ => out.push_str(pair),
            }
        }
    }
    out
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
        let end = rest[i..]
            .char_indices()
            .find(|(_, c)| !url_char(*c))
            .map_or(rest.len(), |(j, _)| i + j);
        out.push_str(&hide_runs(&rest[..start]));
        out.push_str(&scrub_with(&rest[start..end], Form::Absolute, true));
        rest = &rest[end..];
        if end == start {
            // Nothing consumed (a bare "://"): step past it.
            let step = rest.char_indices().nth(1).map_or(rest.len(), |(j, _)| j);
            out.push_str(&rest[..step]);
            rest = &rest[step..];
        }
    }
    out.push_str(&hide_runs(rest));
    out
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
        ] {
            let _ = scrub(s, Form::Absolute);
            let _ = scrub(s, Form::Request);
            let _ = scrub_text(s);
        }
    }
}
