//! Which certificate verifier a connection uses, chosen by its [`Route`].
//!
//! - [`Route::Direct`] keeps the operating system's verifier
//!   (rustls-platform-verifier), with the OS's own trust decisions.
//! - Every other route (a bound address, a bound interface, a SOCKS5
//!   proxy) verifies with [`Verification::SelfContained`]: webpki in this
//!   process, over the bundled Mozilla roots plus the roots in the OS
//!   store, read once from local storage. Validation is a pure
//!   computation on the certificates the server sent; it opens no
//!   connection of its own. Revocation isn't checked on those routes.
//!
//! The plugin HTTP client ([`GuardedClient::with_route`]), the plugin
//! WebSocket client (`lodestar-sources`) and the IRC client
//! (`lodestar-sources-irc`) all choose through [`verification_for`].
//!
//! [`GuardedClient::with_route`]: crate::net::GuardedClient::with_route

use std::sync::{Arc, OnceLock};

use rustls::RootCertStore;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;

use crate::net::Route;

/// How a connection's server certificate is verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The operating system's verifier.
    Platform,
    /// webpki in this process over [`self_contained_roots`], with no
    /// revocation lists: nothing but the certificates the server sent is
    /// consulted.
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

/// The verifier for a connection over `route`, on `provider`.
pub fn verifier_for(
    route: &Route,
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, VerifierUnavailable> {
    match verification_for(route) {
        Verification::Platform => Ok(Arc::new(
            rustls_platform_verifier::Verifier::new(provider).map_err(|_| VerifierUnavailable)?,
        )),
        Verification::SelfContained => {
            Ok(self_contained_verifier(self_contained_store(), provider)?)
        }
    }
}

/// A [`Verification::SelfContained`] verifier over `roots`. Tests pass
/// their own CA; [`verifier_for`] passes [`self_contained_roots`].
pub fn self_contained_verifier(
    roots: Arc<RootCertStore>,
    provider: Arc<CryptoProvider>,
) -> Result<Arc<WebPkiServerVerifier>, VerifierUnavailable> {
    if roots.is_empty() {
        return Err(VerifierUnavailable);
    }
    // No `with_crls`: webpki then checks nothing beyond the chain itself.
    WebPkiServerVerifier::builder_with_provider(roots, provider)
        .build()
        .map_err(|_| VerifierUnavailable)
}

/// The roots [`Verification::SelfContained`] trusts: the bundled Mozilla
/// set and every root in the OS store that webpki can use, without
/// duplicates. Read once per process; an unreadable OS store leaves the
/// bundled set.
pub fn self_contained_roots() -> &'static [CertificateDer<'static>] {
    &roots().0
}

fn self_contained_store() -> Arc<RootCertStore> {
    Arc::clone(&roots().1)
}

type Roots = (Vec<CertificateDer<'static>>, Arc<RootCertStore>);

fn roots() -> &'static Roots {
    static ROOTS: OnceLock<Roots> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let bundled = webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned();
        // Reading the store is local: files on Linux, the system keychains
        // on macOS, the ROOT store on Windows.
        let native = rustls_native_certs::load_native_certs().certs;
        usable(bundled.chain(native))
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
    fn every_route_but_direct_verifies_self_contained() {
        assert_eq!(verification_for(&Route::Direct), wanted(&Route::Direct));
        for r in routed() {
            assert_eq!(wanted(&r), Verification::SelfContained, "{r:?}");
            assert_eq!(verification_for(&r), wanted(&r), "{r:?}");
            assert!(verifier_for(&r, provider()).is_ok(), "{r:?}");
        }
        assert!(verifier_for(&Route::Direct, provider()).is_ok());
    }

    #[test]
    fn guarded_clients_verify_by_route() {
        use crate::net::{GuardedClient, NetPolicy, SystemResolver};
        let build = |r: &Route| {
            GuardedClient::with_route(
                NetPolicy::download_client(),
                Duration::from_secs(10),
                Arc::new(SystemResolver),
                r,
            )
        };
        let direct = build(&Route::Direct).unwrap();
        assert_eq!(direct.verification(), Verification::Platform);
        assert!(crate::net::routed_roots(&Route::Direct).unwrap().is_none());
        for r in routed() {
            // The roots handed to reqwest's `tls_certs_only`: the client's
            // only trust on every route but the direct one.
            let roots = crate::net::routed_roots(&r).unwrap();
            assert_eq!(wanted(&r), Verification::SelfContained, "{r:?}");
            assert_eq!(
                roots.map(|v| v.len()),
                Some(self_contained_roots().len()),
                "{r:?}"
            );
            match build(&r) {
                Ok(c) => assert_eq!(c.verification(), Verification::SelfContained, "{r:?}"),
                // A bound interface is Linux only.
                Err(crate::net::RouteError::Unsupported(_)) => {
                    assert!(matches!(r, Route::Interface(_)))
                }
                Err(e) => panic!("{r:?}: {e}"),
            }
        }
    }

    #[test]
    fn the_roots_include_the_bundled_set_once_each() {
        let roots = self_contained_roots();
        assert!(roots.len() >= webpki_root_certs::TLS_SERVER_ROOT_CERTS.len() / 2);
        for b in webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().take(5) {
            assert_eq!(roots.iter().filter(|r| r.as_ref() == b.as_ref()).count(), 1);
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
        // The bundled and OS roots don't trust the test CA.
        assert!(matches!(
            verify(self_contained_store(), "mail.test"),
            Err(rustls::Error::InvalidCertificate(_))
        ));
    }
}
