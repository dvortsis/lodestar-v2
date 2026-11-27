//! Which certificate verifier a connection uses, chosen by its [`Route`].
//!
//! - [`Route::Direct`] keeps the operating system's verifier
//!   (rustls-platform-verifier), with the OS's own trust decisions.
//! - Every other route (a bound address, a bound interface, a SOCKS5
//!   proxy) verifies with [`Verification::SelfContained`]: webpki over
//!   the OS-store roots, with no revocation lists.
//!
//! [`client_config`] builds the TLS configuration for a route, and every
//! client that takes a route uses it: the plugin HTTP client
//! ([`GuardedClient::with_route`]), the plugin WebSocket client
//! (`lodestar-sources`), the IRC client (`lodestar-sources-irc`) and the
//! tracker scrape client when it is bound (`lodestar-tracker-scrape`).
//!
//! [`GuardedClient::with_route`]: crate::net::GuardedClient::with_route

use std::sync::{Arc, OnceLock};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

use crate::net::Route;

/// How a connection's server certificate is verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The operating system's verifier.
    Platform,
    /// webpki over the OS-store roots ([`self_contained_roots`]), with no
    /// revocation lists.
    SelfContained,
}

/// The verification a connection over `route` uses.
pub fn verification_for(route: &Route) -> Verification {
    match route {
        Route::Direct => Verification::Platform,
        Route::BindAddress(_) | Route::Interface(_) | Route::Socks5(_) => {
            Verification::SelfContained
        }
    }
}

/// The verifier could not be set up (no usable roots, or the OS verifier
/// failed to start).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifierUnavailable;

impl std::fmt::Display for VerifierUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TLS certificate verification couldn't be set up")
    }
}

impl std::error::Error for VerifierUnavailable {}

/// The TLS client configuration for a connection over `route`: rustls on
/// ring, TLS 1.2 and 1.3, no client certificate, no ALPN, verifying as
/// [`verification_for`] says.
pub fn client_config(route: &Route) -> Result<ClientConfig, VerifierUnavailable> {
    client_config_over(route, self_contained_store())
}

/// [`client_config`] with `roots` in place of [`self_contained_roots`]
/// for [`Verification::SelfContained`] (tests pass their own CA, or none).
pub fn client_config_over(
    route: &Route,
    roots: Arc<RootCertStore>,
) -> Result<ClientConfig, VerifierUnavailable> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier: Arc<dyn ServerCertVerifier> = match verification_for(route) {
        Verification::Platform => Arc::new(
            rustls_platform_verifier::Verifier::new(Arc::clone(&provider))
                .map_err(|_| VerifierUnavailable)?,
        ),
        Verification::SelfContained => self_contained_verifier(roots, Arc::clone(&provider))?,
    };
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| VerifierUnavailable)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

/// [`client_config`] for an HTTP/1.1 client (reqwest): the same, with
/// ALPN offering `http/1.1`, as reqwest sets it itself.
pub fn http_client_config(route: &Route) -> Result<ClientConfig, VerifierUnavailable> {
    http_client_config_over(route, self_contained_store())
}

/// [`http_client_config`] over `roots`, as [`client_config_over`].
pub fn http_client_config_over(
    route: &Route,
    roots: Arc<RootCertStore>,
) -> Result<ClientConfig, VerifierUnavailable> {
    let mut config = client_config_over(route, roots)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// A [`Verification::SelfContained`] verifier over `roots`; none is an
/// error.
pub fn self_contained_verifier(
    roots: Arc<RootCertStore>,
    provider: Arc<CryptoProvider>,
) -> Result<Arc<WebPkiServerVerifier>, VerifierUnavailable> {
    if roots.is_empty() {
        return Err(VerifierUnavailable);
    }
    // No `with_crls`: no revocation lists.
    WebPkiServerVerifier::builder_with_provider(roots, provider)
        .build()
        .map_err(|_| VerifierUnavailable)
}

/// The roots [`Verification::SelfContained`] trusts: every root in the
/// OS store that webpki can use, each once. Read once per process; with
/// none, no routed client or connector is built. This blocks on first
/// use, so async code calls [`load_roots`] first.
pub fn self_contained_roots() -> &'static [CertificateDer<'static>] {
    &roots().0
}

/// Reads [`self_contained_roots`] on a blocking thread, once, so the
/// first routed client doesn't read the OS store on an async worker.
pub async fn load_roots() {
    if ROOTS.get().is_none() {
        let _ = tokio::task::spawn_blocking(roots).await;
    }
}

/// Whether `config` verifies with webpki itself, as
/// [`Verification::SelfContained`] does, and not through the OS verifier
/// (which on Linux wraps one). For tests in the crates that build
/// connectors: it reads the verifier `config`'s `Debug` names.
#[doc(hidden)]
pub fn verifies_self_contained(config: &ClientConfig) -> bool {
    format!("{config:?}").contains("verifier: WebPkiServerVerifier {")
}

fn self_contained_store() -> Arc<RootCertStore> {
    Arc::clone(&roots().1)
}

type Roots = (Vec<CertificateDer<'static>>, Arc<RootCertStore>);

static ROOTS: OnceLock<Roots> = OnceLock::new();

fn roots() -> &'static Roots {
    ROOTS.get_or_init(|| {
        // Files on Linux, the system keychains on macOS, the ROOT store on
        // Windows.
        let native = rustls_native_certs::load_native_certs().certs;
        usable(native)
    })
}

/// The certificates webpki accepts as trust anchors, each once.
fn usable(certs: impl IntoIterator<Item = CertificateDer<'static>>) -> Roots {
    let mut kept: Vec<CertificateDer<'static>> = Vec::new();
    let mut store = RootCertStore::empty();
    for c in certs {
        if kept.iter().any(|k| k.as_ref() == c.as_ref()) {
            continue;
        }
        if store.add(c.clone()).is_ok() {
            kept.push(c);
        }
    }
    (kept, Arc::new(store))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::{Duration, SystemTime};

    use rustls::pki_types::{ServerName, UnixTime};

    use super::*;
    use crate::net::{Socks5Route, SocksDns};

    fn fixture(name: &str) -> CertificateDer<'static> {
        let path = format!("{}/tests/fixtures/email/{name}", env!("CARGO_MANIFEST_DIR"));
        CertificateDer::from(std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}")))
    }

    fn provider() -> Arc<CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    fn routed() -> Vec<Route> {
        let proxy = SocketAddr::from((Ipv4Addr::LOCALHOST, 1080));
        vec![
            Route::BindAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            Route::Interface("wg0".into()),
            Route::Socks5(Socks5Route {
                proxy,
                dns: SocksDns::Proxy,
                auth: None,
            }),
            Route::Socks5(Socks5Route {
                proxy,
                dns: SocksDns::Local,
                auth: None,
            }),
        ]
    }

    /// What each route must verify with. No wildcard arm: a new `Route`
    /// variant fails to compile here until it is given its verification.
    fn wanted(route: &Route) -> Verification {
        match route {
            Route::Direct => Verification::Platform,
            Route::BindAddress(_) => Verification::SelfContained,
            Route::Interface(_) => Verification::SelfContained,
            Route::Socks5(_) => Verification::SelfContained,
        }
    }

    #[test]
    fn every_route_but_direct_gets_a_webpki_config() {
        assert_eq!(verification_for(&Route::Direct), wanted(&Route::Direct));
        assert!(!verifies_self_contained(
            &client_config(&Route::Direct).unwrap()
        ));
        for r in routed() {
            assert_eq!(wanted(&r), Verification::SelfContained, "{r:?}");
            assert_eq!(verification_for(&r), wanted(&r), "{r:?}");
            assert!(
                verifies_self_contained(&client_config(&r).unwrap()),
                "{r:?}"
            );
            let http = http_client_config(&r).unwrap();
            assert!(verifies_self_contained(&http), "{r:?}");
            assert_eq!(http.alpn_protocols, vec![b"http/1.1".to_vec()]);
        }
    }

    #[test]
    fn no_roots_no_config() {
        for r in routed() {
            let none = Arc::new(RootCertStore::empty());
            assert_eq!(
                client_config_over(&r, none).err(),
                Some(VerifierUnavailable),
                "{r:?}"
            );
        }
    }

    #[test]
    fn guarded_clients_verify_by_route() {
        use crate::net::{GuardedClient, NetPolicy, RouteError, SystemResolver};
        let build = |r: &Route, roots: Option<Arc<RootCertStore>>| {
            GuardedClient::with_route_trusting(
                NetPolicy::download_client(),
                Duration::from_secs(10),
                Arc::new(SystemResolver),
                r,
                roots,
            )
        };
        let direct = build(&Route::Direct, None).unwrap();
        assert_eq!(direct.verification(), Verification::Platform);
        for r in routed() {
            match build(&r, None) {
                Ok(c) => assert_eq!(c.verification(), Verification::SelfContained, "{r:?}"),
                // A bound interface is Linux only.
                Err(RouteError::Unsupported(_)) => assert!(matches!(r, Route::Interface(_))),
                Err(e) => panic!("{r:?}: {e}"),
            }
            // A routed client with no roots isn't built, as ws and IRC
            // connectors aren't.
            match build(&r, Some(Arc::new(RootCertStore::empty()))) {
                Err(RouteError::Build) => {}
                Err(RouteError::Unsupported(_)) => assert!(matches!(r, Route::Interface(_))),
                other => panic!("{r:?}: {:?}", other.map(|_| ())),
            }
        }
    }

    /// A TLS server on 127.0.0.1 with the email fixtures' certificate
    /// (`mail.test`, signed by the fixtures' CA), answering each request
    /// with `200 ok`.
    async fn test_ca_server() -> u16 {
        use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let key = std::fs::read(format!(
            "{}/tests/fixtures/email/server.key.der",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let config = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![fixture("server.der")],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
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
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        port
    }

    /// `mail.test` is 127.0.0.1.
    struct MailTest;

    impl crate::net::Resolve for MailTest {
        fn resolve<'a>(&'a self, _: &'a str) -> crate::BoxFuture<'a, std::io::Result<Vec<IpAddr>>> {
            Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
        }
    }

    async fn get(
        client: &crate::net::GuardedClient,
        port: u16,
    ) -> Result<u16, crate::net::NetError> {
        let url = format!("https://mail.test:{port}/").parse().unwrap();
        let req = client.request(reqwest::Method::GET, url).build().unwrap();
        client.send(req).await.map(|r| r.status().as_u16())
    }

    #[tokio::test]
    async fn a_routed_http_client_trusts_its_roots_alone() {
        use crate::net::{GuardedClient, NetError, NetPolicy};
        let port = test_ca_server().await;
        let bound = Route::BindAddress(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let client = |roots| {
            GuardedClient::with_route_trusting(
                NetPolicy::download_client(),
                Duration::from_secs(10),
                Arc::new(MailTest),
                &bound,
                roots,
            )
            .unwrap()
        };
        // Given the test CA as its roots, the routed client connects: the
        // OS verifier wouldn't trust that CA.
        let (_, ours) = usable([fixture("ca.der")]);
        assert_eq!(get(&client(Some(ours)), port).await, Ok(200));
        // With the OS-store roots it refuses the certificate, and
        // says so.
        assert_eq!(get(&client(None), port).await, Err(NetError::Certificate));
        assert_eq!(
            NetError::Certificate.to_string(),
            "the server's certificate isn't trusted"
        );
    }

    #[test]
    fn the_roots_are_the_os_stores_once_each() {
        let roots = self_contained_roots();
        assert!(
            !roots.is_empty(),
            "the OS store has roots on every CI runner"
        );
        for (i, r) in roots.iter().enumerate() {
            assert!(!roots[..i].iter().any(|k| k.as_ref() == r.as_ref()));
        }
        assert_eq!(self_contained_store().len(), roots.len());
    }

    #[test]
    fn unusable_and_duplicate_roots_are_dropped() {
        let ca = fixture("ca.der");
        let (kept, store) = usable([ca.clone(), CertificateDer::from(vec![1, 2, 3]), ca]);
        assert_eq!(kept.len(), 1);
        assert_eq!(store.len(), 1);
        assert!(
            self_contained_verifier(Arc::new(RootCertStore::empty()), provider()).is_err(),
            "no roots, no verifier"
        );
    }

    fn verify(roots: Arc<RootCertStore>, name: &str) -> Result<(), rustls::Error> {
        let v = self_contained_verifier(roots, provider()).unwrap();
        let now = UnixTime::since_unix_epoch(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO),
        );
        let name = ServerName::try_from(name.to_owned()).unwrap();
        v.verify_server_cert(&fixture("server.der"), &[], &name, &[], now)
            .map(|_| ())
    }

    #[test]
    fn the_verifier_checks_chain_and_name() {
        let (_, ours) = usable([fixture("ca.der")]);
        assert!(verify(Arc::clone(&ours), "mail.test").is_ok());
        assert!(matches!(
            verify(ours, "other.test"),
            Err(rustls::Error::InvalidCertificate(_))
        ));
        // The OS roots don't trust the test CA.
        assert!(matches!(
            verify(self_contained_store(), "mail.test"),
            Err(rustls::Error::InvalidCertificate(_))
        ));
    }
}
