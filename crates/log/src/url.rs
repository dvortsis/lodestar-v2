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
                // Free text's scheme may be the tail of what came before
                // (`/dl/<passkey>.torrent://`): a real one is never
                // passkey-like.
                out.push_str(&redact_runs(scheme));
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

/// A query value holding a URL, raw or form-encoded (`https://…`,
/// `https%3A%2F%2F…`, `https%3A//…`), whose userinfo would otherwise stay.
fn holds_url(value: &str) -> bool {
    value.contains("://")
        || [b"%3a%2f%2f".as_slice(), b"%3a//"]
            .iter()
            .any(|n| find_ci_bytes(value.as_bytes(), n).is_some())
}

/// `rest` (a path, query and fragment) appended to `out`, scrubbed.
fn finish(mut out: String, rest: &str, drop_query: bool) -> String {
    let rest = rest.split('#').next().unwrap_or(rest);
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    // An `@` in the path, after the authority came off, may be a URL's
    // userinfo that an earlier `@` hid (`http://a@h/r/http://u:p@h2/`, or
    // `://@http://u:p@h/` in free text): the path goes whole, its query
    // too, as the authority rules hide an `@` in a path: never too little.
    if path.contains('@') {
        if path.starts_with('/') {
            out.push('/');
        }
        out.push_str(REDACTED);
        return out;
    }
    for (i, segment) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        // `name=value` credentials too (`/dl/passkey=<k>`).
        out.push_str(&redact_runs(&redact_secrets(segment)));
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
                Some((name, value)) if credential(name) || holds_url(value) => {
                    out.push_str(name);
                    out.push('=');
                    out.push_str(REDACTED);
                }
                Some((name, value)) => {
                    out.push_str(name);
                    out.push('=');
                    out.push_str(&redact_runs(value));
                }
                // A URL in a pair with no `=` (`?a&https://u:p@h/`).
                None if holds_url(pair) => out.push_str(REDACTED),
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
                .take_while(|&(_, c)| scheme_char(c))
                .last()
                .map_or(i, |(j, _)| j)
        } else {
            i
        };
        let mut from = i;
        // Where the next `://` starts in `rest`, if there's one after this
        // URL's own.
        let mut nested = None;
        if rest[i..].starts_with("://") {
            nested = scheme.from(text, pos + i + 3).map(|n| n - pos);
            let next = nested.unwrap_or(rest.len()) - (i + 3);
            let at = userinfo_in_text(&rest[i + 3..], next);
            if let Some(at) = at {
                from = i + 3 + at + 1;
            }
        }
        // An IPv6 literal host (`http://[::1]:6969/…`): its `]` doesn't end
        // the URL.
        let mut scan = from;
        let host = if from == i { i + 3 } else { from };
        if rest[i..].starts_with("://") {
            let past = past_ipv6(rest, host);
            if past != host {
                scan = past;
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
            // A magnet link (B-log1: never shown) ends only at a hard stop,
            // never at a soft end or an IPv6 `]` (`dn=Name,2020`,
            // `tr=udp://[::1]:6969/`). A URL glued to it stays in it, and
            // the magnet runs on to that URL's end, past its userinfo.
            None if !rest[i..].starts_with("://") => {
                let magnet_end = |from: usize| {
                    rest[from..]
                        .char_indices()
                        .find(|(_, c)| hard_stop(*c))
                        .map_or(rest.len(), |(j, _)| from + j)
                };
                let end = magnet_end(scan);
                match scheme.from(text, pos + i).map(|n| n - pos) {
                    // Each `://` it runs over gets its userinfo searched,
                    // and a scan never covers bytes an earlier one did.
                    Some(mut n) if n >= i + 7 && n < end => {
                        let mut end = end;
                        loop {
                            let next = scheme.from(text, pos + n + 3).map(|m| m - pos);
                            let bound = next.unwrap_or(rest.len()) - (n + 3);
                            if let Some(at) = userinfo_in_text(&rest[n + 3..], bound) {
                                let after = n + 3 + at + 1;
                                if after >= end {
                                    end = magnet_end(after);
                                }
                            }
                            match next {
                                Some(m) if m < end => n = m,
                                _ => break end,
                            }
                        }
                    }
                    _ => end,
                }
            }
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
                // `://`, and each still gets its own bounded userinfo
                // search, so a raw space in its password can't end the
                // span early.
                None if rest[host..nested].contains(['?', '#']) => {
                    let mut cur = nested;
                    loop {
                        let next = scheme.from(text, pos + cur + 3).map(|n| n - pos);
                        let bound = next.unwrap_or(rest.len()) - (cur + 3);
                        let scan = userinfo_in_text(&rest[cur + 3..], bound)
                            .map_or(cur + 3, |at| cur + 3 + at + 1);
                        // An IPv6 host's `]` doesn't end it here either.
                        let scan = past_ipv6(rest, scan);
                        match next {
                            Some(n) => match url_end_before(rest, scan, n) {
                                Some(end) => break end,
                                None => cur = n,
                            },
                            None => break url_end(scan),
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
                    out.push_str(&lead(rest, start, i));
                    out.push_str("://");
                    out.push_str(REDACTED);
                    // A magnet in what's hidden gets its own span (B-log1),
                    // so its tracker URL is never read as the next URL.
                    let stop = magnet
                        .from(text, pos + i + 3)
                        .map(|m| m - pos)
                        .filter(|&m| m < nested)
                        .unwrap_or(nested);
                    pos += stop;
                    continue;
                }
                Some(_) | None => {
                    let scheme_start = scheme_start(rest, scan, nested);
                    // After `=` it's a value (`@passkey=<k>://`): it stays
                    // with this URL, whose host and path rules see the
                    // name, and the next URL has no scheme.
                    match rest[..scheme_start].ends_with('=') {
                        true => nested,
                        false => scheme_start.max(scan),
                    }
                }
            },
        };
        out.push_str(&lead(rest, start, i));
        out.push_str(&scrub_with(&rest[i..end.max(i)], Form::Absolute, true));
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

/// The text before a URL in `rest`, its scheme (`start..i`) included,
/// scrubbed as plain text: a credential's value glued to `://`
/// (`passkey=<k>://`) would otherwise be taken for a scheme. A scheme
/// that's left whole goes through the passkey rules too.
fn lead(rest: &str, start: usize, i: usize) -> String {
    let lead = hide_runs(&redact_secrets(&rest[..i]));
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
        Some(close) => host + 1 + close + 1,
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
    let narrow = after[..hard].rfind('@');
    let mut bound = after.len().min(USERINFO_LOOKAHEAD);
    while !after.is_char_boundary(bound) {
        bound -= 1;
    }
    // One scan: the window ends at the first line break, and at the first
    // `/` unless there's userinfo: an `@` before the first hard stop, or a
    // `:` with no port number after it (`u:/p"@h/`). Its password may hold
    // `/` and a hard stop (`u:@/p"@h/`).
    let window = &after[..bound];
    let authority = &after[..after.find(['/', '?', '#']).unwrap_or(after.len()).min(hard)];
    let no_port = !authority.starts_with('[')
        && authority
            .rsplit_once(':')
            .is_some_and(|(_, port)| port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()));
    let ends: &[char] = match (narrow.is_some(), no_port) {
        (true, _) => &[],
        (false, true) => &['\n', '\r'],
        (false, false) => &['/', '\n', '\r'],
    };
    let window = &window[..window.find(ends).unwrap_or(window.len())];
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
        out.push_str(&text[copied..from]);
        // A stray field that runs over another (`path: "a fragment:
        // Some("f")`) would keep that one's secret: the value is scrubbed
        // whole, and the field inside is read again from its name. Its
        // opening quote is this value's closing one, so its value starts
        // after `to`.
        let field_at = |k: usize| {
            !name_char(b[k - 1])
                && FIELDS
                    .iter()
                    .any(|(f, ..)| b[k..].starts_with(f.as_bytes()))
        };
        let inner = (from..to).find(|&k| field_at(k));
        match path {
            _ if to == from => {}
            // An `@` hides the path, as the URL rules do (`finish`).
            true if text[from..to].contains('@') => out.push_str(REDACTED),
            true => {
                let mut scrubbed = String::new();
                for (k, segment) in text[from..to].split('/').enumerate() {
                    if k > 0 {
                        scrubbed.push('/');
                    }
                    scrubbed.push_str(&redact_runs(&redact_secrets(segment)));
                }
                out.push_str(&scrubbed);
            }
            false => out.push_str(REDACTED),
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
        assert_eq!(s, "://[redacted]://[redacted]@host.example/x");
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
        // Each URL keeps what it would keep alone.
        assert_eq!(
            scrub_text("see http://h/a then://@https://u:p@h2/b"),
            "see http://h/a then://[redacted]://[redacted]@h2/b"
        );
        // With no userinfo before it, all up to the next `://` goes: it
        // may be a password holding one.
        assert_eq!(
            scrub_text("go http://h/r/https://h2/b?token=t"),
            "go http://[redacted]://h2/b"
        );
        assert_eq!(
            scrub_text("http://user:hunter2://x@h/ ok"),
            "http://[redacted]://[redacted]@h/ ok"
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
            assert_eq!(
                scrub_text(&format!("got {text} ok")),
                "got magnet:[redacted] ok",
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
        // An IPv6 host in a URL in a fragment keeps its passkey hidden.
        let s = scrub_text("x http://h/x#https://[::1]:6969/ann/Zq0123456789abcdef0123/announce y");
        assert!(!s.contains("0123456789"), "{s}");
        // A glued URL's userinfo stays inside the magnet.
        let s = scrub_text("magnet:?xt=urn:btih:abc&tr=http://u:pa ss@t.example/a ok");
        assert_eq!(s, "magnet:[redacted] ok");
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
        for unit in ["://@", "://@http://u:p@h/", "a://@?"] {
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
            // time, or a `/` and a hard stop after a `:` or `@`. A space
            // with a `://` after it is a known gap (the look-ahead stops at
            // the `://`; decisions file).
            syntax in proptest::prop_oneof![
                "[:/@?#,')\\[\\]]{1,4}",
                "[:/@?#]{0,3}://[a-z:/@]{0,4}",
                "[ \"<>`]{1,3}",
                "[:@]{1,2}/[ \"<>`]{1,2}",
            ],
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
