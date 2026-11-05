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

/// `segment` with each passkey-like run (letters, digits, `-` and `_`,
/// between any other characters) removed: `/dl/<passkey>.torrent` keeps
/// `.torrent`, and `[<passkey>&` keeps `[` and `&`.
fn redact_runs(segment: &str) -> String {
    if passkey_like(segment) {
        return REDACTED.to_string();
    }
    let mut out = String::with_capacity(segment.len());
    let mut run = String::new();
    for c in segment.chars() {
        if !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
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
    // A network-path reference (`//u:p@host/x`) has an authority too.
    if form == Form::Request && url.starts_with("//") {
        let scrubbed = scrub_with(&format!("x:{url}"), Form::Absolute, drop_query);
        return scrubbed["x:".len()..].to_string();
    }
    // A magnet link is all torrent: never shown, percent-encoded too.
    if starts_magnet(url) {
        return format!("magnet:{REDACTED}");
    }
    // A magnet glued to the authority (`http://[::1]magnet%3A…`, B-log1):
    // the whole authority goes with it, since cutting there would drop what
    // makes the rules below hide it (`https://<secret>,magnet:` lost its
    // `:`; review). One in the path or query is `finish`'s.
    let in_authority = |m: usize| {
        form == Form::Absolute
            && url[..m]
                .find("://")
                .is_some_and(|s| !url[s + 3..m].contains(['/', '?', '#']))
    };
    if let Some(m) = magnets().from(url, 1).filter(|&m| in_authority(m)) {
        let s = url[..m].find("://").unwrap_or(0);
        return format!("{}://{REDACTED}", redact_runs(&redact_secrets(&url[..s])));
    }
    let mut out = String::with_capacity(url.len());
    // The authority, userinfo included, comes off the raw URL first: a
    // password may hold `#` or `?` (`http://u:p#w@host/`).
    let rest = match form {
        Form::Absolute => match url.split_once("://") {
            Some((scheme, rest)) => {
                // Free text's scheme may be the tail of what came before
                // (`/dl/<passkey>.torrent://`): a real one is never
                // passkey-like, nor a credential's value (`token:<t>://`).
                out.push_str(&redact_runs(&redact_secrets(scheme)));
                out.push_str("://");
                let (at, end) = userinfo(rest);
                let (authority, rest) = rest.split_at(end);
                // An `@` after the authority may end a raw password that
                // holds `@` and `/` (`http://u:@<secret>/@h/`, `…/<secret>?
                // @h/`): the host may be part of it, so nothing more is
                // shown.
                if rest.contains('@') {
                    out.push_str(REDACTED);
                    return out;
                }
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
                // No host holds a `%` outside an IPv6 literal: it's an
                // encoded password (`u%3A<secret>%40h`), hidden whole. An
                // IPv6 literal is one `[…]` and a port number at most.
                let h = &authority[..host_end];
                let odd = match h.strip_prefix('[') {
                    Some(inner) => inner.split_once(']').is_none_or(|(literal, port)| {
                        !literal
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.' | b'%'))
                            || !(port.is_empty()
                                || port
                                    .strip_prefix(':')
                                    .is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit())))
                    }),
                    // Nor any character a host can't hold, nor a port that's
                    // no number (`@token:https:\/\/…`, a credential's value
                    // read as a host; review).
                    None => marks_userinfo(h, false),
                };
                if odd {
                    out.push_str(REDACTED);
                    return out;
                }
                // A `:` with no port number after it, and no `@` found,
                // is a password whose `@` wasn't found (`u:<secret>/x`).
                if at.is_none() {
                    let h = &authority[..host_end];
                    if !h.starts_with('[')
                        && h.rsplit_once(':').is_some_and(|(_, port)| {
                            port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit())
                        })
                    {
                        out.push_str(REDACTED);
                        return out;
                    }
                }
                // Free text's host may be a path segment of what came
                // before (`/https://<passkey>/announce`, `@passkey=<k>`): a
                // real host label is never passkey-like or `name=value`.
                out.push_str(&redact_runs(&redact_secrets(&authority[..host_end])));
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

/// A query value holding a URL, raw or encoded in any mix (`https://…`,
/// `https%3A%2F%2F…`, `https:%2F%2F…`, `https:\/\/…`), whose userinfo
/// would otherwise stay, or a magnet link (B-log1).
fn holds_url(value: &str) -> bool {
    value.contains("://")
        || [b"%3a%2f%2f".as_slice(), b"%3a//"]
            .iter()
            .any(|n| find_ci_bytes(value.as_bytes(), n).is_some())
        || normalize_separators(value, false).contains("://")
        || holds_magnet(value)
}

/// `rest` (a path, query and fragment) appended to `out`, scrubbed.
fn finish(mut out: String, rest: &str, drop_query: bool) -> String {
    let rest = rest.split('#').next().unwrap_or(rest);
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    let whole_path = path;
    // A magnet in the path (B-log1): it and all after it go, with the run
    // it's glued to (`/<passkey>magnet:`), which the passkey rules would
    // otherwise see only in part.
    let (path, query) = match magnets().from(path, 0) {
        Some(m) => (&path[..glued_run_start(path, m)], None),
        None => (path, query),
    };
    let magnet = path.len() < whole_path.len();
    // An `@` in the path, after the authority came off, may be a URL's
    // userinfo that an earlier `@` hid (`http://a@h/r/http://u:p@h2/`, or
    // `://@http://u:p@h/` in free text): the path goes whole, its query
    // too, as the authority rules hide an `@` in a path: never too little.
    if whole_path.contains('@') {
        if path.starts_with('/') {
            out.push('/');
        }
        out.push_str(REDACTED);
        return out;
    }
    let (scrubbed, cut) = scrub_path(path);
    out.push_str(&scrubbed);
    if cut.is_some() {
        return out;
    }
    if magnet {
        out.push_str("magnet:");
        out.push_str(REDACTED);
        return out;
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
            // A magnet in the query (`?uri=magnet:?xt=…&dn=…`, B-log1):
            // its own pairs follow it, so all from it on goes.
            if holds_magnet(pair) {
                if let Some((name, _)) = pair.split_once('=').filter(|(n, _)| !holds_magnet(n)) {
                    out.push_str(name);
                    out.push('=');
                }
                out.push_str(REDACTED);
                return out;
            }
            match pair.split_once('=') {
                // A credential's value, or a URL in a value (`?u=https://
                // u:p@h/`), whose userinfo would otherwise stay.
                Some((name, value)) if credential(name) || holds_url(value) => {
                    out.push_str(name);
                    out.push('=');
                    out.push_str(REDACTED);
                }
                Some((name, value)) => {
                    out.push_str(name);
                    out.push('=');
                    out.push_str(&redact_runs(&redact_secrets(value)));
                }
                // A URL in a pair with no `=` (`?a&https://u:p@h/`).
                None if holds_url(pair) => out.push_str(REDACTED),
                // A credential glued on with `:` or `%3D` (`?passkey%3D<k>`).
                None => out.push_str(&redact_runs(&redact_secrets(pair))),
            }
            out.push_str(&rest[end..(end + 1).min(rest.len())]);
            rest = &rest[(end + 1).min(rest.len())..];
        }
    }
    out
}

/// A path scrubbed segment by segment, `name=value` credentials included
/// (`/dl/passkey=<k>`), and whether all after some point went: a
/// credential's value that runs over a `/` (`/token:https:\/\/h\/<s>`)
/// takes the rest of the path with it.
fn scrub_path(path: &str) -> (String, Option<usize>) {
    let cut = secret_cuts(path)
        .into_iter()
        .find(|&(a, b)| path[a..b].contains('/'))
        .map(|(a, _)| a);
    let keep = &path[..cut.unwrap_or(path.len())];
    let mut out = String::with_capacity(path.len());
    for (i, segment) in keep.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&redact_runs(&redact_secrets(segment)));
    }
    if cut.is_some() {
        out.push_str(REDACTED);
    }
    (out, cut)
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
/// byte is searched once however many times it's asked. The answer is
/// right for any `from`, in any order: one before the last search's
/// start searches again (X-log-url-redact-2; a span that ends past a
/// magnet asks again from the magnet).
struct Next {
    needle: &'static str,
    ci: bool,
    /// The last search's start, and the first match at or after it.
    base: Option<usize>,
    at: Option<usize>,
}

impl Next {
    fn new(needle: &'static str, ci: bool) -> Next {
        Next {
            needle,
            ci,
            base: None,
            at: None,
        }
    }

    fn from(&mut self, text: &str, from: usize) -> Option<usize> {
        if self.base.is_some_and(|base| base <= from) {
            match self.at {
                Some(at) if at >= from => return Some(at),
                // Nothing at or after the base, so nothing after `from`.
                None => return None,
                Some(_) => {}
            }
        }
        // The needles are ASCII, so a byte search finds only whole
        // characters, from any `from`.
        let hay = text.as_bytes().get(from..).unwrap_or_default();
        let needle = self.needle.as_bytes();
        let found = match self.ci {
            true => find_ci_bytes(hay, needle),
            false => hay.windows(needle.len()).position(|w| w == needle),
        };
        self.base = Some(from);
        self.at = found.map(|j| from + j);
        self.at
    }
}

/// The first match of any of several needles at or after `from`.
struct AnyOf<const N: usize>([Next; N]);

impl<const N: usize> AnyOf<N> {
    fn from(&mut self, text: &str, from: usize) -> Option<usize> {
        self.0.iter_mut().filter_map(|n| n.from(text, from)).min()
    }
}

/// The ways a magnet link starts: raw, or percent-encoded once or twice
/// (`magnet%3A%3Fxt=…`).
fn magnets() -> AnyOf<3> {
    AnyOf([
        Next::new("magnet:", true),
        Next::new("magnet%3a", true),
        Next::new("magnet%253a", true),
    ])
}

/// A raw magnet link only, as `main` found them (see [`scrub_text`]'s
/// first pass).
fn raw_magnets() -> AnyOf<3> {
    AnyOf([
        Next::new("magnet:", true),
        Next::new("magnet:", true),
        Next::new("magnet:", true),
    ])
}

/// Where the first percent-encoded magnet (`magnet%3A`, `magnet%253A`)
/// starts in `b`, in one scan.
fn find_encoded_magnet(b: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(k) = find_ci_bytes(&b[from..], b"magnet%").map(|k| from + k) {
        let rest = &b[k + 7..];
        if [b"3a".as_slice(), b"253a"].iter().any(|e| {
            rest.get(..e.len())
                .is_some_and(|r| r.eq_ignore_ascii_case(e))
        }) {
            return Some(k);
        }
        from = k + 1;
    }
    None
}

/// Whether `s` starts with a magnet link, raw or percent-encoded.
fn starts_magnet(s: &str) -> bool {
    ["magnet:", "magnet%3a", "magnet%253a"]
        .iter()
        .any(|m| s.get(..m.len()).is_some_and(|p| p.eq_ignore_ascii_case(m)))
}

/// Where the run of letters, digits, `-` and `_` that ends at `k` in `s`
/// starts.
fn glued_run_start(s: &str, k: usize) -> usize {
    k - s[..k].bytes().rev().take_while(|&b| name_char(b)).count()
}

/// Whether `s` holds a magnet link anywhere.
fn holds_magnet(s: &str) -> bool {
    magnets().from(s, 0).is_some()
}

/// Where each `@` is, so the userinfo's look-ahead finds the last one in
/// its window in constant time, however many `://` it crosses.
struct Ats(Vec<u32>);

impl Ats {
    /// `last[k]` is one past the last `@` before `k`, or 0.
    fn new(text: &str) -> Ats {
        if !text.contains('@') || text.len() >= u32::MAX as usize {
            return Ats(Vec::new());
        }
        let mut last = Vec::with_capacity(text.len() + 1);
        let mut seen = 0u32;
        last.push(0);
        for (k, b) in text.bytes().enumerate() {
            if b == b'@' {
                seen = k as u32 + 1;
            }
            last.push(seen);
        }
        Ats(last)
    }

    /// The last `@` in `from..to`.
    fn last_in(&self, text: &str, from: usize, to: usize) -> Option<usize> {
        if self.0.is_empty() {
            // Built empty: no `@` at all, or text too long to index (then
            // search directly).
            return text
                .get(from..to)
                .and_then(|w| w.rfind('@'))
                .map(|j| from + j);
        }
        let seen = self.0[to.min(self.0.len() - 1)] as usize;
        (seen > from).then(|| seen - 1)
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
    let first = scrub_pass(&scrub_debug_forms(text), false);
    // What `main` didn't know, URL forms without a raw `://` (`http:%2F%2F`,
    // `http:/`, `//u:p@h`, written with one here) and percent-encoded
    // magnets, is scrubbed in a second pass over the first's output
    // (runner's review). Found in the first pass, a rewritten `://`
    // bounded the URL before it (`ftp://<s>") "h#http%3A%2F%2F@`), and an
    // encoded magnet's span split a value the credential rules hid whole.
    // The second pass can only hide more, since what the first hid is
    // already gone.
    let separated = normalize_separators(&first, true);
    let encoded = [b"magnet%3a".as_slice(), b"magnet%253a"]
        .iter()
        .any(|m| find_ci_bytes(separated.as_bytes(), m).is_some());
    match (separated, encoded) {
        (std::borrow::Cow::Borrowed(_), false) => first,
        (separated, _) => scrub_pass(&separated, true),
    }
}

/// One pass of [`scrub_text`]'s URL rules over `text`; `encoded` is
/// whether percent-encoded magnets start a span too.
fn scrub_pass(text: &str, encoded: bool) -> String {
    let ats = Ats::new(text);
    let whole_cuts = secret_cuts(text);
    let whole = Whole {
        cuts: &whole_cuts,
        at: 0,
    };
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    let mut scheme = Next::new("://", false);
    let mut magnet = match encoded {
        true => magnets(),
        false => raw_magnets(),
    };
    // The magnets that extend a URL's span (below).
    let mut in_span = magnets();
    // A credential's name that ended the last span (see below).
    let mut carry: Option<String> = None;
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
        let url = rest[i..].starts_with("://");
        // Back up over the scheme.
        let start = match url {
            true => rest[..i]
                .char_indices()
                .rev()
                .take_while(|&(_, c)| scheme_char(c))
                .last()
                .map_or(i, |(j, _)| j),
            false => i,
        };
        let mut from = i;
        // Where the next `://` starts in `rest`, if there's one after this
        // URL's own userinfo.
        let mut nested = None;
        // Whether that look-ahead ran over a later `://`.
        let mut crossed = false;
        if url {
            nested = scheme.from(text, pos + i + 3).map(|n| n - pos);
            let next = nested.unwrap_or(rest.len()) - (i + 3);
            if let Some(at) = userinfo_in_text(text, pos + i + 3, next, &ats) {
                from = i + 3 + at + 1;
                // A look-ahead that ran over later `://`s: they're in this
                // URL's userinfo, and the next one is after it.
                if nested.is_some_and(|n| n < from) {
                    nested = scheme.from(text, pos + from).map(|n| n - pos);
                    crossed = true;
                }
            }
        }
        // A credential's name in the userinfo whose value runs on past its
        // `@` (`://\nAuthorization: x@<secret>`): what follows the `@` is
        // that value, not a host, so the whole span goes (below).
        // A value can't stop at the `@` (it's no stop character), so one
        // that reaches the userinfo's end runs on past it. The userinfo
        // regions never overlap, so this stays linear.
        let value_in_userinfo = from > i + 3
            && secret_cuts(&rest[i + 3..from])
                .iter()
                .any(|&(_, b)| i + 3 + b >= from);
        // An IPv6 literal host (`http://[::1]:6969/…`): its `]` doesn't end
        // the URL.
        let mut scan = from;
        let host = if from == i { i + 3 } else { from };
        if url {
            let past = past_ipv6(rest, host);
            if past != host {
                // With a later `://`, an extra `]` after the literal still
                // ends the scan, as on `main`: past it, a `?` would choose a
                // different arm below (`s3://[::1]]?'<s>ws://`; review).
                scan = match nested {
                    Some(_) => {
                        past - rest[..past]
                            .bytes()
                            .rev()
                            .take_while(|&b| b == b']')
                            .count()
                            + 1
                    }
                    None => past,
                };
            }
        }
        let url_end = |from: usize| {
            rest[from..]
                .char_indices()
                .find(|(_, c)| !url_char(*c))
                .map_or(rest.len(), |(j, _)| from + j)
        };
        // A later `://` before this URL's query or fragment starts a URL of
        // its own, with its own userinfo search (`://@http://u:p@h/`): this
        // one ends where that one's scheme starts. One in the query or
        // fragment stays in this URL, which loses both. With no userinfo
        // found before it, that `://` may be inside a raw password
        // (`http://u:pa/ss://x@h/`): all between the two goes, and the
        // next URL starts at the `://` itself. The first scan stops at
        // that `://`, so a line of them stays linear.
        let end = match nested {
            // A magnet link (B-log1: never shown); see `magnet_span`.
            _ if !url => magnet_span(text, pos, i, &mut scheme, &ats),
            None => url_end(scan),
            Some(nested) => match url_end_before(rest, scan, nested) {
                // Ended by a hard stop, or with userinfo or a query before
                // it: as any URL.
                Some(end)
                    if from != i
                        || rest[host..end].contains(['?', '#'])
                        || rest[end..nested].contains(hard_stop) =>
                {
                    end
                }
                // In the query or fragment: this URL runs on over each later
                // `://`, and each still gets its own userinfo search, so a
                // raw space in its password can't end the span early.
                None if rest[host..nested].contains(['?', '#']) => {
                    let mut cur = nested;
                    // The furthest any of those userinfo searches reached.
                    let mut reach = 0;
                    loop {
                        let next = scheme.from(text, pos + cur + 3).map(|n| n - pos);
                        let bound = next.unwrap_or(rest.len()) - (cur + 3);
                        let scan = userinfo_in_text(text, pos + cur + 3, bound, &ats)
                            .map_or(cur + 3, |at| cur + 3 + at + 1);
                        // An IPv6 host's `]` doesn't end it here either.
                        reach = reach.max(past_ipv6(rest, scan));
                        match next {
                            Some(n) => match url_end_before(rest, reach, n) {
                                Some(end) => break end,
                                None => cur = n,
                            },
                            None => break url_end(reach),
                        }
                    }
                }
                // No userinfo before it, and nothing but URL characters or a
                // soft end (`)`, `]`, `,`, `'`, `}`) up to it: the next `://`
                // may be inside a raw password (`http://u:pa]://x@h/`). So
                // may it when no `/` lies between the userinfo and it: it's
                // in this URL's authority (`ws://u:?@<secret>://@h/`), and
                // what was read as userinfo or host is never read again as
                // a scheme.
                // And when what would be the next URL's scheme isn't a known
                // one: it may be a password's tail (`u:@/<secret>://@h/`).
                Some(_) | None
                    if from == i
                        || !rest[from..nested].contains('/')
                        || !known_scheme(&rest[scheme_start(rest, scan, nested)..nested]) =>
                {
                    out.push_str(&lead(rest, start, i, carry.take(), whole.at(pos)));
                    out.push_str("://");
                    out.push_str(REDACTED);
                    // A magnet in what's hidden gets its own span (B-log1),
                    // so its tracker URL is never read as the next URL. Only
                    // when no hard stop lies between it and the next `://`:
                    // then its span surely reaches that far, and a password
                    // after it can't print (`u:magnet:?dn=X <secret>@h://`).
                    // The last magnet before it, not the first (runner's
                    // review: `#magnet:?dn=A B magnet:?xt=a&dn=x@redis://`).
                    let mut last = None;
                    let mut m = magnet.from(text, pos + i + 3).map(|m| m - pos);
                    while let Some(at) = m.filter(|&at| at < nested) {
                        last = Some(at);
                        m = magnet.from(text, pos + at + 1).map(|m| m - pos);
                    }
                    let stop = last
                        .filter(|&m| !rest[m..nested].contains(hard_stop))
                        .unwrap_or(nested);
                    // After a look-ahead that ran over a `://`, the next URL
                    // goes too, to its first hard stop: what the look-ahead
                    // took may be its password (`://#://@=://<secret>`).
                    let stop = match crossed && stop == nested {
                        true => {
                            nested
                                + rest[nested..]
                                    .find(hard_stop)
                                    .unwrap_or(rest.len() - nested)
                        }
                        false => stop,
                    };
                    carry = Some(rest[start..stop].to_string());
                    pos += stop;
                    continue;
                }
                Some(_) | None => {
                    let scheme_start = scheme_start(rest, scan, nested);
                    // After `=` (or `:`, `%3D`) it's a value
                    // (`@passkey=<k>://`): it stays with this URL, whose
                    // host and path rules see the name, and the next URL
                    // has no scheme.
                    let before = &rest[..scheme_start];
                    let value = before.ends_with(['=', ':'])
                        || before
                            .get(before.len().saturating_sub(3)..)
                            .is_some_and(|s| s.eq_ignore_ascii_case("%3d"));
                    match value {
                        true => nested,
                        false => scheme_start.max(scan),
                    }
                }
            },
        };
        // A magnet in this URL's span (`/add?uri=magnet:?xt=…&dn=Name,2020`)
        // ends only where the magnet does (B-log1), never at this URL's
        // soft end.
        // After a look-ahead that ran over a later `://`, the span runs at
        // least to the first hard stop after its userinfo: the URLs it ran
        // over may hold the password, and what's glued after them may be
        // its rest (`x://#éx://(udp://@h%3A//Some(ws://<s>`; review).
        let end = match url && crossed {
            true => end.max(from + rest[from..].find(hard_stop).unwrap_or(rest.len() - from)),
            false => end,
        };
        // Each magnet in it counts, not only the first: one after the
        // userinfo may run further than one inside it (review). One inside
        // another's span is that one's, so each byte is scanned once.
        let mut end = end;
        // Encoded ones count in either pass: a span only grows here, and
        // the second pass would no longer see one the first hid in a URL
        // (`://magnet%3A?…&dn=A'<s>`).
        let mut m = in_span.from(text, pos + i).map(|m| m - pos);
        while let Some(at) = m.filter(|&at| url && at < end) {
            let span = magnet_span(text, pos, at, &mut scheme, &ats);
            end = end.max(span);
            m = in_span.from(text, pos + span.max(at + 1)).map(|m| m - pos);
        }
        out.push_str(&lead(rest, start, i, carry.take(), whole.at(pos)));
        match value_in_userinfo {
            true => {
                out.push_str("://");
                out.push_str(REDACTED);
            }
            false => out.push_str(&scrub_with(&rest[i..end.max(i)], Form::Absolute, true)),
        }
        // A span that ends on a credential's name (`&tr=Authorization:`,
        // `/token=`, `@Bearer`) leaves its value after the span: the text
        // after it is read with that name before it, so the plain-text
        // rules still hide the value (review). The span itself is scrubbed
        // whole as before, since the name may be glued to a passkey
        // (`/<passkey>Authorization:`).
        carry = Some(rest[start..end.max(start)].to_string());
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
    out.push_str(&plain(&text[pos..], carry.take(), whole.at(pos)));
    out
}

/// `text` scrubbed as plain text. `carry` is the span just before it
/// (review): a credential's name in it, or a value that started in it
/// (`…&tr=Authorization: <s>`, `https://h/&Basic @key\n:<s>`), still has
/// its value hidden in `text`. Every cut `text` has on its own is kept, so
/// nothing shows that it hid alone.
fn plain(text: &str, carry: Option<String>, whole: Whole<'_>) -> String {
    let own = secret_cuts(text);
    let mut cuts = own.clone();
    // The whole text's cuts that cross this region (runner's review): a
    // value that started before it (`token: "a magnet%3A… <s>"`, whose
    // quote opened before the magnet's span) still goes here.
    cuts.extend(whole.within(text.len()));
    if let Some(carry) = carry {
        // Never glue a name across the seam: `x` and `user=` stay apart.
        let glue = match (carry.bytes().last(), text.bytes().next()) {
            (Some(a), Some(b)) if name_char(a) && name_char(b) => " ",
            _ => "",
        };
        let seam = carry.len() + glue.len();
        let joined = format!("{carry}{glue}{text}");
        cuts.extend(
            secret_cuts(&joined)
                .into_iter()
                .filter(|&(_, b)| b > seam)
                .map(|(a, b)| (a.max(seam) - seam, b - seam)),
        );
    }
    cuts.sort_unstable();
    // A cut that holds a percent-encoded magnet's start runs to that
    // magnet's end: the first pass hides it as a value, and the second,
    // which knows encoded magnets, would no longer see it
    // (`pW:magnet%3A?xt=…&dn=<s>`).
    // Each magnet's end is searched once: a cut that starts before the
    // furthest end found takes it (runner's review: `token=magnet%3A&dn=`
    // repeated was quadratic).
    let mut reach = 0;
    for (a, b) in &mut cuts {
        if *a < reach {
            *b = (*b).max(reach);
            continue;
        }
        if let Some(m) = find_encoded_magnet(&text.as_bytes()[*a..*b]) {
            reach = magnet_end(text, *a + m);
            *b = (*b).max(reach);
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (from, to) in cuts {
        if to <= copied {
            continue;
        }
        let from = from.max(copied);
        if from > copied || copied == 0 && from == 0 {
            out.push_str(&text[copied..from]);
            out.push_str(REDACTED);
        }
        copied = to;
    }
    out.push_str(&text[copied..]);
    hide_runs(&out)
}

/// The cuts [`secret_cuts`] finds in the whole text, seen from a region
/// that starts at byte `at`.
#[derive(Clone, Copy)]
struct Whole<'a> {
    cuts: &'a [(usize, usize)],
    at: usize,
}

impl<'a> Whole<'a> {
    fn at(self, at: usize) -> Whole<'a> {
        Whole { at, ..self }
    }

    /// The cuts that cross `at..at + len`, clipped to it and relative to
    /// `at`. The cuts are in order and apart, so a binary search finds
    /// the first.
    fn within(self, len: usize) -> impl Iterator<Item = (usize, usize)> + 'a {
        let (from, to) = (self.at, self.at + len);
        let first = self.cuts.partition_point(|&(_, b)| b <= from);
        self.cuts[first..]
            .iter()
            .take_while(move |&&(a, _)| a < to)
            .map(move |&(a, b)| (a.max(from) - from, b.min(to) - from))
    }
}

/// Where the magnet link at `k` in `text[pos..]` ends. It ends only at a
/// hard stop (see [`magnet_end`]), never at a soft end or an IPv6 `]`
/// (`dn=Name,2020`, `tr=udp://[::1]:6969/`). A URL glued to it stays in
/// it, and the magnet runs on to that URL's end, past its userinfo: each
/// `://` it runs over gets its userinfo searched.
fn magnet_span(text: &str, pos: usize, k: usize, scheme: &mut Next, ats: &Ats) -> usize {
    let rest = &text[pos..];
    let mut end = magnet_end(rest, k);
    let Some(mut n) = scheme
        .from(text, pos + k)
        .map(|n| n - pos)
        .filter(|&n| n < end)
    else {
        return end;
    };
    loop {
        let next = scheme.from(text, pos + n + 3).map(|m| m - pos);
        let bound = next.unwrap_or(rest.len()) - (n + 3);
        if let Some(at) = userinfo_in_text(text, pos + n + 3, bound, ats) {
            let after = n + 3 + at + 1;
            if after >= end {
                end = magnet_end(rest, after);
            }
        }
        match next {
            Some(m) if m < end => n = m,
            _ => return end,
        }
    }
}

/// Magnet parameters (BEP 9 and the common extras), to tell a word that
/// carries a magnet on past a raw space (`dn=a b&tr=…`).
const MAGNET_KEYS: &[&str] = &[
    "xt", "dn", "tr", "xl", "ws", "as", "xs", "kt", "mt", "so", "x.pe",
];

/// Where a magnet link, or the rest of one from `from`, ends in `rest`:
/// at a hard stop, past `\`-escaped characters (a `Debug` string's `\"`).
/// A word after it that carries a magnet parameter (`a&tr=…`) carries the
/// magnet on. A display name or keywords (`dn`, `kt`) may hold any number
/// of raw spaces and line breaks, so when the stop falls in one of those,
/// any later such word carries it on, and with none the magnet runs to the
/// text's end. Never too little.
fn magnet_end(rest: &str, from: usize) -> usize {
    let mut k = from;
    // Where the parameter the scan is in starts.
    let mut param = from;
    loop {
        while let Some(c) = rest[k..].chars().next() {
            if c == '\\' {
                k += 1;
                k += rest[k..].chars().next().map_or(0, char::len_utf8);
                continue;
            }
            if hard_stop(c) {
                break;
            }
            if matches!(c, '?' | '&' | ';') {
                param = k + 1;
            } else if c == '%' && ["%26", "%3f"].iter().any(|e| starts_ci(&rest[k..], e)) {
                param = k + 3;
            }
            k += c.len_utf8();
        }
        if k >= rest.len() {
            return rest.len();
        }
        let value = &rest[param..k];
        let name_end = value
            .find('=')
            .into_iter()
            .chain(find_ci_bytes(value.as_bytes(), b"%3d"))
            .min()
            .unwrap_or(value.len());
        let free = ["dn", "kt"]
            .iter()
            .any(|n| value[..name_end].eq_ignore_ascii_case(n));
        // The next word that carries a parameter: only the very next one,
        // or, in a name or keywords (which may hold any number of words),
        // any later one, with the rest of the text going if there's none.
        let mut at = k;
        let next = loop {
            let word = at
                + rest[at..]
                    .find(|c: char| !hard_stop(c))
                    .unwrap_or(rest.len() - at);
            if word >= rest.len() {
                break None;
            }
            let word_end = word + rest[word..].find(hard_stop).unwrap_or(rest.len() - word);
            if carries_magnet_param(&rest[word..word_end]) {
                break Some(word);
            }
            if !free {
                break None;
            }
            at = word_end;
        };
        match (next, free) {
            (Some(word), _) => {
                k = word;
                param = word;
            }
            (None, true) => return rest.len(),
            (None, false) => return k,
        }
    }
}

/// Whether `word` holds `&<key>=` (or `%26<key>%3D`) for a magnet key.
fn carries_magnet_param(word: &str) -> bool {
    let b = word.as_bytes();
    (0..b.len()).any(|j| {
        let after = match b[j] {
            b'&' => j + 1,
            b'%' if starts_ci(&word[j..], "%26") => j + 3,
            _ => return false,
        };
        MAGNET_KEYS.iter().any(|key| {
            word.get(after..after + key.len())
                .is_some_and(|k| k.eq_ignore_ascii_case(key))
                && (word[after + key.len()..].starts_with('=')
                    || starts_ci(&word[after + key.len()..], "%3d"))
        })
    })
}

/// Whether `s` starts with `prefix`, ignoring ASCII case.
fn starts_ci(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// `text` with URL forms that lack a raw `://` written with one, so
/// [`scrub_text`] finds them (X-log-url-redact-2):
/// - a `:` and two `/`, any of them percent-encoded once or twice or the
///   `/` JSON-escaped (`http:%2F%2F`, `http%3A%2F%2F`, `http%3A//`,
///   `http:\/\/`, `http:%2F/`);
/// - a known scheme and a single `/` (`http:/u:p@h`), `file` aside;
/// - a network-path reference at a word's start (`//u:p@h`) with an `@`
///   after it on the same line.
///
/// The URL then shows with a plain `://`, whatever it was written with.
/// With `keep_values`, a separator inside a credential's value (what
/// [`redact_secrets`] would hide) is left as it is, so the value stays
/// one and is hidden whole.
fn normalize_separators(text: &str, keep_values: bool) -> std::borrow::Cow<'_, str> {
    let b = text.as_bytes();
    if !b.contains(&b'/')
        && find_ci_bytes(b, b"%2f").is_none()
        && find_ci_bytes(b, b"%252f").is_none()
    {
        return text.into();
    }
    // A credential's value is never split (security review): one holding
    // an encoded `://` (`secret=https%3A%2F%2Fh/<secret>`, `token%3A%2F%2F
    // <secret>`) stays one value, which the credential rules hide whole.
    let cuts = match keep_values {
        true => secret_cuts(text),
        false => Vec::new(),
    };
    let mut cut = 0;
    let mut out = String::new();
    let mut copied = 0;
    let mut at = Next::new("@", false);
    let mut line = AnyOf([Next::new("\n", false), Next::new("\r", false)]);
    let mut k = 0;
    // Whether `k` is in a word that holds a raw `://` already: a form inside
    // a URL is that URL's rules' (`https://h/{//u@<s>` keeps its path `@`).
    let mut in_url = false;
    while k < b.len() {
        while cuts.get(cut).is_some_and(|&(_, to)| to < k) {
            cut += 1;
        }
        if b[k].is_ascii_whitespace() {
            in_url = false;
        } else if b[k..].starts_with(b"://") {
            in_url = true;
        }
        // (what's replaced, what it becomes)
        let found = match b[k] {
            _ if in_url => None,
            b':' | b'%' => separator_len(text, k).map(|len| (len, "://")),
            b'/' if b.get(k + 1) == Some(&b'/') => {
                let word_start = k == 0
                    || b[k - 1].is_ascii_whitespace()
                    || matches!(
                        b[k - 1],
                        b'"' | b'\'' | b'(' | b'<' | b'[' | b'{' | b'=' | b','
                    );
                let host_next = b
                    .get(k + 2)
                    .is_some_and(|&c| c != b'/' && !c.is_ascii_whitespace());
                // On the same line, or a `:` right after it (`//u:\n<s>@h`).
                let colon = text[k + 2..]
                    .find(|c: char| matches!(c, ':' | '/') || c.is_whitespace())
                    .is_some_and(|j| text[k + 2 + j..].starts_with(':'));
                // And not where a credential's value starts before that `@`
                // (`//x Authorization: u@<secret>`): it would become a host.
                let at_after = word_start
                    && host_next
                    && at.from(text, k + 2).is_some_and(|a| {
                        a - k <= USERINFO_LOOKAHEAD
                            && (colon || line.from(text, k).is_none_or(|l| l > a))
                            && cuts.get(cut).is_none_or(|&(from, _)| from > a)
                    });
                at_after.then_some((2, "://"))
            }
            _ => None,
        };
        // Not where it touches a cut: inside one, or ending at one's start.
        let found = found.filter(|&(len, _)| cuts.get(cut).is_none_or(|&(from, _)| from > k + len));
        match found {
            Some((len, with)) => {
                out.push_str(&text[copied..k]);
                out.push_str(with);
                k += len;
                copied = k;
                in_url = true;
            }
            None => k += 1,
        }
    }
    if copied == 0 {
        return text.into();
    }
    out.push_str(&text[copied..]);
    out.into()
}

/// How long the URL separator written another way at `k` in `text` is
/// (see [`normalize_separators`]), if there is one there.
fn separator_len(text: &str, k: usize) -> Option<usize> {
    // A scheme always comes before it (`[:/%2F` is no URL's).
    if !text[..k].ends_with(scheme_char) {
        return None;
    }
    let s = &text[k..];
    let colon = [":", "%3a", "%253a"]
        .iter()
        .find(|c| starts_ci(s, c))
        .map(|c| c.len())?;
    let slash = |from: usize| {
        ["/", "%2f", "%252f", "\\/"]
            .iter()
            .find(|c| starts_ci(&s[from..], c))
            .map(|c| c.len())
    };
    match slash(colon) {
        Some(one) => match slash(colon + one) {
            // A raw `://` is left as it is.
            Some(two) if !(colon == 1 && one == 1 && two == 1) => Some(colon + one + two),
            Some(_) => None,
            // A known scheme and a single raw `/` (`http:/u:p@h`), even
            // glued to what's before it (`%2Fftp:/`).
            None if colon == 1
                && one == 1
                && k > 0
                && text.as_bytes()[k - 1].is_ascii_alphanumeric() =>
            {
                KNOWN_SCHEMES
                    .iter()
                    .chain(OTHER_SCHEMES)
                    .filter(|&&known| known != "file")
                    .any(|known| {
                        k >= known.len()
                            && text.is_char_boundary(k - known.len())
                            && text[k - known.len()..k].eq_ignore_ascii_case(known)
                    })
                    .then_some(2)
            }
            None => None,
        },
        None => None,
    }
}

/// The text before a URL in `rest`, its scheme (`start..i`) included,
/// scrubbed as plain text: a credential's value glued to `://`
/// (`passkey=<k>://`) would otherwise be taken for a scheme. A scheme
/// that's left whole goes through the passkey rules too.
fn lead(rest: &str, start: usize, i: usize, carry: Option<String>, whole: Whole<'_>) -> String {
    let lead = plain(&rest[..i], carry, whole);
    let scheme = &rest[start..i];
    match lead.strip_suffix(scheme) {
        Some(before) if !scheme.is_empty() => format!("{before}{}", redact_runs(scheme)),
        _ => lead,
    }
}

/// Past an IPv6 literal host at `host` in `rest` (`[::1]`, hex digits,
/// `:`, `.` and a zone's `%` up to the `]`), or `host` if there's none:
/// its `]` doesn't end the URL.
fn past_ipv6(rest: &str, host: usize) -> usize {
    if !rest[host..].starts_with('[') {
        return host;
    }
    let mut bound = rest.len().min(host + 64);
    while !rest.is_char_boundary(bound) {
        bound -= 1;
    }
    let literal = &rest[host + 1..bound];
    match literal
        .find(|c: char| !(c.is_ascii_hexdigit() || matches!(c, ':' | '.' | '%')))
        .filter(|&k| literal[k..].starts_with(']'))
    {
        // A malformed literal's extra `]` (`[::1]]:80/<passkey>`) doesn't
        // end the URL either.
        Some(close) => {
            let past = host + 1 + close + 1;
            past + rest[past..].bytes().take_while(|&b| b == b']').count()
        }
        None => host,
    }
}

/// Schemes a URL nested in another's path is split off at; any other
/// run before a nested `://` may be the tail of a raw password, so it's
/// hidden with what comes before it.
const KNOWN_SCHEMES: &[&str] = &[
    "http", "https", "ws", "wss", "udp", "tcp", "ftp", "ftps", "sftp", "socks4", "socks4a",
    "socks5", "socks5h", "file",
];

/// More schemes that take credentials, for a URL written with one `/`
/// (`ssh:/u:p@h`); never split at, unlike [`KNOWN_SCHEMES`].
const OTHER_SCHEMES: &[&str] = &[
    "ssh",
    "git",
    "svn",
    "smb",
    "s3",
    "ldap",
    "ldaps",
    "redis",
    "rediss",
    "mongodb",
    "postgres",
    "postgresql",
    "mysql",
    "amqp",
    "amqps",
    "mqtt",
    "mqtts",
    "rtsp",
    "irc",
    "ircs",
    "imap",
    "imaps",
    "pop3",
    "smtp",
    "smtps",
    "nntp",
    "telnet",
];

fn known_scheme(scheme: &str) -> bool {
    KNOWN_SCHEMES.iter().any(|k| k.eq_ignore_ascii_case(scheme))
}

/// Where the scheme before the `://` at `nested` starts, backing no
/// further than `scan`.
fn scheme_start(rest: &str, scan: usize, nested: usize) -> usize {
    rest[..nested]
        .char_indices()
        .rev()
        .take_while(|&(j, c)| j >= scan && scheme_char(c))
        .last()
        .map_or(nested, |(j, _)| j)
}

/// A scheme's characters, and `_`: a passkey glued to a scheme
/// (`/dl/<pass_key>http://`) is backed over whole, so the passkey rules
/// see all of it.
fn scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.' | '_')
}

/// Where a URL starting at `scan` in `rest` ends, if it ends before
/// `limit` (the next `://`); `None` if it runs on to `limit`.
fn url_end_before(rest: &str, scan: usize, limit: usize) -> Option<usize> {
    if scan >= limit {
        return None;
    }
    rest[scan..limit]
        .char_indices()
        .find(|(_, c)| !url_char(*c))
        .map(|(j, _)| scan + j)
}

/// Where the userinfo's `@` is in what follows `scheme://` in free text,
/// if there is one: `text[base..]` is what follows, and `next` is where
/// the next `://` starts in it (or its length). Two rules, the wider wins
/// ("never too little"):
/// - up to a hard stop and the next `://`, the URL rules' [`userinfo`] (a
///   raw `,`, `'` or `)` stays inside the password, however long, and a
///   `/` before the `@` makes the URL ambiguous, so it's hidden whole);
/// - within [`USERINFO_LOOKAHEAD`] bytes, the last `@`, so a raw space,
///   `"`, `<`, `>`, backtick or `/` in a password goes too.
///   - With a sign of userinfo (see [`marks_userinfo`]), the window runs
///     over line breaks and later `://`s, since a raw password may hold
///     either (`u:\n<secret>@h`, `u:@<secret>"://@h`). [`Ats`] finds that
///     `@` without a scan, so the text stays linear however many `://`
///     the window crosses.
///   - With none, it ends at a line break, the next `://`, and the later
///     of the first `/` and the first whitespace. So `see https://a.example
///     and me@x` loses `a.example and me` with it (over-redaction is
///     accepted), while `see https://a.example/p and me@x` keeps its path.
fn userinfo_in_text(text: &str, base: usize, next: usize, ats: &Ats) -> Option<usize> {
    let after = &text[base..base + next];
    let hard = after.find(hard_stop).unwrap_or(after.len());
    let narrow = after[..hard].rfind('@');
    let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..authority_end.min(hard)];
    // A `://` with no scheme before it is no URL's own: it may be inside
    // a raw password (`u:://<secret>@h`), as may a `://` the authority
    // runs straight into (`u://<secret>@h`, whose `:` is the password's).
    let no_scheme = base < 3 || !text[..base - 3].ends_with(scheme_char);
    let runs_into_next = authority.len() == after.len() && base + next < text.len();
    // Only `file` has an empty authority (`ftp:///\n<secret>@h`).
    let file = base >= 7
        && text[..base - 3].is_char_boundary(base - 7)
        && text[base - 7..base - 3].eq_ignore_ascii_case("file");
    let marked = narrow.is_some()
        || no_scheme
        || runs_into_next
        || (authority.is_empty() && !file)
        || marks_userinfo(authority, hard < authority_end);
    let wide = match marked {
        true => {
            let to = text.len().min(base + USERINFO_LOOKAHEAD);
            ats.last_in(text, base, to).map(|at| at - base)
        }
        false => {
            let mut bound = after.len().min(USERINFO_LOOKAHEAD);
            while !after.is_char_boundary(bound) {
                bound -= 1;
            }
            let window = &after[..bound];
            // With no sign of userinfo the window ends at a line break, and
            // at whichever comes later of the first `/` (as on `main`, which
            // crossed spaces: `x://a b@h`) and the first other whitespace
            // (which crosses `/`: `http://<s>/"@h/`). So `see https://h/p and
            // me@x` keeps its path (runner, §2.26).
            let line = window.find(['\n', '\r']).unwrap_or(window.len());
            let slash = window[..line].find('/').unwrap_or(line);
            let space = window[..line].find(char::is_whitespace).unwrap_or(line);
            window[..slash.max(space)].rfind('@')
        }
    };
    narrow.max(wide)
}

/// Whether an authority with no `@` (what follows `scheme://` up to the
/// first `/`, `?`, `#` or hard stop) still shows userinfo: a `:` with no
/// port number after it (`u:<secret>`), a character no host holds (`%`,
/// `'`, `,`, `=`…: `u%3A<secret>`), or nothing at all before a hard stop
/// (`ssh://\n<secret>@h`).
fn marks_userinfo(authority: &str, hard_stop_first: bool) -> bool {
    if authority.is_empty() {
        return hard_stop_first;
    }
    let (host, port) = match authority.strip_prefix('[') {
        // An IPv6 literal, then an optional port.
        Some(inner) => match inner.find(']') {
            Some(close) => {
                let literal = &inner[..close];
                if !literal
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.' | b'%'))
                {
                    return true;
                }
                ("", &inner[close + 1..])
            }
            None => return true,
        },
        None => match authority.rsplit_once(':') {
            Some((host, _)) => (host, &authority[host.len()..]),
            None => (authority, ""),
        },
    };
    let port_ok = port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    !port_ok
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
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
    let field_at = |k: usize| {
        (k == 0 || !name_char(b[k - 1]))
            && FIELDS
                .iter()
                .any(|(f, ..)| b[k..].starts_with(f.as_bytes()))
    };
    let mut hidden = Hidden::new(text);
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        let field = field_at(i)
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
        let value_end = to.min(b.len());
        // A replacement never takes a `://` with it (`username: "http://u:
        // "p@h/`, a raw `"` in a URL's password): the field's rule stops at
        // that URL's scheme, `scheme://` stays, and the rest of the value
        // goes, so the URL rules still find the `@` after the quote.
        let mut to = value_end;
        let mut url = None;
        if let Some(c) = text[from..to].find("://") {
            to = text[from..from + c]
                .char_indices()
                .rev()
                .take_while(|&(_, ch)| scheme_char(ch))
                .last()
                .map_or(from + c, |(j, _)| from + j);
            url = Some(from + c + 3);
        }
        if from < copied {
            // Inside a value already scrubbed (see below).
            i = from;
            continue;
        }
        hidden.copy(&mut out, copied, from);
        // A stray field that runs over another (`path: "a fragment:
        // Some("f")`) would keep that one's secret: the value is scrubbed
        // whole, and the field inside is read again from its name. Its
        // opening quote is this value's closing one, so its value starts
        // after `to`.
        let inner = (from..to).find(|&k| field_at(k));
        // A value that starts inside a credential's value the free-text
        // rules read (`username: "bob_key=", password: "<s>"`) goes whole,
        // a path's too.
        let covered = hidden.covers(from, to);
        let whole = match path {
            _ if to == from => false,
            true if covered => true,
            // A magnet is written as one, so the URL rules still run it on
            // over what follows (`path: "magnet:s3://…") @<s>`; review).
            true if raw_magnets().from(&text[from..to], 0).is_some() => {
                out.push_str("magnet:");
                out.push_str(REDACTED);
                hidden.hid(from, to);
                false
            }
            // An `@` hides the path, as the URL rules do (`finish`).
            true if text[from..to].contains('@') => true,
            true => {
                let (scrubbed, cut) = scrub_path(&text[from..to]);
                out.push_str(&scrubbed);
                // What the path rules hid of it (its values, and with one
                // holding a `/`, all after it) may hold a credential's name.
                for (a, b) in secret_cuts(&text[from..to]) {
                    hidden.hid(from + a, from + b);
                }
                if let Some(c) = cut {
                    hidden.hid(from + c, to);
                    // As an `@` is kept: a `/` before a userinfo's `@` makes
                    // the URL rules hide the URL whole (`://path: "key:/"@`).
                    out.push('/');
                }
                false
            }
            false => true,
        };
        if whole {
            out.push_str(REDACTED);
            hidden.hid(from, to);
        }
        // A value holding an `@` keeps one after it: the field's name may be
        // inside a URL's raw password (`http://u:a path: "b@h/`), whose `@`
        // the URL rules then still find.
        if text[from..to].contains('@') {
            out.push('@');
        }
        if let Some(sep) = url {
            // `[redacted]@` marks it as userinfo, so the URL rules read a
            // password that runs on past the quote, `/` and all.
            let s = &text[to..sep - 3];
            out.push_str(if known_scheme(s) { s } else { REDACTED });
            out.push_str("://");
            out.push_str(REDACTED);
            out.push('@');
        }
        copied = value_end;
        i = inner.unwrap_or(value_end.max(from));
    }
    hidden.copy(&mut out, copied, text.len());
    out.into()
}

/// What [`scrub_debug_forms`] hides beyond the fields' values: the values
/// of credential names inside what it hid (runner's review). A field's
/// value replaced whole may hold a credential's name, or open its value
/// (`username: "bob_key=", password: "<s>"`: `bob_key=`'s value runs to
/// the next quote, over `password: `). The free-text rules would have
/// read those names, so their values, read from the original text by
/// [`secret_cuts`], go too: each cut that overlaps a hidden region or
/// starts right after it, and then each cut that does so for one of
/// those, and so on. A cut stops at a `://` or a raw magnet, which the
/// URL rules hide as before, and never hides an `@` without leaving one.
struct Hidden<'a> {
    text: &'a str,
    /// The free-text rules' cuts over the original text, in order.
    cuts: Vec<(usize, usize)>,
    /// The first cut not yet taken.
    next: usize,
    /// The cuts taken and not yet written, in order.
    pending: std::collections::VecDeque<(usize, usize)>,
}

impl<'a> Hidden<'a> {
    fn new(text: &'a str) -> Hidden<'a> {
        Hidden {
            text,
            cuts: secret_cuts(text),
            next: 0,
            pending: std::collections::VecDeque::new(),
        }
    }

    /// `from..to` was hidden: the cuts it holds or opens are taken.
    fn hid(&mut self, from: usize, to: usize) {
        // Cuts that end before it can't be reached any more.
        while self.cuts.get(self.next).is_some_and(|&(_, b)| b <= from) {
            self.next += 1;
        }
        while let Some(&(a, b)) = self.cuts.get(self.next) {
            // Right after it: a quote and a space at most.
            if a > to + 2 {
                break;
            }
            self.next += 1;
            let a = a.max(to);
            if b <= a {
                // All inside what's hidden already.
                continue;
            }
            // Never past a `://` or a raw magnet.
            let stop = self.text[a..b]
                .find("://")
                .into_iter()
                .chain(raw_magnets().from(&self.text[a..b], 0))
                .min()
                .map_or(b, |s| a + s);
            if stop > a {
                self.pending.push_back((a, stop));
            }
        }
    }

    /// Whether a taken cut overlaps `from..to`.
    fn covers(&self, from: usize, to: usize) -> bool {
        self.pending
            .iter()
            .any(|&(a, b)| a < to.max(from + 1) && b > from)
    }

    /// `text[from..to]` written to `out`, the taken cuts in it hidden.
    fn copy(&mut self, out: &mut String, from: usize, to: usize) {
        let mut at = from;
        while at < to {
            while self.pending.front().is_some_and(|&(_, b)| b <= at) {
                self.pending.pop_front();
            }
            let Some(&(a, b)) = self.pending.front() else {
                break;
            };
            if a >= to {
                break;
            }
            let (s, e) = (a.max(at), b.min(to));
            out.push_str(&self.text[at..s]);
            out.push_str(REDACTED);
            if self.text[s..e].contains('@') {
                out.push('@');
            }
            // A cut clipped at `to` keeps its rest for the next copy.
            if b <= to {
                self.pending.pop_front();
            }
            self.hid(s, e);
            at = e;
        }
        out.push_str(&self.text[at..to]);
    }
}

/// A bearer token's characters, and `%` and `:`, so a credential glued
/// on behind one goes with it (`Bearer passkey%3D<k>`).
fn token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'_' | b'-' | b'.' | b'~' | b'+' | b'/' | b'=' | b'%' | b':'
        )
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
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (from, to) in secret_cuts(text) {
        out.push_str(&text[copied..from]);
        out.push_str(REDACTED);
        copied = to;
    }
    out.push_str(&text[copied..]);
    out
}

/// The byte ranges [`redact_secrets`] replaces, in order and apart.
fn secret_cuts(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    // Where an unquoted value ends, remembered (the last search's start and
    // answer), since values are read again from their start (below).
    let last = std::cell::Cell::new((usize::MAX, 0));
    let stop = |from: usize| {
        let (start, at) = last.get();
        if start <= from && from <= at {
            return at;
        }
        let at = (from..b.len())
            .find(|&k| {
                b[k].is_ascii_whitespace()
                    || matches!(b[k], b'&' | b';' | b',' | b'"' | b'\'' | b')')
            })
            .unwrap_or(b.len());
        last.set((from, at));
        at
    };
    // Byte ranges to replace, in order; every bound is next to ASCII, so
    // each is a char boundary.
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    // Cuts come in order of their start; one inside the last widens it.
    let mut cut = |from: usize, to: usize| match cuts.last_mut() {
        Some(last) if from <= last.1 => last.1 = last.1.max(to),
        _ => cuts.push((from, to)),
    };
    let mut i = 0;
    // Where the value being read again starts: a word starts there even
    // after `%3D` (`key%3DAuthorization: <secret>`).
    let mut value_at = usize::MAX;
    while i < b.len() {
        let at_word = i == 0 || i == value_at || !name_char(b[i - 1]);
        if !at_word || !name_char(b[i]) {
            i += 1;
            continue;
        }
        let name_end = (i..b.len()).find(|&j| !name_char(b[j])).unwrap_or(b.len());
        if b[i..].starts_with(b"lsk_") || b[i..].starts_with(b"lsf_") {
            cut(i, name_end);
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
                cut(start, end);
            }
            // The token is read again from its start, so a credential's
            // name inside it (`Bearer %3Apassword: "…"`) still has its own
            // value hidden.
            i = start.max(i + 1);
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
                cut(j, end);
            }
            // A value is read again from its start, so a credential's name
            // inside it keeps its own value hidden: a quoted value's closing
            // quote may open the next field's (`password: Some("username:
            // "<secret>"`), and an unquoted one may be a name glued on
            // (`pass:token: <secret>`). Each value's end is searched once
            // (`stop` remembers it), and a quoted one ends at the next quote
            // of its kind, so the reading stays linear.
            // After `%3D` or `%3A`, from its `3`: that's where a word starts
            // when the value isn't read again (`Bearer pass%3Authorization:`
            // has `3Authorization`).
            i = match sep_len {
                3 => sep + 1,
                _ => j,
            }
            .max(i + 1);
            value_at = i;
            continue;
        }
        i = name_end;
    }
    cuts
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
    use proptest::strategy::Strategy;

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
        // A form-encoded URL in a query value, and a network-path reference.
        for encoded in [
            "https%3A%2F%2Fu%3AS3CRET%40h",
            "https%3a%2f%2fu%3aS3CRET%40h",
            "https%3A//u:S3CRET@h",
        ] {
            let s = scrub(&format!("/x?u={encoded}&n=1"), Form::Request);
            assert!(
                !s.contains("S3CRET") && s.ends_with("u=[redacted]&n=1"),
                "{s}"
            );
        }
        assert_eq!(scrub("//u:S3CRET@h/x", Form::Request), "//[redacted]@h/x");
        // An IPv6 literal host doesn't end the URL at its `]`.
        let s = scrub_text("x http://[::1]:6969/ann/0123456789abcdef0123?passkey=K y");
        assert_eq!(s, "x http://[::1]:6969/ann/[redacted] y");
        let s = scrub_text("x https://u:p@[2001:db8::1]/a?passkey=K y");
        assert_eq!(s, "x https://[redacted]@[2001:db8::1]/a y");
        // Over-redaction of an ambiguous line is accepted.
        assert_eq!(
            scrub_text("see https://a.example and me@x.org"),
            "see https://[redacted]@x.org"
        );
        // X-log-url-redact-2: with no sign of userinfo, the wide rule runs
        // to the later of the first `/` and the first whitespace, so a raw
        // `/` and a hard stop in a password go (`http://<secret>/"@h/`),
        // while a path followed by words keeps its path (runner, §2.26).
        assert_eq!(
            scrub_text("see https://a.example/p and me@x"),
            "see https://a.example/p and me@x"
        );
        assert_eq!(
            scrub_text("see https://a.example/p\nme@x"),
            "see https://a.example/p\nme@x"
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

    /// The times for `small` and `large` bytes of `unit` repeated: the
    /// least of several runs each, interleaved so a burst of load on the
    /// machine slows both sizes rather than one.
    fn times_for(unit: &str, small: usize, large: usize) -> [std::time::Duration; 2] {
        let texts = [small, large].map(|n| unit.repeat(n / unit.len()));
        let mut best = [std::time::Duration::MAX; 2];
        for _ in 0..5 {
            for (text, best) in texts.iter().zip(&mut best) {
                let t = std::time::Instant::now();
                std::hint::black_box(scrub_text(std::hint::black_box(text)));
                *best = (*best).min(t.elapsed());
            }
        }
        best
    }

    #[test]
    fn scrub_text_is_linear() {
        // 16x the input takes about 16x the time if linear, 256x if
        // quadratic; 64x splits them. Only the ratio is checked, never
        // the time alone, and both sizes are large enough that timer and
        // scheduler noise stays small next to them.
        for unit in [
            "a://",
            "magnet:",
            "token=",
            "@",
            "://u:p@",
            "a:// ",
            "x magnet:? ",
            "://@",
            "://@http://u:p@h/",
            "?://u:p q",
            // X-log-url-redact-2: look-aheads that cross `://`, magnets
            // that run on, forms without `://`.
            "://u:x",
            "://u:@x\n",
            "x magnet:?dn=a ",
            "magnet:?xt=a b&tr=c ",
            " //a@",
            "a%3A%2F%2F",
            "http:/",
            "[::1]]",
            "\\\"",
            "token: \"",
            "Bearer token=",
            "password: Some(\"username: \"",
            "a: 'b: \"",
            "x/Authorization: ",
            "Bearer a:",
            "token=token=",
            "magnet:?xt=a &tr=token: ",
            "pass:%2F%2F",
            // Runner's re-review: an encoded magnet and `dn` in a value.
            "token=magnet%3A&dn=",
            "pass: magnet%3A%26dn%3D",
        ] {
            let [small, large] = times_for(unit, 256 << 10, 4 << 20);
            let small = small.max(std::time::Duration::from_micros(50));
            let ratio = large.as_secs_f64() / small.as_secs_f64();
            assert!(
                ratio <= 64.0,
                "{unit:?}: {large:?} for 4 MiB, {small:?} for 256 KiB ({ratio:.1}x)"
            );
        }
    }

    #[test]
    fn a_stray_scheme_separator_never_hides_a_later_url() {
        // The proptest's shrunk failure (X-log-url-redact): the bare `://`
        // took the real URL's `@` for its own userinfo, and the real URL
        // was never searched.
        let s = scrub_text("://@http://u:a00aa0@host.example/x");
        assert!(!s.contains("a00aa0"), "{s}");
        // X-log-url-redact-2: a `://` with no scheme, or an `@` before the
        // first hard stop, marks userinfo, whose look-ahead then runs over
        // later `://`s to the last `@`: the host goes too.
        assert_eq!(s, "://[redacted]");
        for (text, secret) in [
            ("x://@http://u:S3cr3t@h/x", "S3cr3t"),
            ("://@://@https://u:S3cr3t@h/x", "S3cr3t"),
            ("://@/http://u:S3cr3t@h/x", "S3cr3t"),
            ("a://@b http://u:S3 cr3t@h/x", "cr3t"),
            ("://@http://u:pa ss@h/x", "pa"),
            ("://@?x=http://u:S3cr3t@h/", "S3cr3t"),
            ("://#@http://u:S3cr3t@h/", "S3cr3t"),
            ("://[a]@http://u:S3cr3t@h/", "S3cr3t"),
            ("http://a@h/r/https://u:S3cr3t@h2/x", "S3cr3t"),
            // A password that holds a `://` (review).
            ("http://user:S3cr3t://x@h/", "S3cr3t"),
            ("http://u:S3cr3t://@h/", "S3cr3t"),
            ("ftp://u:S3cr/3t://@h/", "S3cr"),
            ("x http://u:S3cr3t://x@h/ y", "S3cr3t"),
        ] {
            let s = scrub_text(text);
            assert!(!s.contains(secret), "{text} -> {s}");
        }
        // A passkey after a nested scheme, or glued to one (security
        // review).
        for (text, secret) in [
            (
                "https://tracker.example/https://a1b2c3d4e5f6a7b8c9d0/announce",
                "a1b2c3d4",
            ),
            (
                "http://h/https://0123456789abcdef0123/announce",
                "0123456789",
            ),
            (
                "http://u:p@h/https://0123456789abcdef0123.example/",
                "0123456789",
            ),
            (
                "see http://u:p@h/dl/0123456789ab_cdefghttp://x.example/y",
                "0123456789",
            ),
            // A credential's value glued to `://` isn't a scheme.
            ("x passkey=Zq9S3cr3t_://", "S3cr3t"),
            ("x passkey=Zq9S3cr3t://h/", "S3cr3t"),
            ("token=S3cr3t://h/x", "S3cr3t"),
            ("x://a@passkey=Zq9S3cr3t/y", "S3cr3t"),
            ("http://h/dl/passkey=Zq9S3cr3t", "S3cr3t"),
            ("x://a@[Zq0123456789abcdef/y", "0123456789"),
            ("http://h/a&Zq0123456789abcdef&b", "0123456789"),
            ("'://?'@passkey=Zq9S3cr3t://&.", "S3cr3t"),
            // The runner's review: a Debug value never takes a later URL's
            // `://`; userinfo is never re-read as a scheme; an `@` in the
            // path hides the host.
            (r#"username: "x username: "http://u:"S3cr3t@h/x"#, "S3cr3t"),
            (r#"path: "@)socks5://u:"'>S3cr3t"%<@[::1]/a/b"#, "S3cr3t"),
            ("ws://u:?@S3cr3t9://@h:80", "S3cr3t"),
            ("http://u:@S3cr3t/@h/", "S3cr3t"),
            ("http://u:@/S3cr3t?@@h.example/x", "S3cr3t"),
            (r#"username: "x http://u:/S3cr3t"@h.example/x"#, "S3cr3t"),
            (
                r#"fragment: Some("http://u:@/S3cr3t?@h.example/x"#,
                "S3cr3t",
            ),
            ("magnet:%3Ahttp://u:]S3cr3t?@h.example/x", "S3cr3t"),
            ("magnet:://>\"@#http://u:]S3cr3t#@h.example/x", "S3cr3t"),
            // Round 3: a raw `/` and a hard stop in a password that `main`
            // hid only because something before it swallowed the span.
            (r#"] ://http://u:@/S3cr3t"@h.example/x"#, "S3cr3t"),
            (r#"x magnet:http://u:/S3cr3t"@h.example/x"#, "S3cr3t"),
            (r#"fragment: Some("http://u:"S3cr3t/@h.example/x"#, "S3cr3t"),
            ("http://u:@/S3 cr3t@h.example/x", "cr3t"),
        ] {
            let s = scrub_text(text);
            assert!(!s.contains(secret), "{text} -> {s}");
        }
        // Each URL keeps what it would keep alone, but a `://` with an `@`
        // after it marks userinfo that may run over the next `://`
        // (X-log-url-redact-2), so the second URL's host goes too.
        assert_eq!(
            scrub_text("see http://h/a then://@https://u:p@h2/b"),
            "see http://h/a then://[redacted]"
        );
        // With no userinfo before it, all up to the next `://` goes: it
        // may be a password holding one.
        assert_eq!(
            scrub_text("go http://h/r/https://h2/b?token=t"),
            "go http://[redacted]://h2/b"
        );
        // The userinfo's look-ahead runs over the `://` in the password
        // (X-log-url-redact-2), so the host goes with it.
        assert_eq!(
            scrub_text("http://user:hunter2://x@h/ ok"),
            "http://[redacted] ok"
        );
        // A URL in the query or fragment is dropped with it.
        assert_eq!(
            scrub_text("go http://h/r?u=https://u:p@h2/b&n=1 ok"),
            "go http://[redacted] ok"
        );
        assert_eq!(
            scrub_text("go http://h/r?u=https://h2/b&token=t ok"),
            "go http://h/r ok"
        );
    }

    #[test]
    fn an_at_in_a_path_never_keeps_a_url_s_userinfo() {
        // An earlier `@` ends the authority: a URL in the path kept its
        // userinfo.
        for (url, form) in [
            ("http://a@h/r/https://u:S3cr3t@h2/x", Form::Absolute),
            ("://@http://u:S3cr3t@host.example/x", Form::Absolute),
            ("http://a@h/u:S3cr3t@h2/", Form::Absolute),
            ("/r/https://u:S3cr3t@h2/x", Form::Request),
            ("/r/x?a&https://u:S3cr3t@h2/x", Form::Request),
            ("/r/x?https://u:S3cr3t@h2/x", Form::Request),
            ("http://u:@S3cr3t/@h/", Form::Absolute),
            ("http://u:@S3cr3t/@h/", Form::Request),
        ] {
            let s = scrub(url, form);
            assert!(!s.contains("S3cr3t"), "{url} {form:?} -> {s}");
        }
        assert_eq!(
            scrub("http://a@h/r/https://u:p@h2/x?n=1", Form::Absolute),
            "http://[redacted]"
        );
        assert_eq!(scrub("/r/https://u:p@h2/x", Form::Request), "/[redacted]");
        assert_eq!(scrub("/x?a&https://h2/", Form::Request), "/x?a&[redacted]");
        // A path without an `@` is untouched.
        assert_eq!(
            scrub("http://a@h/r/x?n=1", Form::Absolute),
            "http://[redacted]@h/r/x?n=1"
        );
    }

    #[test]
    fn a_magnet_link_is_never_shown_in_part() {
        // B-log1: magnet links are never shown, a raw tracker URL in one
        // included (runner, round 4).
        for text in [
            "magnet:?xt=urn:btih:abc&dn=Some.Movie.2020.1080p&tr=http://t.example/announce",
            "magnet:?xt=urn:btih:abc&tr=udp://t.example:6969/announce",
            "magnet:?xt=urn:btih:abc&tr=udp://a.example:80/announce&tr=http://b.example/ann",
            "magnet:?xt=urn:btih:abc&dn=Some.Movie.2020http://t.example/announce",
            // Round 5: a soft end or an IPv6 `]` doesn't end a magnet.
            "magnet:?xt=urn:btih:abc&tr=udp://t.example:80/announce,_@ftp://[::1]/dl/Zq0123456789abcdef0123.torrent",
            "magnet:?xt=urn:btih:abc&dn=Secret,Name.2020&tr=http://t.example/a",
            "magnet:?xt=urn:btih:abc&dn=Secret'Name&tr=http://t.example/a",
            "magnet:?xt=urn:btih:abc&dn=Secret(Name)2020&tr=http://t.example/a",
            "magnet:?xt=urn:btih:abc&dn=Secret[Name]2020&tr=http://t.example/a",
            "magnet:?xt=urn:btih:abc&dn=Secret}Name&tr=http://t.example/a",
            "magnet:?xt=urn:btih:abc&tr=udp://a.example:80/announce,udp://b.example/a",
            "magnet:?xt=urn:btih:abc&tr=udp://[::1]:6969/announce&dn=Secret.Name",
        ] {
            assert_eq!(scrub_text(text), "magnet:[redacted]", "{text}");
            // X-log-url-redact-2: a magnet whose `dn` comes last may run on
            // past a space, so the rest of the text goes with it.
            let dn_last = text
                .rsplit('&')
                .next()
                .is_some_and(|p| p.starts_with("dn="));
            let after = if dn_last { "" } else { " ok" };
            assert_eq!(
                scrub_text(&format!("got {text} ok")),
                format!("got magnet:[redacted]{after}"),
                "{text}"
            );
        }
        // Round 6: a URL, a soft end, then a magnet with a raw `tr=` URL.
        for before in [
            "udp://t.example:80/announce,",
            "udp://t.example:80/announce]",
            "udp://t.example:80/announce(",
            "udp://t.example:80/announce'",
            "https://h:443/a,",
            "https://[::1]/a,",
            "http://u:pa]",
        ] {
            let text = format!(
                "see {before}magnet:?xt=urn:btih:abc&dn=SecretName&tr=udp://trk.example/a ok"
            );
            let s = scrub_text(&text);
            assert!(
                !s.contains("trk.example") && !s.contains("SecretName") && s.ends_with(" ok"),
                "{text} -> {s}"
            );
        }
        // Round 7: a magnet inside a password, or with an `@` in its name.
        for text in [
            "see udp://u:magnet:?dn=X SecretPw@h://y ok",
            "see udp://u:magnet:?dn=X\tSecretPw@h://y ok",
            "see udp://u:magnet:?dn=X\"SecretPw@h://y ok",
            "see http://h,magnet:?dn=Name@Group&tr=udp://trk.example/a ok",
        ] {
            let s = scrub_text(text);
            assert!(
                !s.contains("SecretPw") && !s.contains("trk.example") && !s.contains("Group"),
                "{text:?} -> {s}"
            );
        }
        // An IPv6 host in a URL in a fragment keeps its passkey hidden.
        let s = scrub_text("x http://h/x#https://[::1]:6969/ann/Zq0123456789abcdef0123/announce y");
        assert!(!s.contains("0123456789"), "{s}");
        // A glued URL's userinfo stays inside the magnet.
        let s = scrub_text("magnet:?xt=urn:btih:abc&tr=http://u:pa ss@t.example/a ok");
        assert_eq!(s, "magnet:[redacted] ok");
    }

    #[test]
    fn a_raw_password_with_url_syntax_goes_whole() {
        // X-log-url-redact-2, class 1: each leaks on `main` with the URL
        // standing alone.
        for text in [
            // `://` after the secret, and before it.
            "udp://h/xudp://u:@S3CRET\"://@h/x",
            "udp://u:@S3CRET\"://@h/x",
            "ssh://u:://@S3CRET\"@h/",
            "http://u://S3CRET\n@h/x",
            // A raw line break, with no userinfo `@` before it.
            "ssh://u:\nS3CRET@h",
            "wss://u%3AS3CRET\n@h",
            "ssh://u:\r\nS3CRET@h",
            "ftp:///\nS3CRET@h",
            // A `/` and a hard stop with nothing marking the userinfo.
            "http://S3CRET/\"@h/",
            "http://u%3AS3CRET/\"@h/",
        ] {
            for line in [text.to_string(), format!("got {text} ok")] {
                let s = scrub_text(&line);
                assert!(!s.contains("S3CRET"), "{line:?} -> {s:?}");
            }
        }
        // A known gap (runner, §2.26): a `/` and then a space, with nothing
        // marking the userinfo, ends the window. `main` shows it too, and
        // `url` never prints a raw space.
        assert!(scrub_text("see http://S3CRET/ x@h/ ok").contains("S3CRET"));
        // A `:` with no port, or `%3A`, marks it: the look-ahead crosses
        // the line break. With no sign of userinfo it doesn't.
        assert_eq!(scrub_text("ssh://u:\nS3CRET@h ok"), "ssh://[redacted]@h ok");
        assert_eq!(
            scrub_text("see https://h.example:8080/x\nme@x"),
            "see https://h.example:8080/x\nme@x"
        );
        // An encoded password with no raw `@`: no host holds a `%`.
        assert_eq!(
            scrub("wss://u%3AS3CRET%40h/x", Form::Absolute),
            "wss://[redacted]"
        );
        // A malformed IPv6 literal is no host.
        assert_eq!(
            scrub("http://[::1]x/a", Form::Absolute),
            "http://[redacted]"
        );
        assert_eq!(
            scrub("http://[::1]:80/a", Form::Absolute),
            "http://[::1]:80/a"
        );
    }

    #[test]
    fn a_magnet_runs_on_past_a_space_in_its_name() {
        // X-log-url-redact-2, classes 2 and 5 (B-log1).
        for text in [
            "magnet:?xt=urn:btih:abc&dn=Some S3CRET&tr=udp://t.example/a",
            "magnet:?xt=urn:btih:abc&dn=Some\nS3CRET",
            "magnet:?xt=urn:btih:abc&dn=Some\tS3CRET Name",
            "magnet:?xt=urn:btih:abc&kt=a S3CRET",
            "magnet:?xt=urn:btih:abc&tr=http://t.example/a S3CRET&dn=x",
            "magnet:?xt=urn:btih:abc&dn=x&tr=http://t.example/a S3CRET&tr=udp://t/b",
            r#"msg: "see magnet:?xt=urn:btih:abc&dn=a\"S3CRET\" ok""#,
            r#"Custom("magnet:?xt=urn:btih:abc&tr=udp://t/a\"S3CRET\" b")"#,
        ] {
            for line in [text.to_string(), format!("got {text} ok")] {
                let s = scrub_text(&line);
                assert!(
                    !s.contains("S3CRET") && !s.contains("t.example"),
                    "{line:?} -> {s:?}"
                );
            }
        }
        // A name that comes last takes the rest of the text with it; one
        // followed by another parameter ends at the next hard stop.
        assert_eq!(
            scrub_text("add magnet:?xt=urn:btih:abc&dn=Some Name to queue"),
            "add magnet:[redacted]"
        );
        assert_eq!(
            scrub_text("add magnet:?xt=urn:btih:abc&dn=Name&tr=udp://t/a to queue"),
            "add magnet:[redacted] to queue"
        );
    }

    #[test]
    fn a_magnet_inside_a_url_is_never_shown() {
        // X-log-url-redact-2, class 3: the outer URL's soft end once ended
        // the magnet (B-log1).
        for text in [
            "GET http://lodestar:8080/api/add?uri=magnet:?xt=urn:btih:abc&dn=Some.Movie,S3CRET ok",
            "see http://h/x?m=magnet:?xt=urn:btih:abc&tr=udp://t.example/a,S3CRET ok",
            "see http://h/x#magnet:?xt=urn:btih:abc&dn=A)S3CRET ok",
            "see http://h/magnet:?xt=urn:btih:abc&dn=A]S3CRET ok",
            "see http://[::1]magnet%3A%3Fxt%3Durn%3Abtih%3Aabc%26dn%3DS3CRET ok",
        ] {
            let s = scrub_text(text);
            assert!(
                !s.contains("S3CRET") && !s.contains("t.example"),
                "{text:?} -> {s:?}"
            );
        }
        assert!(
            scrub_text("GET http://lodestar:8080/api/add?uri=magnet:?xt=urn:btih:abc&dn=M,2020")
                .starts_with("GET http://lodestar:8080/api/add")
        );
        // The typed rules: a magnet in a query value takes the rest of the
        // query (its own pairs follow it), and one in the path goes too.
        assert_eq!(
            scrub(
                "http://h/api/add?cat=1&uri=magnet:?xt=urn:btih:abc&dn=Some.Movie&tr=udp://t/a",
                Form::Absolute
            ),
            "http://h/api/add?cat=1&uri=[redacted]"
        );
        assert_eq!(
            scrub(
                "/api/add?uri=magnet%3A%3Fxt%3Durn%3Abtih%3Aabc",
                Form::Request
            ),
            "/api/add?uri=[redacted]"
        );
        assert_eq!(
            scrub(
                "http://h/x/magnet:?xt=urn:btih:abc&dn=S3CRET",
                Form::Absolute
            ),
            "http://h/x/magnet:[redacted]"
        );
    }

    #[test]
    fn a_percent_encoded_magnet_is_a_magnet() {
        // X-log-url-redact-2, class 4.
        for (text, shown) in [
            // Its `dn` comes last, so the rest goes with it.
            (
                "see magnet%3A%3Fxt%3Durn%3Abtih%3Aabc%26dn%3DS3CRET ok",
                "see magnet:[redacted]",
            ),
            (
                "see magnet%3A?xt=urn:btih:abc&dn=S3CRET&tr=x ok",
                "see magnet:[redacted] ok",
            ),
            (
                "see MAGNET%253A%253Fxt%253DS3CRET ok",
                "see magnet:[redacted] ok",
            ),
        ] {
            assert_eq!(scrub_text(text), shown, "{text}");
        }
        assert_eq!(
            scrub("magnet%3A%3Fxt%3Dabc", Form::Absolute),
            "magnet:[redacted]"
        );
    }

    #[test]
    fn a_malformed_ipv6_host_never_ends_the_url_early() {
        // X-log-url-redact-2, class 6.
        for text in [
            "x http://h/x?u=http://[::1]]:80/ann/Zq0123456789abcdef0123/announce y",
            "x http://[::1]]/ann/Zq0123456789abcdef0123/announce y",
            "x http://[::1]]]:6969/Zq0123456789abcdef0123 y",
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("0123456789"), "{text} -> {s}");
            assert!(s.ends_with(" y"), "{text} -> {s}");
        }
    }

    #[test]
    fn url_forms_without_a_raw_scheme_separator_lose_their_secrets() {
        // X-log-url-redact-2, class 7: none is found on `main`.
        for text in [
            "see //u:S3CRET@h/x ok",
            "see (//u:S3CRET@h/x) ok",
            "see //u:\nS3CRET@h/x ok",
            "see http:/u:S3CRET@h/x ok",
            "see ssh:/u:S3CRET@h/x ok",
            "see http:%2F%2Fu:S3CRET@h/x ok",
            "see http:%2F/u:S3CRET@h/x ok",
            "see http%3A%2F%2Fu%3AS3CRET%40h/x ok",
            "see http%3A//u:S3CRET@h/x ok",
            r#"{"url":"http:\/\/u:S3CRET@h\/x"}"#,
            "see http%253A%252F%252Fu%253AS3CRET%2540h ok",
            "GET /login?next=https%3A%2F%2Fu%3AS3CRET%40h%2Fx HTTP/1.1",
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("S3CRET"), "{text:?} -> {s:?}");
        }
        assert_eq!(
            scrub_text("see http:/u:S3CRET@h/x ok"),
            "see http://[redacted]@h/x ok"
        );
        assert_eq!(
            scrub_text("see //u:S3CRET@h/x ok"),
            "see ://[redacted]@h/x ok"
        );
        // Ordinary text keeps its slashes: a comment, a protocol-relative
        // URL with no `@`, a Windows path, `file:/`.
        for text in [
            "a // comment",
            "load //cdn.example/x.js",
            "open C:/Users/x",
            "open file:/tmp/x",
            "a//b@c",
        ] {
            assert_eq!(scrub_text(text), text);
        }
        // A query value holding one, encoded in any mix.
        for value in [
            "http:%2F%2Fu:S3CRET@h",
            "http:\\/\\/u:S3CRET@h",
            "http%3A/%2Fu:S3CRET@h",
        ] {
            let s = scrub(&format!("/x?u={value}&n=1"), Form::Request);
            assert_eq!(s, "/x?u=[redacted]&n=1", "{value}");
        }
    }

    #[test]
    fn a_credential_glued_on_with_a_colon_or_an_encoded_equals_goes() {
        // X-log-url-redact-2, class 7: in a query pair with no raw `=`, or
        // before a `://`.
        for (url, form) in [
            ("https://h/x?passkey%3DS3CRET", Form::Absolute),
            ("https://h/x?a=1&token:S3CRET&b=2", Form::Absolute),
            ("https://h/x?a=1&passkey%3dS3CRET", Form::Absolute),
            ("/x?u=a&passkey%3DS3CRET", Form::Request),
            ("token:S3CRET://h/x", Form::Absolute),
            ("passkey%3DS3CRET://h/x", Form::Absolute),
            ("https://h/x?u=passkey%3DS3CRET", Form::Absolute),
        ] {
            let s = scrub(url, form);
            assert!(!s.contains("S3CRET"), "{url} {form:?} -> {s}");
        }
        for text in [
            "Bearer passkey%3DS3CRET://h/x",
            "Bearer token:S3CRET://h/x",
            r#"Bearer %3Apassword: Some("S3CRET")"#,
            // A quoted value is read again: its closing quote opened the
            // next field's (fuzz, against `main`).
            r#"Bearer password: Some("username: "udp:%2F%2Fu:S3CRET]?%3A@10.0.0.1:8080/x"#,
            r#"password: Some("username: "S3CRET"#,
            r#"x: "token: "S3CRET""#,
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("S3CRET"), "{text} -> {s}");
        }
    }

    #[test]
    fn a_credential_s_value_is_never_split_into_a_url() {
        // Security review: the separator pass made a URL inside a value.
        for text in [
            r#"{"token":"https:\/\/hooks.slack.com\/services\/T0\/B0\/S3CRETxyz"}"#,
            "secret=https%3A%2F%2Fhooks.slack.com/services/T0/B0/S3CRETxyz ok",
            "token='https%3A%2F%2Fh.example/hook/S3CRETxyz'",
            "GET /announce?passkey=Zq%3A%2F%2FS3CRETxyz HTTP/1.1",
            "username=bob&password=p%3A%2F%2FS3CRETxyz",
            "token%3A%2F%2FS3CRETxyz",
            "Authorization: Bearer abc:%2F%2FS3CRETxyz",
            "password:%2F%2Fab+S3CRETxyz==",
            "?sig%3A%2F%2FS3CRETxyz",
            "key:%252F%252FS3CRETxyz",
            r"token:\/\/S3CRETxyz",
            "pass:http:/S3CRETxyz",
            "api_key:http%3A%2F%2FS3CRETxyz",
            "x %3A/%2FAuthorization: S3CRETxyz",
            "//]Basic Authorization: -sid%3A@passS3CRETxyz ",
            "://=[:/%2FS3CRETxyz",
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("S3CRET"), "{text:?} -> {s:?}");
        }
        assert_eq!(
            scrub_text("secret=https%3A%2F%2Fh.example/x ok"),
            "secret=[redacted] ok"
        );
    }

    #[test]
    fn a_name_glued_on_never_escapes_with_its_value() {
        // Fresh review: a `Bearer` token, a value or a span that ends on a
        // credential's name kept that name's value.
        for text in [
            "Bearer pass:token: S3CRET",
            r#"Bearer key:password: "S3CRET hunter""#,
            r#"Bearer token:password:"S3CRET"#,
            "Bearer pass%3Authorization: S3CRET",
            r#"Bearer key%3Atoken%3D"S3CRET"#,
            r#"Bearer user:username:"S3CRET"#,
            "bearer\tkey%3DAuthorization: aS3CRET\"",
            "pass:token: S3CRET",
            r#"user:username:"S3CRET"#,
            "magnet:?xt=a &tr=Authorization: S3CRET",
            "//@Bearer S3CRET",
            "x ://S3CRET&magnet:",
            "://S3CRET=magnet:",
            "see https://h/x/token: S3CRET ok",
            "https://h/Some(Bearer Bearer S3CRET",
        ] {
            for s in [
                scrub_text(text),
                scrub(text, Form::Absolute),
                scrub(text, Form::Request),
            ] {
                assert!(!s.contains("S3CRET"), "{text:?} -> {s:?}");
            }
        }
        // A look-ahead over a line break or a `://` took a credential's name
        // into the userinfo (fuzz, against `main`). Free text only: `scrub`
        // takes a URL, and shows these as `main` does.
        for text in [
            "://\nAuthorization: Some(httpx[@S3CRET",
            "://{,\nAuthorization:  @keyS3CRET",
            "://\nAuthorization: passpassword@S3CRET/",
            "Authorization: &tr=://#://@=://S3CRET",
            "http%40sid)https://h/&Basic @key\n:S3CRET",
            "https://h/Basic =Authorization: @%2F/}S3CRETusername",
            "ahttps://h/{//usernamea@S3CRET",
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("S3CRET"), "{text:?} -> {s:?}");
        }
        assert_eq!(
            scrub_text("see https://h/x/token: S3CRET ok"),
            "see https://h/x/token: [redacted] ok"
        );
        // A magnet glued to an authority hides the whole URL.
        assert_eq!(
            scrub("https://S3CRET,magnet:?xt=a", Form::Absolute),
            "https://[redacted]"
        );
    }

    #[test]
    fn a_debug_field_name_in_a_password_keeps_its_at() {
        // Runner (rc.4 delta): a Debug field's name inside a raw password
        // was read as a field, and the value it took held the userinfo's
        // `@`, so what came before showed as a host.
        for text in [
            r#"s3://u:>`S3CRET,path: "x@h/x"#,
            r#"socks5://u:<S3CRET{ fragment: Some("x")@h/x"#,
            r#"https://u:<]S3CRET{ username: "a""@h/x"#,
            r#"wss://u:"S3CRET,query: Some("[@h.example/x"#,
            r#"udp://u:a path: "b"S3CRET@h/x ok"#,
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("S3CRET"), "{text:?} -> {s:?}");
        }
        let s = scrub_text(r#"Url { username: "a@b.example", password: None }"#);
        assert!(!s.contains("b.example"), "{s}");
    }

    #[test]
    fn the_runner_s_review_cases_keep_no_secret() {
        // Runner's review of #460: each hid on `main` and showed at the head
        // the review read.
        for text in [
            // A value whose quote opened before an encoded magnet's span.
            r#"token: "a magnet%3A%3Fxt=b S3CRET""#,
            r#"token="a b magnet%253Axt S3CRET""#,
            r#"token=" magnet%3A%3F http:/u:S3CRET@h"#,
            r#"xpassword: Some(" magnet%3A%3F udp:%2F%2Fu:S3CRET@h/"#,
            // `main` shows this one; it goes too.
            r#"token: "a magnet:?xt=b S3CRET""#,
            // The last magnet before the next `://`, not the first.
            "https://h.example/#magnet:?dn=A B magnet:?xt=a&dn=x@redis://S3CRET \n@h.example/",
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("S3CRET"), "{text:?} -> {s:?}");
        }
        // A Debug value ending on a credential's name never takes the next
        // field's name with its probe (real `url::Url` output).
        for url in [
            "http://user%3A:S3CRETpw@h.example/",
            "https://h.example/dl/key:#S3CRETfrag",
        ] {
            let parsed = ::url::Url::parse(url).unwrap();
            for text in [format!("{parsed:?}"), format!("{parsed:#?}")] {
                let s = scrub_text(&text);
                assert!(!s.contains("S3CRET"), "{text:?} -> {s:?}");
                assert!(s.contains("h.example"), "{text:?} -> {s:?}");
            }
        }
        // Runner's re-review: a hidden value's credential name keeps its own
        // value hidden, however the Debug rules hid the name.
        for text in [
            r#"Login { username: "bob_key=", password: "S3CRET" }"#,
            r#"Feed { query: Some("apikey="), password: "S3CRET" }"#,
            r#"username: "r" Bearer :password: S3CRET"#,
            r#"path: "magnet: password: "S3CRET"#,
            r#"path: "key=/'password: "S3CRET"#,
            r#"://password: Some("z"/@secret=x&S3CRET"#,
            r#"://path: "key:/"@S3CRET"#,
            r#"://username: ":"@key=?://S3CRET"#,
            r#"path: "key=/
pass:"S3CRET"#,
            r#"path: "pw:pw: pw:"S3CRET"#,
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("S3CRET"), "{text:?} -> {s:?}");
        }
        let parsed = ::url::Url::parse("https://h.example/?token=").unwrap();
        let text = format!(r#"{parsed:?} password: "S3CRET""#);
        let s = scrub_text(&text);
        assert!(!s.contains("S3CRET"), "{text:?} -> {s:?}");
        // A name with a space in it ends at a later parameter's word.
        assert_eq!(
            scrub_text("add magnet:?xt=urn:btih:abc&dn=Some Name&tr=udp://t/a to queue"),
            "add magnet:[redacted] to queue"
        );
    }

    #[test]
    fn a_debug_value_glued_to_a_scheme_keeps_no_secret() {
        // Runner, round 4: the run before a Debug value's `://` is shown
        // only if it's a known scheme.
        for text in [
            r#"query: Some("token=S3cr3t9://h/")"#,
            r#"fragment: Some("S3cr3t9http://h/")"#,
            r#"query: Some("a=1&sig=S3cr3t9://x")"#,
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("S3cr3t"), "{text} -> {s}");
        }
        assert_eq!(
            scrub_text(r#"query: Some("http://u:p@h/")"#),
            r#"query: Some("http://[redacted]@")"#
        );
    }

    #[test]
    fn a_port_that_is_no_number_is_a_password() {
        // Runner, round 4: `u:<secret>` with no `@` found.
        for url in ["http://u:S3cr3t9", "http://u:S3cr3t9/x", "http://u:/x"] {
            assert_eq!(scrub(url, Form::Absolute), "http://[redacted]", "{url}");
        }
        assert_eq!(scrub("http://h:8080/x", Form::Absolute), "http://h:8080/x");
        assert_eq!(
            scrub("http://[::1]:6969/x", Form::Absolute),
            "http://[::1]:6969/x"
        );
    }

    #[test]
    fn a_stray_debug_field_never_keeps_a_later_one() {
        // A stray `path: "` ran over the next field, whose value then
        // stayed.
        for text in [
            r#"path: "a fragment: Some("fr4g")"#,
            r#"path: "a, fragment: Some("fr4g") }"#,
            r#"path: "C:" fragment: Some("fr4g")"#,
            r#"x path: "a query: Some("fr4g") fragment: Some("fr4g")"#,
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("fr4g"), "{text} -> {s}");
        }
        assert_eq!(
            scrub_text(r#"path: "a fragment: Some("fr4g")"#),
            r#"path: "a fragment: Some("[redacted]")"#
        );
        // What a stray field covers never stays (security review).
        for text in [
            r#"path: "/dl/fragment: /ABCDEF0123456789abcdef.torrent""#,
            r#"path: "/ann/query: x/0123456789abcdef0123/announce""#,
            r#"path: "fragment: Some("fragment: Some("0123456789abcdefZ"#,
        ] {
            let s = scrub_text(text);
            assert!(!s.contains("0123456789"), "{text} -> {s}");
        }
        // A plain path still keeps its words.
        let plain = format!("{:?}", ::url::Url::parse("https://h/a/b").unwrap());
        assert!(scrub_text(&plain).contains(r#"path: "/a/b""#));
    }

    #[test]
    fn scrub_text_on_a_mebibyte_of_stray_separators_is_quick() {
        for unit in [
            "://@",
            "://@http://u:p@h/",
            "a://@?",
            "://u:x",
            "magnet:?xt=a b&tr=c ",
            " //a@",
            "token=magnet%3A&dn=",
            "pass: magnet%3A%26dn%3D",
        ] {
            let text = unit.repeat((1 << 20) / unit.len());
            // The best of three, so a burst of load on the runner doesn't
            // count; linear work takes well under 0.1 s here.
            let took = (0..3)
                .map(|_| {
                    let t = std::time::Instant::now();
                    std::hint::black_box(scrub_text(std::hint::black_box(&text)));
                    t.elapsed()
                })
                .min()
                .unwrap();
            assert!(
                took < std::time::Duration::from_secs(1),
                "{unit:?}: {took:?} for 1 MiB"
            );
        }
    }

    /// Junk before a URL, weighted towards the characters URL syntax
    /// turns on.
    fn junk() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        let syntax = proptest::sample::select(vec![':', '/', '@', '?', '#', '[', ']']);
        proptest::collection::vec(
            prop_oneof![3 => syntax, 1 => proptest::char::range(' ', '~')],
            0..40,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(1024))]

        #[test]
        fn planted_secrets_never_get_through(
            before in proptest::prop_oneof!["[ -~]{0,40}", junk(), junk().prop_map(|j| format!("{j}://@"))],
            after in "[ -~]{0,40}",
            scheme in "(https?|udp|ftp)",
            secret in "[A-Za-z0-9]{6,20}",
            // URL syntax inside a raw password (review): one kind at a
            // time, or a `/` and a hard stop after a `:` or `@`, and
            // (X-log-url-redact-2) a hard stop or line break with a `://`
            // before or after it.
            syntax in proptest::prop_oneof![
                "[:/@?#,')\\[\\]]{1,4}",
                "[:/@?#]{0,3}://[a-z:/@]{0,4}",
                "[ \"<>`]{1,3}",
                "[:@]{1,2}/[ \"<>`]{1,2}",
                "[ \"<>`\n\r\t]{1,3}(://)?[@ ]{0,2}",
                "(://)?[\n\r]{1,2}[:/@ ]{0,2}",
                "[@:]{0,2}://[ \"\n]{1,2}",
            ],
            // With nothing marking the userinfo: no line break (a URL at a
            // line's end with an address on the next can't be told apart;
            // decisions file).
            bare in "[/ \"<>`,')\\]?#]{1,3}",
            // URL forms without a raw `://`.
            sep in "(://|:%2F%2F|%3A%2F%2F|%3a//|:\\\\/\\\\/|:%2F/|%253A%252F%252F)",
        ) {
            let passkey = format!("Zq{secret}90{secret}");
            let secret = format!("Zq{secret}9");
            for (text, secret) in [
                (format!("{before}{scheme}://u:{secret}@host.example/x{after}"), &secret),
                (format!("{before}{scheme}://host.example/a?passkey={secret}{after}"), &secret),
                (format!("{before}{scheme}://host.example/a?x=1&passkey={secret}&y{after}"), &secret),
                (format!("{before}{scheme}://host.example/ann/{passkey}/announce{after}"), &passkey),
                (format!("{before}{scheme}://host.example/dl/{passkey}.torrent{after}"), &passkey),
                (format!("{before}{scheme}://host.example/{scheme}://{passkey}/announce{after}"), &passkey),
                (format!("{before}{scheme}://host.example/dl/{passkey}{scheme}://x{after}"), &passkey),
                (format!("{before}{scheme}://u:{secret}{syntax}@host.example/x{after}"), &secret),
                (format!("{before}{scheme}://u:{syntax}{secret}@host.example/x{after}"), &secret),
                (format!("{before}{scheme}://u:{syntax}{secret}{syntax}@host.example/x{after}"), &secret),
                (format!("{before}{scheme}://u%3A{syntax}{secret}{syntax}@host.example/x{after}"), &secret),
                (format!("{before}{scheme}://{bare}{secret}{bare}@host.example/x{after}"), &secret),
                (format!("{before}{scheme}{sep}u:{secret}@host.example/x{after}"), &secret),
                (format!("{before} //u:{syntax}{secret}@host.example/x{after}"), &secret),
                (format!("{before} {scheme}:/u:{secret}@host.example/x{after}"), &secret),
            ] {
                let s = scrub_text(&text);
                proptest::prop_assert!(!s.contains(secret.as_str()), "{} -> {}", text, s);
            }
        }

        #[test]
        fn debug_values_and_magnets_keep_no_secret(
            name in "(token|sig|a|passkey|x)",
            field in "(query|fragment)",
            scheme in "(https?|udp|ftp|ws)",
            host in "([a-z]{2,8}\\.example|\\[::1\\])(:[0-9]{2,5})?",
            secret in "[A-Za-z0-9]{6,20}",
            glue in "(|=|&|\\.|,|'|\\(|\\)|\\]|\\})",
        ) {
            let secret = format!("Zq{secret}9");
            let debug = format!(r#"{field}: Some("{name}={secret}{glue}{scheme}://{host}/a")"#);
            let s = scrub_text(&debug);
            proptest::prop_assert!(!s.contains(&secret), "{} -> {}", debug, s);
            let magnet = format!(
                "x magnet:?xt=urn:btih:abc&dn={secret}{glue}&tr={scheme}://{host}/announce y"
            );
            let s = scrub_text(&magnet);
            proptest::prop_assert_eq!(&s, "x magnet:[redacted] y", "{}", magnet);
        }

        #[test]
        fn a_credential_s_value_keeps_its_secret(
            name in "(token|password|passkey|api_key|Authorization|sig|secret|key)",
            glue in "(:|=|%3A|%3D|: |=\"|: \"|: Some\\(\")",
            // What the value starts with: an encoded or escaped `//`, a
            // scheme and `:/`, a `Bearer` chain, a field's name.
            lead in "(|%2F%2F|%252F%252F|\\\\/\\\\/|http:/|https%3A%2F%2F|https:\\\\/\\\\/h\\\\/|Bearer |pass:|user:username:\"|key%3A|x%3D)",
            before in "(|Bearer |x |https://h/|magnet:\\?xt=a&tr=|//@)",
            secret in "[A-Za-z0-9]{6,20}",
            after in "(| ok|\" }|&n=1)",
        ) {
            let secret = format!("Zq{secret}9");
            let text = format!("{before}{name}{glue}{lead}{secret}{after}");
            let s = scrub_text(&text);
            proptest::prop_assert!(!s.contains(&secret), "{:?} -> {:?}", text, s);
        }

        #[test]
        fn magnets_keep_no_secret(
            before in "[ -~]{0,20}",
            scheme in "(https?|udp|ftp)",
            secret in "[A-Za-z0-9]{6,20}",
            // A raw space or line break in a magnet (X-log-url-redact-2).
            ws in "[ \t\n\r]{1,2}",
            tail in "(| ok|&tr=udp://trk.invalid/a ok|&tr=http://u:p@trk.invalid/a ok)",
            soft in "[,)\\]'}(\\[]",
            host in "([a-z]{2,8}\\.example|\\[::1\\]\\]?)(:[0-9]{2,5})?",
        ) {
            let secret = format!("Zq{secret}9");
            for text in [
                format!("{before}magnet:?xt=urn:btih:abc&dn=A{ws}{secret}{tail}"),
                format!("{before}magnet:?xt=urn:btih:abc&tr={scheme}://trk.invalid/a{ws}{secret}&dn=x{tail}"),
                format!("{before}magnet:?xt=urn:btih:abc&kt=a{ws}{secret}{tail}"),
                // Inside a `://` URL, ended there by a soft end.
                format!("{before}{scheme}://{host}/add?uri=magnet:?xt=urn:btih:abc&dn=A{soft}{secret}{tail}"),
                format!("{before}{scheme}://{host}/x{soft}magnet:?xt=urn:btih:abc&dn=A{soft}{secret}{tail}"),
                // Percent-encoded, and escaped in a `Debug` string.
                format!("{before}magnet%3A%3Fxt%3Durn%3Abtih%3Aabc%26dn%3D{secret}{tail}"),
                format!("{before}magnet%3A?xt=urn:btih:abc&dn=A{soft}{secret}{tail}"),
                format!(r#"{before}Custom("magnet:?xt=urn:btih:abc&dn=a\"{secret}\" b"){tail}"#),
            ] {
                let s = scrub_text(&text);
                proptest::prop_assert!(!s.contains(&secret), "{:?} -> {:?}", text, s);
                proptest::prop_assert!(!s.contains("trk.invalid"), "{:?} -> {:?}", text, s);
            }
            // A malformed IPv6 literal before a passkey.
            let passkey = format!("{secret}0{secret}x");
            let text = format!("{before}{scheme}://h/x?u={scheme}://[::1]]:80/ann/{passkey}/announce y");
            let s = scrub_text(&text);
            proptest::prop_assert!(!s.contains(&passkey), "{:?} -> {:?}", text, s);
        }

        #[test]
        fn a_url_after_junk_in_a_path_keeps_no_secret(
            junk in junk(),
            scheme in "(https?|udp|ftp)",
            secret in "[A-Za-z0-9]{6,20}",
        ) {
            let secret = format!("Zq{secret}9");
            for (url, form) in [
                (format!("{scheme}://h/{junk}{scheme}://u:{secret}@host.example/x"), Form::Absolute),
                (format!("{scheme}://a@h/{junk}{scheme}://u:{secret}@host.example/x"), Form::Absolute),
                (format!("/{junk}{scheme}://u:{secret}@host.example/x"), Form::Request),
                (format!("{junk}{scheme}://u:{secret}@host.example/x"), Form::Absolute),
            ] {
                let s = scrub(&url, form);
                proptest::prop_assert!(!s.contains(&secret), "{} {:?} -> {}", url, form, s);
            }
        }

        #[test]
        fn scrubbing_never_panics_on_url_tokens(
            text in "(://|\\[|\\]|::|@|/|\\?|#|é|ü|a|0|magnet:|path: \"|fragment: Some\\(\"|\"| ){0,60}",
        ) {
            let _ = scrub_text(&text);
            let _ = scrub(&text, Form::Absolute);
            let _ = scrub(&text, Form::Request);
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
            "a://[::://",
        ] {
            let _ = scrub(s, Form::Absolute);
            let _ = scrub(s, Form::Request);
            let _ = scrub_text(s);
        }
        // A multi-byte character where the IPv6 look-ahead ends (review).
        for pad in 60..66 {
            let s = format!("see http://[{}é ü", "a".repeat(pad));
            let _ = scrub_text(&s);
        }
    }
}
