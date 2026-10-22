//! Mutual TLS for the cluster port (§19.2, §19.6): TLS 1.3 only, on the
//! ring provider, trusting the cluster CA and nothing else. The system
//! roots are never loaded.
//!
//! The port speaks two ALPN protocols. The coordinator reads the
//! ClientHello first (rustls' `Acceptor`) and picks the configuration with
//! [`CoordinatorTls::config_for`]:
//! - [`ALPN_CLUSTER`]: a worker certificate is required. It must chain to
//!   a trusted cluster CA, be one the coordinator issued, and not be
//!   revoked. This is checked on every handshake, against the live
//!   revocation list.
//! - [`ALPN_JOIN`]: no client certificate (the worker has none yet). The
//!   only thing such a connection may do is redeem a join token. The worker
//!   checks the coordinator against the CA fingerprint in its token.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::{ClientHello, ResolvesServerCert, WebPkiClientVerifier};
use rustls::sign::CertifiedKey;
use rustls::time_provider::{DefaultTimeProvider, TimeProvider};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, Error, RootCertStore,
    ServerConfig, ServerConnection, SignatureScheme,
};

use crate::{ClusterError, Fingerprint, KeyPair, WorkerId};

pub const ALPN_CLUSTER: &[u8] = b"lodestar-cluster/1";
pub const ALPN_JOIN: &[u8] = b"lodestar-join/1";

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn algorithms() -> WebPkiSupportedAlgorithms {
    provider().signature_verification_algorithms
}

/// The name workers verify the coordinator's certificate against,
/// whatever address they dialled.
pub fn coordinator_server_name() -> ServerName<'static> {
    ServerName::try_from(crate::x509::COORDINATOR_NAME).expect("a valid DNS name")
}

pub(crate) fn signing_key(
    key: &KeyPair,
) -> Result<Arc<dyn rustls::sign::SigningKey>, ClusterError> {
    let der = PrivateKeyDer::Pkcs8(key.pkcs8().to_vec().into());
    Ok(provider().key_provider.load_private_key(der)?)
}

fn roots(cas: &[CertificateDer<'static>]) -> Result<Arc<RootCertStore>, ClusterError> {
    let mut store = RootCertStore::empty();
    for ca in cas {
        store.add(ca.clone())?;
    }
    Ok(Arc::new(store))
}

/// A certificate the coordinator issued, as the handshake check sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issued {
    pub serial: [u8; 16],
    pub worker: WorkerId,
    pub name: String,
    pub issuer: Fingerprint,
    pub not_after: i64,
    pub revoked: bool,
}

/// Who a verified cluster connection belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub worker: WorkerId,
    pub name: String,
    pub serial: [u8; 16],
    /// The certificate is from a CA that's being rotated out: ask the
    /// worker to renew now rather than at half-life.
    pub renew_now: bool,
}

/// What every handshake reads: the trusted CAs and the issued
/// certificates, keyed by fingerprint, revocations included.
pub(crate) struct Trust {
    pub current_ca: Fingerprint,
    pub issued: HashMap<Fingerprint, Issued>,
    webpki: Arc<dyn ClientCertVerifier>,
}

impl Trust {
    pub fn new(
        cas: Vec<CertificateDer<'static>>,
        current_ca: Fingerprint,
        issued: HashMap<Fingerprint, Issued>,
    ) -> Result<Trust, ClusterError> {
        let webpki = WebPkiClientVerifier::builder_with_provider(roots(&cas)?, provider())
            .build()
            .map_err(|e| ClusterError::Verifier(e.to_string()))?;
        Ok(Trust {
            current_ca,
            issued,
            webpki,
        })
    }
}

pub(crate) struct Shared {
    pub trust: RwLock<Trust>,
    pub server: RwLock<Arc<CertifiedKey>>,
}

/// The coordinator's TLS side: cheap to clone, shared with the listener
/// tasks. Issuing, renewing, revoking and rotating through
/// [`Coordinator`](crate::Coordinator) take effect on the next handshake.
#[derive(Clone)]
pub struct CoordinatorTls {
    shared: Arc<Shared>,
    cluster: Arc<ServerConfig>,
    join: Arc<ServerConfig>,
}

impl std::fmt::Debug for CoordinatorTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CoordinatorTls")
    }
}

impl CoordinatorTls {
    pub(crate) fn new(
        shared: Arc<Shared>,
        time: Arc<dyn TimeProvider>,
    ) -> Result<CoordinatorTls, ClusterError> {
        let resolver = Arc::new(ServerKey(shared.clone()));
        let builder = || {
            ServerConfig::builder_with_details(provider(), time.clone())
                .with_protocol_versions(&[&rustls::version::TLS13])
        };
        let mut cluster = builder()?
            .with_client_cert_verifier(Arc::new(ClusterClients(shared.clone())))
            .with_cert_resolver(resolver.clone());
        cluster.alpn_protocols = vec![ALPN_CLUSTER.to_vec()];
        let mut join = builder()?
            .with_no_client_auth()
            .with_cert_resolver(resolver);
        join.alpn_protocols = vec![ALPN_JOIN.to_vec()];
        Ok(CoordinatorTls {
            shared,
            cluster: Arc::new(cluster),
            join: Arc::new(join),
        })
    }

    /// Uses `time` instead of the system clock for certificate validity.
    pub fn with_time_provider(self, time: Arc<dyn TimeProvider>) -> Result<Self, ClusterError> {
        CoordinatorTls::new(self.shared, time)
    }

    /// The configuration for a connection whose ClientHello offered `hello`'s
    /// ALPN list, or `None` to refuse it: a client must ask for exactly one
    /// of the two cluster protocols.
    pub fn config_for(&self, hello: &ClientHello<'_>) -> Option<Arc<ServerConfig>> {
        let mut offered: Vec<&[u8]> = hello.alpn()?.collect();
        offered.dedup();
        match offered.as_slice() {
            [p] if *p == ALPN_CLUSTER => Some(self.cluster.clone()),
            [p] if *p == ALPN_JOIN => Some(self.join.clone()),
            _ => None,
        }
    }

    pub fn cluster_config(&self) -> Arc<ServerConfig> {
        self.cluster.clone()
    }

    pub fn join_config(&self) -> Arc<ServerConfig> {
        self.join.clone()
    }

    /// The worker behind a completed cluster handshake, re-checked against
    /// the revocation list. `None` for a join connection or a certificate
    /// revoked since the handshake.
    pub fn identify(&self, conn: &ServerConnection) -> Option<PeerIdentity> {
        if conn.alpn_protocol() != Some(ALPN_CLUSTER) {
            return None;
        }
        let cert = conn.peer_certificates()?.first()?;
        let trust = self
            .shared
            .trust
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let issued = trust.issued.get(&Fingerprint::of(cert))?;
        (!issued.revoked).then(|| PeerIdentity {
            worker: issued.worker,
            name: issued.name.clone(),
            serial: issued.serial,
            renew_now: issued.issuer != trust.current_ca,
        })
    }
}

/// Serves the coordinator's current certificate, so a renewal or a CA
/// switch needs no new `ServerConfig`.
#[derive(Debug)]
struct ServerKey(Arc<Shared>);

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shared")
    }
}

impl ResolvesServerCert for ServerKey {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(
            self.0
                .server
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        )
    }
}

/// The coordinator's check of a worker's certificate.
#[derive(Debug)]
struct ClusterClients(Arc<Shared>);

impl ClientCertVerifier for ClusterClients {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        let trust = self.0.trust.read().unwrap_or_else(PoisonError::into_inner);
        // Chain to a trusted cluster CA, clientAuth EKU, validity window.
        trust
            .webpki
            .verify_client_cert(end_entity, intermediates, now)?;
        // Then the issued-certificate list: a certificate the coordinator
        // doesn't know (one signed by a CA key that leaked, or whose record
        // is gone) is refused like a revoked one.
        match trust.issued.get(&Fingerprint::of(end_entity)) {
            Some(issued) if !issued.revoked => Ok(ClientCertVerified::assertion()),
            Some(_) => Err(Error::InvalidCertificate(CertificateError::Revoked)),
            None => Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            )),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::General("the cluster port is TLS 1.3 only".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &algorithms())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        algorithms().supported_schemes()
    }
}

/// How a worker recognises the coordinator.
#[derive(Debug)]
enum CoordinatorPin {
    /// After joining: the CA bundle it was given.
    Roots(Arc<WebPkiServerVerifier>),
    /// While joining: the CA whose fingerprint is in the token. The
    /// coordinator sends that CA's certificate in its chain; nothing else
    /// is trusted.
    Fingerprint(Fingerprint),
}

#[derive(Debug)]
struct CoordinatorVerifier(CoordinatorPin);

impl ServerCertVerifier for CoordinatorVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let name = coordinator_server_name();
        match &self.0 {
            CoordinatorPin::Roots(webpki) => {
                webpki.verify_server_cert(end_entity, intermediates, &name, ocsp, now)
            }
            CoordinatorPin::Fingerprint(fp) => {
                let ca = intermediates
                    .iter()
                    .find(|c| Fingerprint::of(c) == *fp)
                    .ok_or(Error::InvalidCertificate(CertificateError::UnknownIssuer))?;
                let webpki = server_verifier(&[ca.clone().into_owned()])
                    .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
                webpki.verify_server_cert(end_entity, &[], &name, ocsp, now)
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::General("the cluster port is TLS 1.3 only".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &algorithms())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        algorithms().supported_schemes()
    }
}

fn server_verifier(
    cas: &[CertificateDer<'static>],
) -> Result<Arc<WebPkiServerVerifier>, ClusterError> {
    WebPkiServerVerifier::builder_with_provider(roots(cas)?, provider())
        .build()
        .map_err(|e| ClusterError::Verifier(e.to_string()))
}

fn client_builder(
    pin: CoordinatorPin,
    time: Option<Arc<dyn TimeProvider>>,
) -> Result<rustls::ConfigBuilder<ClientConfig, rustls::client::WantsClientCert>, ClusterError> {
    Ok(ClientConfig::builder_with_details(
        provider(),
        time.unwrap_or_else(|| Arc::new(DefaultTimeProvider)),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(CoordinatorVerifier(pin))))
}

/// A worker's configuration for the join connection.
pub(crate) fn join_client_config(
    ca: Fingerprint,
    time: Option<Arc<dyn TimeProvider>>,
) -> Result<Arc<ClientConfig>, ClusterError> {
    let mut config = client_builder(CoordinatorPin::Fingerprint(ca), time)?.with_no_client_auth();
    config.alpn_protocols = vec![ALPN_JOIN.to_vec()];
    Ok(Arc::new(config))
}

/// A worker's configuration for cluster connections.
pub(crate) fn cluster_client_config(
    cas: &[CertificateDer<'static>],
    cert: CertificateDer<'static>,
    key: &KeyPair,
    time: Option<Arc<dyn TimeProvider>>,
) -> Result<Arc<ClientConfig>, ClusterError> {
    let pin = CoordinatorPin::Roots(server_verifier(cas)?);
    let der = PrivateKeyDer::Pkcs8(key.pkcs8().to_vec().into());
    let mut config = client_builder(pin, time)?.with_client_auth_cert(vec![cert], der)?;
    config.alpn_protocols = vec![ALPN_CLUSTER.to_vec()];
    Ok(Arc::new(config))
}
