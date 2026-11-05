//! The host guard for tracker URLs that come from third parties: claims
//! from feeds and plugins, and the trackers in a cached `.torrent`
//! (X-claim-trackers).
//!
//! These URLs aren't dialed today, but a magnet link or an API answer that
//! carries one makes a client dial it, from inside the user's network. So
//! a kept tracker is never on a host that reaches that network: no literal
//! private, loopback, link-local, metadata or reserved address in any
//! spelling, and no `localhost` name. What's kept is the `url` crate's
//! serialisation, the address as it was judged, never the raw text a
//! looser parser could read as another host.
//!
//! This is a DNS-free check. A public name that resolves to a private
//! address is the dialing client's concern, as it is for any tracker.

use reqwest::Url;

use super::{NetPolicy, check_url};

/// `raw` in the `url` crate's form when it's a tracker on a public host,
/// else `None`. Kept: `http`, `https` and `udp` URLs with a host, no user
/// name or password, a host that isn't a literal non-public address
/// ([`classify`](super::classify): IPv4-mapped, compatible, NAT64 and 6to4
/// forms by the IPv4 address they carry) and isn't `localhost` or a
/// `*.localhost` name (RFC 6761), in any case or IDNA spelling.
///
/// `udp` isn't a special scheme to the URL standard, so its host is opaque
/// (not lower-cased, decoded or IDNA-mapped). It's judged as the same host
/// over `http`, which is how a client resolving it would read it; a host
/// that doesn't parse that way is refused.
///
/// The judged host must be a plain hostname (ASCII lower-case letters,
/// digits, `-` and `.`) or an IP literal, and a `udp` URL is stored with
/// that judged host, not its opaque spelling.
///
/// The kept form has no `@` anywhere, and nothing but the host and port
/// between `://` and the first `/`, so a reader that splits the text
/// naively finds the same host the `url` crate does.
///
/// The result is a fixed point: `public_tracker_url(&kept) == Some(kept)`.
/// Length and other caps are the caller's.
pub fn public_tracker_url(raw: &str) -> Option<String> {
    let mut url = Url::parse(raw).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let judged = match url.scheme() {
        "http" | "https" => url.clone(),
        "udp" => {
            let host = url.host_str().filter(|h| !h.is_empty())?;
            Url::parse(&format!("http://{host}/")).ok()?
        }
        _ => return None,
    };
    check_url(&judged, NetPolicy::notification(false)).ok()?;
    let host = judged.host_str()?;
    if !hostname_chars(host) || local_name(host) {
        return None;
    }
    // A `udp` URL keeps the host as judged (lower-cased, decoded,
    // IDNA-mapped), never its opaque spelling: `udp://T.Example` and
    // `udp://%74.example` store `t.example`, so a reader that doesn't
    // decode can't see a different set of labels.
    if url.scheme() == "udp" {
        url.set_host(Some(host)).ok()?;
        if url.host_str() != Some(host) {
            return None;
        }
    }
    let kept = String::from(url);
    // The serialisation must read back as itself, or what's stored isn't
    // what was judged. And no looser reader may find another host in it:
    // a `udp` URL with no path keeps a `?` or `#` straight after the
    // authority (`udp://t.example:6969?@192.168.1.1:80/`), which a parser
    // that ends the authority at the first `/` reads as user info before
    // a LAN host. So no `@` anywhere, and the authority ends at a `/` or
    // the end of the text.
    let authority = kept.split_once("://")?.1.split('/').next()?;
    let unambiguous = !kept.contains('@') && !authority.contains(['?', '#']);
    (unambiguous && Url::parse(&kept).ok()?.as_str() == kept).then_some(kept)
}

/// Whether a judged host is made only of what a hostname or an IP
/// literal can hold: ASCII lower-case letters, digits, `-` and `.`, or a
/// bracketed IPv6 literal. The `url` crate keeps `,`, `;`, `_` and other
/// characters in a domain, and a reader that splits a tracker list on
/// them would find `127.0.0.1` in `127.0.0.1,t.example`.
fn hostname_chars(host: &str) -> bool {
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6.parse::<std::net::Ipv6Addr>().is_ok();
    }
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

/// Names that resolve inside the user's network, judged after the `url`
/// crate's lower-casing and IDNA mapping and with any trailing root dots:
/// `localhost` and every name under it (RFC 6761), the special-use LAN
/// domains `local` (mDNS, RFC 6762), `home.arpa` (RFC 8375) and
/// `internal` (ICANN's private-use TLD), and any single-label name, which
/// a resolver completes with the LAN's search domain. `localdomain` (and
/// its `4` and `6` forms) is refused too: Red Hat-family systems map
/// `localhost.localdomain` to loopback in `/etc/hosts`, and it's a common
/// LAN search domain. So is a
/// name whose last label is a number (`127.0.0.1..`, which the `url`
/// crate reads as a name rather than an address), since a looser IPv4
/// parser could read it as one. A bracketed IPv6 literal or a dotted IPv4
/// one isn't a name; [`check_url`] judged it.
fn local_name(host: &str) -> bool {
    if host.starts_with('[') || host.parse::<std::net::Ipv4Addr>().is_ok() {
        return false;
    }
    let host = host.trim_end_matches('.');
    let last = host.rsplit('.').next().unwrap_or_default();
    !host.contains('.')
        || last.bytes().all(|b| b.is_ascii_digit())
        || last.starts_with("0x")
        || [
            "localhost",
            "localdomain",
            "localdomain4",
            "localdomain6",
            "local",
            "home.arpa",
            "internal",
        ]
        .iter()
        .any(|d| host == *d || host.strip_suffix(d).is_some_and(|h| h.ends_with('.')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{AddrClass, classify};
    use proptest::prelude::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn public_trackers_are_kept_in_canonical_form() {
        for (raw, kept) in [
            (
                "udp://tracker.example:1337/announce",
                "udp://tracker.example:1337/announce",
            ),
            (
                "https://tracker.example/announce",
                "https://tracker.example/announce",
            ),
            (
                "http://tracker.example:6969/announce",
                "http://tracker.example:6969/announce",
            ),
            (
                "HTTPS://Tracker.Example:443/announce",
                "https://tracker.example/announce",
            ),
            ("http://tracker.example", "http://tracker.example/"),
            ("http://8.8.8.8/announce", "http://8.8.8.8/announce"),
            (
                "udp://[2606:4700::1]:6969/announce",
                "udp://[2606:4700::1]:6969/announce",
            ),
            (
                "https://t.example/announce?passkey=x",
                "https://t.example/announce?passkey=x",
            ),
            // IDNA: a public name stays public, in its ASCII form.
            (
                "http://bücher.example/announce",
                "http://xn--bcher-kva.example/announce",
            ),
            // Not a `localhost` label, only a name that contains it.
            (
                "http://localhost.example/announce",
                "http://localhost.example/announce",
            ),
            (
                "http://notlocalhost.example/announce",
                "http://notlocalhost.example/announce",
            ),
            (
                "http://t.local.example/announce",
                "http://t.local.example/announce",
            ),
            ("http://t.example./announce", "http://t.example./announce"),
            // A udp host is stored as judged, not as spelled.
            ("udp://T.Example:6969/a", "udp://t.example:6969/a"),
            ("udp://%74.example:6969/a", "udp://t.example:6969/a"),
            (
                "udp://bücher.example:6969/a",
                "udp://xn--bcher-kva.example:6969/a",
            ),
            ("udp://t%E3%80%82example:6969/a", "udp://t.example:6969/a"),
            ("udp://8.8.8.8:6969/a", "udp://8.8.8.8:6969/a"),
            // url 2.5.8 leaves `^` unencoded in a path.
            ("http:1.1/announce^x", "http://1.0.0.1/announce^x"),
        ] {
            assert_eq!(public_tracker_url(raw).as_deref(), Some(kept), "{raw}");
        }
    }

    #[test]
    fn every_blocked_class_and_spelling_is_refused() {
        for bad in [
            // Loopback, every spelling.
            "http://127.0.0.1/announce",
            "udp://127.0.0.1:6969/announce",
            "http://127.1/announce",
            "http://0x7f.1/announce",
            "http://2130706433/announce",
            "http://0177.0.0.1/announce",
            "udp://127.1:6969/announce",
            "udp://0x7f.1:6969/announce",
            "http://[::1]/announce",
            "udp://[::1]:6969/announce",
            "http://[::ffff:127.0.0.1]/announce",
            "udp://[::ffff:127.0.0.1]:80/announce",
            "http://[::127.0.0.1]/announce",
            "http://[64:ff9b::7f00:1]/announce",
            "http://[2002:7f00:1::]/announce",
            // localhost, every spelling.
            "http://localhost/announce",
            "http://LOCALHOST/announce",
            "http://localhost./announce",
            "http://foo.localhost/",
            "http://Foo.LocalHost./announce",
            "udp://localhost:6969/announce",
            "udp://LocalHost:6969/announce",
            "udp://a.b.localhost:6969/announce",
            "http://ⓛocalhost/announce",
            "udp://ⓛocalhost:6969/announce",
            "http://%6Cocalhost/announce",
            "udp://%6Cocalhost:6969/announce",
            "http://ＬＯＣＡＬＨＯＳＴ/announce",
            // LAN names: single-label, mDNS, home.arpa, internal.
            "http:///announce",
            "http://router/announce",
            "udp://nas:6969/announce",
            "http://router./announce",
            "http://t.local/announce",
            "http://T.LOCAL./announce",
            "udp://t.local:6969/announce",
            "http://nas.home.arpa/announce",
            "http://t.internal/announce",
            "http://localhost.localdomain/announce",
            "udp://localhost6.localdomain6:6969/announce",
            "http://nas.localdomain/announce",
            "http://127.0.0.1../announce",
            "udp://127.0.0.1..:6969/announce",
            "http://0x7f.1../announce",
            "http://127.0.0.1。。/announce",
            "http://t。local/announce",
            "http://ⓛocalhost．/announce",
            "http://xn--ocalhost-/announce",
            // Private and carrier-grade NAT.
            "udp://192.168.1.1:80/announce",
            "http://10.0.0.1/announce",
            "http://172.16.0.1/announce",
            "http://100.64.0.1/announce",
            "http://[fd00::1]/announce",
            "http://[::ffff:192.168.1.1]/announce",
            // Link-local and metadata.
            "http://169.254.169.254/announce",
            "http://169.254.1.1/announce",
            "http://[fe80::1]/announce",
            "http://100.100.100.200/announce",
            // Unspecified, multicast, broadcast.
            "http://0.0.0.0/announce",
            "udp://0.0.0.0:6969/announce",
            "http://0/announce",
            "http://[::]/announce",
            "http://224.0.0.1/announce",
            "http://255.255.255.255/announce",
            "http://[ff02::1]/announce",
            // Userinfo, plain and smuggled.
            "https://user@t.example/announce",
            "https://user:pw@t.example/announce",
            "https://:pw@t.example/announce",
            "udp://user@t.example:6969/announce",
            "http://t.example%40127.0.0.1/announce",
            "http://t.example@127.0.0.1/announce",
            "http://t.example\\@127.0.0.1/announce",
            "udp://t.example%40127.0.0.1:6969/announce",
            // Backslash, tab and newline: stripped or read as `/` by the
            // parser, so the host judged is the one the parser sees.
            "http:\\\\127.0.0.1\\announce",
            "http://127.0.\t0.1/announce",
            "http://127.0.\n0.1/announce",
            "http://local\thost/announce",
            "udp://127.0.0.1\\x:6969/announce",
            // A looser parser's reading: `udp` keeps a `?` or `#` right
            // after the authority, and `@` after it reads as user info.
            "udp://t.example:6969?@192.168.1.1:80/announce",
            "udp://t.example:6969?@127.0.0.1:6969",
            "udp://t.example:6969#@127.0.0.1:6969",
            "udp://t.example:6969?x",
            "udp://t.example#x",
            "http://t.example/announce?u=@127.0.0.1",
            "http://t.example/@127.0.0.1/announce",
            // Characters no hostname holds, which a list splitter could
            // cut at.
            "http://127.0.0.1,t.example/announce",
            "http://localhost,t.example/a",
            "http://127.0.0.1;t.example/a",
            "http://127.0.0.1_.example/a",
            "udp://127.0.0.1,t.example:6969/announce",
            "http://t.example,/a",
            "http://t_x.example/a",
            "http://t.ex$ample/a",
            "http://t.ex!ample/a",
            "udp://t.ex*ample:6969/a",
            // Other schemes and no host.
            "wss://t.example/announce",
            "ws://t.example/announce",
            "file:///etc/passwd",
            "udp:///announce",
            "udp:announce",
            "",
            "not a url",
        ] {
            assert_eq!(public_tracker_url(bad), None, "{bad:?}");
        }
    }

    /// The host as a client would dial it: for `udp`, re-read over `http`.
    fn dialed_host(kept: &str) -> Option<String> {
        let u = Url::parse(kept).ok()?;
        let u = if u.scheme() == "udp" {
            Url::parse(&format!("http://{}/", u.host_str()?)).ok()?
        } else {
            u
        };
        u.host_str().map(str::to_owned)
    }

    fn blocked(host: &str) -> bool {
        let lit = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .map(|h| h.parse::<Ipv6Addr>().ok().map(IpAddr::V6))
            .unwrap_or_else(|| host.parse::<Ipv4Addr>().ok().map(IpAddr::V4));
        let name = host.trim_end_matches('.');
        let last = name.rsplit('.').next().unwrap_or_default();
        let lan = !name.contains('.')
            || last.bytes().all(|b| b.is_ascii_digit())
            || last.starts_with("0x")
            || [
                "localhost",
                "localdomain",
                "localdomain4",
                "localdomain6",
                "local",
                "home.arpa",
                "internal",
            ]
            .iter()
            .any(|d| name == *d || name.ends_with(&format!(".{d}")));
        match lit {
            Some(ip) => classify(ip) != AddrClass::Public,
            None => lan,
        }
    }

    /// The host as a naive reader finds it: the authority runs from `://`
    /// to the first `/`, the host follows its last `@` and precedes the
    /// port.
    fn naive_host(kept: &str) -> &str {
        let authority = kept.split_once("://").map_or("", |(_, r)| r);
        let authority = authority.split('/').next().unwrap_or_default();
        let hostport = authority.rsplit('@').next().unwrap_or_default();
        if hostport.starts_with('[') {
            hostport.split_inclusive(']').next().unwrap_or_default()
        } else {
            hostport.rsplit_once(':').map_or(hostport, |(h, _)| h)
        }
    }

    fn check(raw: &str) {
        if let Some(kept) = public_tracker_url(raw) {
            assert_eq!(public_tracker_url(&kept).as_deref(), Some(kept.as_str()));
            let host = dialed_host(&kept).expect("a kept tracker has a host");
            assert!(!blocked(&host), "{raw:?} kept as {kept:?}");
            let u = Url::parse(&kept).unwrap();
            assert_eq!(
                naive_host(&kept),
                u.host_str().unwrap(),
                "{raw:?} kept as {kept:?}"
            );
            let h = u.host_str().unwrap();
            let plain =
                |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.';
            assert!(
                h.starts_with('[') || h.bytes().all(plain),
                "{raw:?} kept as {kept:?}"
            );
            assert_eq!(h, host, "the stored host is the judged one: {raw:?}");
            assert!(u.username().is_empty() && u.password().is_none());
            assert!(matches!(u.scheme(), "http" | "https" | "udp"));
        }
    }

    fn octet() -> impl Strategy<Value = String> {
        prop_oneof![
            (0u8..=255).prop_map(|b| b.to_string()),
            (0u8..=255).prop_map(|b| format!("0x{b:x}")),
            (0u8..=255).prop_map(|b| format!("0{b:o}")),
            Just("127".to_owned()),
            Just("0".to_owned()),
        ]
    }

    fn host() -> impl Strategy<Value = String> {
        prop_oneof![
            prop::collection::vec(octet(), 1..=4).prop_map(|o| o.join(".")),
            any::<u32>().prop_map(|n| n.to_string()),
            any::<u32>().prop_map(|n| Ipv4Addr::from(n).to_string()),
            any::<u128>().prop_map(|n| format!("[{}]", Ipv6Addr::from(n))),
            any::<u32>().prop_map(|n| format!("[::ffff:{}]", Ipv4Addr::from(n))),
            any::<u32>().prop_map(|n| format!("[64:ff9b::{}]", Ipv4Addr::from(n))),
            "[a-zA-Z0-9.ⓛＬ%@\\\\:,;_$!*-]{0,24}",
            "[a-z]{0,8}\\.?(localhost|LOCALHOST|ⓛocalhost|%6cocalhost)\\.?",
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        #[test]
        fn never_panics_on_any_text(raw in "\\PC{0,64}") {
            check(&raw);
        }

        #[test]
        fn kept_trackers_are_fixed_points_on_public_hosts(
            scheme in prop_oneof![Just("http"), Just("https"), Just("udp"), Just("HTTP"), Just("wss")],
            sep in prop_oneof![Just("://"), Just(":"), Just(":\\\\"), Just(":/\t/")],
            userinfo in prop_oneof![Just(""), Just("u@"), Just("u:p@"), Just("%40")],
            host in host(),
            port in prop_oneof![Just(String::new()), (0u32..70_000).prop_map(|p| format!(":{p}"))],
            slash in prop_oneof![Just("/"), Just("")],
            path in "[a-z/^\\\\%@?#\t]{0,12}",
        ) {
            check(&format!("{scheme}{sep}{userinfo}{host}{port}{slash}{path}"));
        }
    }
}
