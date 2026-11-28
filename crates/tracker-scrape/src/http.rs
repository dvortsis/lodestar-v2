//! HTTP tracker scrape (BEP 48): `GET <scrape url>?info_hash=…&info_hash=…`
//! returning a bencoded dictionary
//!
//! ```text
//! d5:filesd20:<hash>d8:completei5e10:downloadedi50e10:incompletei10eeee
//! ```
//!
//! or `failure reason`. Optional `flags.min_request_interval` asks us to
//! back off. Bodies are capped before they're read, and the bencode is
//! decoded with tight limits.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use ::url::Url;
use lodestar_bencode::{self as bencode, DecodeOptions, Dict, Value};
use lodestar_metainfo::InfoHash;

use crate::{Counts, Error, Reply, Result, check_batch, message};

/// Most hashes one HTTP request may carry. Many trackers cap multi-hash
/// scrapes well below this; [`HttpConfig::max_hashes`] sets the batch.
pub const MAX_HASHES: usize = 74;
/// Body cap: a full reply is about 70 B per hash, so this leaves ample
/// room for names and padding while stopping a hostile stream early.
pub const MAX_BODY: usize = 64 << 10;
/// Longest wait a tracker may ask for.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(86_400);

/// The deepest legitimate nesting is top → `files` → entry → value.
const LIMITS: bencode::Limits = bencode::Limits {
    max_depth: 4,
    max_items: 4096,
};

#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Hashes per request, at most [`MAX_HASHES`].
    pub max_hashes: usize,
    /// Whole request, connect to last byte.
    pub timeout: Duration,
    /// Blocklisted addresses: give the same one to [`client`], which
    /// checks names and redirects; the tracker checks a literal IP host.
    pub deny: Option<crate::Deny>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        HttpConfig {
            max_hashes: 32,
            timeout: Duration::from_secs(15),
            deny: None,
        }
    }
}

/// The HTTP client shared by every HTTP tracker.
///
/// It ignores `HTTP(S)_PROXY`: crawler traffic leaves through the bound
/// interface or VPN like the DHT does (§5), not through whatever proxy
/// the environment names. Redirects are limited, since a tracker has no
/// business sending us far away. `local` sets the source address.
///
/// With `deny`, host names resolve to their allowed addresses only, and a
/// redirect to a blocklisted literal IP is refused.
///
/// Bound to `local`, it verifies certificates as every routed client does
/// (`lodestar_integrations::tls`); unbound, with the OS verifier.
pub fn client(local: Option<IpAddr>, deny: Option<&crate::Deny>) -> Result<reqwest::Client> {
    builder(local, deny, None)?
        .build()
        .map_err(|e| Error::Http(e.to_string()))
}

/// [`client`], sending only through `iface` (X-vpn-bind), and looking
/// no name up ([`NoLookups`]: a tracker named by host fails, one named by
/// address works). Linux only: pinned to the device (`SO_BINDTODEVICE`).
/// reqwest gives no hook to pin its sockets on Windows (binding an address
/// alone leaves the weak-host send path open), so there, as elsewhere, the
/// client isn't built and HTTP trackers stay off ([`through_reaches`])
/// rather than going out by the normal route.
pub fn client_through(
    iface: &lodestar_network_policy::iface::Interface,
    deny: Option<&crate::Deny>,
) -> Result<reqwest::Client> {
    through(iface, deny)?
        .build()
        .map_err(|e| Error::Http(e.to_string()))
}

#[cfg(target_os = "linux")]
fn through(
    iface: &lodestar_network_policy::iface::Interface,
    deny: Option<&crate::Deny>,
) -> Result<reqwest::ClientBuilder> {
    // Certificates are checked as every routed client checks them
    // (`lodestar_integrations::tls`), not by the OS verifier.
    let route = lodestar_integrations::Route::Interface(iface.name().to_owned());
    let tls = lodestar_integrations::tls::http_client_config(&route)
        .map_err(|e| Error::Http(e.to_string()))?;
    Ok(builder(None, deny, None)?
        .interface(iface.name())
        .dns_resolver(NoLookups)
        .tls_backend_preconfigured(tls))
}

#[cfg(not(target_os = "linux"))]
fn through(
    iface: &lodestar_network_policy::iface::Interface,
    _: Option<&crate::Deny>,
) -> Result<reqwest::ClientBuilder> {
    Err(Error::Http(format!(
        "HTTP trackers can't be limited to {iface} here"
    )))
}

/// Whether a [`client_through`] client may ask `url`'s tracker: on Linux,
/// one named by address (no lookups); elsewhere none.
pub fn through_reaches(url: &Url) -> bool {
    cfg!(target_os = "linux") && literal_ip(url).is_some()
}

/// [`client`]'s builder, with `roots` in place of the routed roots when
/// bound (tests pass their own CA).
fn builder(
    local: Option<IpAddr>,
    deny: Option<&crate::Deny>,
    roots: Option<std::sync::Arc<rustls::RootCertStore>>,
) -> Result<reqwest::ClientBuilder> {
    // Several clients may be built; only the first install wins, and any
    // provider installed by the host process is fine too.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .local_address(local)
        .user_agent(concat!("Lodestar/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10));
    if let Some(tls) = tls_for(local, roots)? {
        builder = builder.tls_backend_preconfigured(tls);
    }
    builder = match deny {
        None => builder.redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS)),
        Some(deny) => {
            let d = deny.clone();
            builder.dns_resolver(DenyResolver(deny.clone())).redirect(
                reqwest::redirect::Policy::custom(move |attempt| {
                    if attempt.previous().len() > MAX_REDIRECTS {
                        attempt.error("too many redirects")
                    } else if literal_ip(attempt.url()).is_some_and(|ip| d.denies(ip)) {
                        attempt.error("redirect to a blocklisted address")
                    } else {
                        attempt.follow()
                    }
                }),
            )
        }
    };
    Ok(builder)
}

const MAX_REDIRECTS: usize = 2;

/// The TLS configuration [`client`] is given: the bound route's when
/// `local` is set (over `roots`, or the routed roots), and reqwest's own
/// (`None`) when it isn't.
fn tls_for(
    local: Option<IpAddr>,
    roots: Option<std::sync::Arc<rustls::RootCertStore>>,
) -> Result<Option<rustls::ClientConfig>> {
    use lodestar_integrations::{Route, tls};
    let Some(ip) = local else {
        return Ok(None);
    };
    let route = Route::BindAddress(ip);
    match roots {
        Some(roots) => tls::http_client_config_over(&route, roots),
        None => tls::http_client_config(&route),
    }
    .map(Some)
    .map_err(|e| Error::Http(e.to_string()))
}

/// The host of `url` when it's an IP address rather than a name: the
/// resolver never sees those.
fn literal_ip(url: &Url) -> Option<IpAddr> {
    match url.host()? {
        ::url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        ::url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        ::url::Host::Domain(_) => None,
    }
}

/// X-vpn-bind: no name is looked up (the system resolver would ask
/// outside the crawl's interface). IP-literal URLs never reach a resolver,
/// so only names come here, and each is refused. Linux only: elsewhere the
/// pinned client is refused before it is built.
#[cfg(target_os = "linux")]
struct NoLookups;

#[cfg(target_os = "linux")]
impl reqwest::dns::Resolve for NoLookups {
    fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async { Err(Error::Resolve.into()) })
    }
}

/// The system resolver, minus blocklisted addresses.
struct DenyResolver(crate::Deny);

impl reqwest::dns::Resolve for DenyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let (deny, host) = (self.0.clone(), name.as_str().to_owned());
        Box::pin(async move {
            crate::looked_up();
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let allowed: Vec<SocketAddr> = all
                .iter()
                .copied()
                .filter(|a| !deny.denies(a.ip()))
                .collect();
            if allowed.is_empty() && !all.is_empty() {
                return Err(Error::Denied.into());
            }
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// `url` with one `info_hash` parameter per hash, every byte
/// percent-encoded. Parameters already in the URL (passkeys) are kept.
pub fn request_url(scrape_url: &Url, hashes: &[InfoHash]) -> Url {
    let mut query = scrape_url.query().unwrap_or_default().to_owned();
    for h in hashes {
        if !query.is_empty() {
            query.push('&');
        }
        query.push_str("info_hash=");
        for b in h.0 {
            query.push_str(&format!("%{b:02X}"));
        }
    }
    let mut url = scrape_url.clone();
    url.set_query(Some(&query));
    url
}

fn count(entry: &Dict<'_>, key: &[u8]) -> Option<u32> {
    match entry.get(key) {
        // Absent means the tracker didn't say; zero is the honest reading.
        None => Some(0),
        Some(v) => v
            .as_int()
            .and_then(|n| u32::try_from(n.max(0)).ok().or(Some(u32::MAX))),
    }
}

/// Parses a scrape reply for `hashes` (the request, in order). Entries for
/// hashes we didn't ask about, malformed entries and unknown keys are
/// ignored; a negative count reads as 0 and an over-large one as
/// `u32::MAX`.
pub fn parse_scrape(body: &[u8], hashes: &[InfoHash]) -> Result<Reply> {
    let value = bencode::decode_with(body, &DecodeOptions::lenient().with_limits(LIMITS))
        .map_err(|_| Error::Protocol("reply is not bencode"))?;
    let top = value
        .as_dict()
        .ok_or(Error::Protocol("reply is not a dictionary"))?;
    if let Some(reason) = top.get(b"failure reason") {
        return Err(Error::Refused(message(
            reason.as_bytes().unwrap_or_default(),
        )));
    }
    let files = top
        .get(b"files")
        .and_then(Value::as_dict)
        .ok_or(Error::Protocol("reply has no files dictionary"))?;
    let mut counts = vec![None; hashes.len()];
    for (key, entry) in files.iter() {
        let Some(entry) = entry.as_dict() else {
            continue;
        };
        for (i, h) in hashes.iter().enumerate() {
            if h.0[..] != *key {
                continue;
            }
            let parsed = (|| {
                Some(Counts {
                    seeders: count(entry, b"complete")?,
                    completed: count(entry, b"downloaded")?,
                    leechers: count(entry, b"incomplete")?,
                })
            })();
            counts[i] = parsed;
        }
    }
    let retry_after = top
        .get(b"flags")
        .and_then(Value::as_dict)
        .and_then(|f| f.get(b"min_request_interval"))
        .and_then(Value::as_int)
        .filter(|s| *s > 0)
        .map(|s| Duration::from_secs(s as u64).min(MAX_RETRY_AFTER));
    Ok(Reply {
        counts,
        retry_after,
    })
}

/// One HTTP tracker.
#[derive(Debug, Clone)]
pub struct HttpTracker {
    scrape_url: Url,
    cfg: HttpConfig,
    client: reqwest::Client,
}

impl HttpTracker {
    pub fn new(scrape_url: Url, cfg: HttpConfig, client: reqwest::Client) -> HttpTracker {
        HttpTracker {
            scrape_url,
            cfg,
            client,
        }
    }

    pub fn max_hashes(&self) -> usize {
        self.cfg.max_hashes.clamp(1, MAX_HASHES)
    }

    pub async fn scrape(&self, hashes: &[InfoHash]) -> Result<Reply> {
        check_batch(hashes, self.max_hashes())?;
        if literal_ip(&self.scrape_url).is_some_and(|ip| crate::denied(self.cfg.deny.as_ref(), ip))
        {
            return Err(Error::Denied);
        }
        let url = request_url(&self.scrape_url, hashes);
        let fetch = async {
            let mut resp = self.client.get(url).send().await.map_err(http_error)?;
            if resp.status() != reqwest::StatusCode::OK {
                return Err(Error::Status(resp.status().as_u16()));
            }
            if resp.content_length().is_some_and(|n| n > MAX_BODY as u64) {
                return Err(Error::TooLarge);
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(http_error)? {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(Error::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        };
        let body = tokio::time::timeout(self.cfg.timeout, fetch)
            .await
            .map_err(|_| Error::Timeout)??;
        parse_scrape(&body, hashes)
    }
}

fn http_error(e: reqwest::Error) -> Error {
    // The resolver's refusal comes back wrapped in connect errors.
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(err) = source {
        if matches!(err.downcast_ref::<Error>(), Some(Error::Denied)) {
            return Error::Denied;
        }
        source = err.source();
    }
    if e.is_timeout() {
        Error::Timeout
    } else {
        // Without the URL: it may hold a passkey.
        Error::Http(e.without_url().to_string())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_bound_client_verifies_as_routed_clients_do() {
        use lodestar_integrations::tls::verifies_self_contained;
        assert!(super::tls_for(None, None).unwrap().is_none());
        let bound = super::tls_for(Some("127.0.0.1".parse().unwrap()), None)
            .unwrap()
            .expect("a bound client gets the routed TLS configuration");
        assert!(verifies_self_contained(&bound));
        assert_eq!(bound.alpn_protocols, vec![b"http/1.1".to_vec()]);
        assert!(super::client(Some("127.0.0.1".parse().unwrap()), None).is_ok());
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/../integrations/tests/fixtures/email/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// A TLS server on 127.0.0.1 with the fixtures' `mail.test`
    /// certificate, answering each request with `200`.
    async fn test_ca_server() -> SocketAddr {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(fixture("server.der"))],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fixture("server.key.der"))),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut buf = [0u8; 4096];
                    let _ = tls.read(&mut buf).await;
                    let _ = tls
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_bound_client_trusts_its_roots_alone() {
        let addr = test_ca_server().await;
        let local = Some(IpAddr::from([127, 0, 0, 1]));
        let get = |roots| async move {
            let client = builder(local, None, roots)
                .unwrap()
                .resolve("mail.test", addr)
                .build()
                .unwrap();
            client
                .get(format!("https://mail.test:{}/", addr.port()))
                .send()
                .await
        };
        // Given the test CA, the bound client connects.
        let mut ours = rustls::RootCertStore::empty();
        ours.add(fixture("ca.der").into()).unwrap();
        let resp = get(Some(std::sync::Arc::new(ours))).await.unwrap();
        assert_eq!(resp.status(), 200);
        // With the routed roots, which don't hold that CA, the certificate
        // is refused.
        let err = get(None).await.unwrap_err();
        assert!(is_certificate_error(&err), "{err:?}");
    }

    fn is_certificate_error(e: &reqwest::Error) -> bool {
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
        while let Some(err) = source {
            let tls = err.downcast_ref::<rustls::Error>().or_else(|| {
                err.downcast_ref::<std::io::Error>()
                    .and_then(|io| io.get_ref())
                    .and_then(|i| i.downcast_ref::<std::io::Error>())
                    .and_then(|io| io.get_ref())
                    .and_then(|i| i.downcast_ref::<rustls::Error>())
            });
            if matches!(tls, Some(rustls::Error::InvalidCertificate(_))) {
                return true;
            }
            source = err.source();
        }
        false
    }

    use super::*;

    #[test]
    fn a_pinned_client_asks_only_trackers_it_can_keep_on_the_interface() {
        let reaches = |u: &str| through_reaches(&Url::parse(u).unwrap());
        assert_eq!(
            reaches("http://203.0.113.9/scrape"),
            cfg!(target_os = "linux")
        );
        assert!(
            !reaches("http://tracker.example/scrape"),
            "a name: no lookup"
        );
        assert_eq!(
            reaches("http://[2001:db8::66]/scrape"),
            cfg!(target_os = "linux")
        );
    }

    const A: InfoHash = InfoHash([0xaa; 20]);
    const B: InfoHash = InfoHash([0xbb; 20]);

    fn files(entries: &[(&[u8], &str)]) -> Vec<u8> {
        let mut out = b"d5:filesd".to_vec();
        for (k, v) in entries {
            out.extend_from_slice(format!("{}:", k.len()).as_bytes());
            out.extend_from_slice(k);
            out.extend_from_slice(v.as_bytes());
        }
        out.extend_from_slice(b"ee");
        out
    }

    #[test]
    fn parses_counts_in_request_order() {
        let body = files(&[
            (
                &A.0,
                "d8:completei5e10:downloadedi50e10:incompletei10e4:name1:xe",
            ),
            (&B.0, "d8:completei0e10:downloadedi0e10:incompletei0ee"),
            (&[0xcc; 20], "d8:completei9ee"),
        ]);
        let r = parse_scrape(&body, &[B, A, InfoHash([0xdd; 20])]).unwrap();
        assert_eq!(
            r.counts,
            [
                Some(Counts::default()),
                Some(Counts {
                    seeders: 5,
                    completed: 50,
                    leechers: 10
                }),
                None
            ]
        );
        assert_eq!(r.retry_after, None);
    }

    #[test]
    fn failure_reason_and_flags() {
        match parse_scrape(b"d14:failure reason12:rate limitede", &[A]) {
            Err(Error::Refused(m)) => assert_eq!(m, "rate limited"),
            other => panic!("{other:?}"),
        }
        let body = b"d5:filesde5:flagsd20:min_request_intervali900eee";
        let r = parse_scrape(body, &[A]).unwrap();
        assert_eq!(r.counts, [None]);
        assert_eq!(r.retry_after, Some(Duration::from_secs(900)));
        let huge = b"d5:filesde5:flagsd20:min_request_intervali99999999eee";
        assert_eq!(
            parse_scrape(huge, &[A]).unwrap().retry_after,
            Some(MAX_RETRY_AFTER)
        );
    }

    #[test]
    fn odd_values_are_clamped_or_skipped() {
        let body = files(&[(
            &A.0,
            "d8:completei-4e10:downloadedi99999999999e10:incomplete1:xe",
        )]);
        assert_eq!(parse_scrape(&body, &[A]).unwrap().counts, [None]);
        let body = files(&[(&A.0, "d8:completei-4e10:downloadedi99999999999ee")]);
        assert_eq!(
            parse_scrape(&body, &[A]).unwrap().counts,
            [Some(Counts {
                seeders: 0,
                completed: u32::MAX,
                leechers: 0
            })]
        );
        assert_eq!(
            parse_scrape(&files(&[(&A.0, "i1e")]), &[A]).unwrap().counts,
            [None]
        );
    }

    #[test]
    fn rejects_non_replies() {
        for body in [
            &b""[..],
            b"le",
            b"d4:spami1ee",
            b"<html>busy</html>",
            b"d5:filesi1ee",
        ] {
            assert!(parse_scrape(body, &[A]).is_err(), "{body:?}");
        }
        // Too deep for a scrape reply.
        assert!(parse_scrape(b"d5:filesd1:xd1:yd1:zdeeeee", &[A]).is_err());
    }

    #[test]
    fn request_urls_keep_existing_parameters() {
        let base = Url::parse("http://t.example/scrape.php?passkey=k").unwrap();
        let u = request_url(&base, &[InfoHash([0x00; 20]), InfoHash([0xff; 20])]);
        let q = u.query().unwrap();
        assert!(q.starts_with("passkey=k&info_hash=%00%00"), "{q}");
        assert_eq!(q.matches("info_hash=").count(), 2);
        assert!(q.ends_with(&"%FF".repeat(20)));
    }
}
