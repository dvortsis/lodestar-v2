//! The PKI end to end, over real rustls handshakes in memory: joining,
//! mutual TLS, and every refusal (expired and reused tokens, a wrong CA, a
//! man in the middle, revoked, expired and unknown certificates, clock
//! skew, TLS 1.2, a missing certificate, rotation and restarts).

use std::sync::Arc;
use std::time::Duration;

use lodestar_integrations::SecretStore;
use lodestar_store::{Store, StoreConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::Acceptor;
use rustls::time_provider::TimeProvider;
use rustls::{
    CertificateError, ClientConfig, ClientConnection, Error, ServerConfig, ServerConnection,
};

use crate::x509::{self, Role, Spec};
use crate::*;

const T0: i64 = 1_800_000_000;

#[derive(Debug)]
struct At(i64);

impl TimeProvider for At {
    fn current_time(&self) -> Option<UnixTime> {
        Some(UnixTime::since_unix_epoch(Duration::from_secs(
            self.0 as u64,
        )))
    }
}

/// A clock tests can move, for one long-lived configuration.
#[derive(Debug)]
struct Clock(std::sync::atomic::AtomicI64);

impl Clock {
    fn set(&self, t: i64) {
        self.0.store(t, std::sync::atomic::Ordering::SeqCst);
    }
}

impl TimeProvider for Clock {
    fn current_time(&self) -> Option<UnixTime> {
        let t = self.0.load(std::sync::atomic::Ordering::SeqCst);
        Some(UnixTime::since_unix_epoch(Duration::from_secs(t as u64)))
    }
}

fn at(t: i64) -> Option<Arc<dyn TimeProvider>> {
    Some(Arc::new(At(t)))
}

struct Cluster {
    store: Store,
    secrets: SecretStore,
    coord: Coordinator,
}

fn cluster(seed: u8) -> Cluster {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let secrets = SecretStore::from_key(&[seed; 32]);
    let coord = Coordinator::open(&mut store, &secrets, T0).unwrap();
    Cluster {
        store,
        secrets,
        coord,
    }
}

impl Cluster {
    fn token(&mut self, name: &str, now: i64) -> JoinToken {
        let token = self
            .coord
            .create_join_token(&mut self.store, name, now)
            .unwrap();
        // As the worker gets it: pasted text.
        JoinToken::parse(&token.expose()).unwrap()
    }

    fn join(&mut self, name: &str, now: i64) -> Credentials {
        let joining = Joining::new(self.token(name, now)).unwrap();
        // The join channel itself: the worker pins the coordinator by the
        // token's fingerprint.
        let outcome = handshake(&self.tls(now), joining.client_config(at(now)).unwrap());
        assert!(matches!(outcome, Outcome::Done(None)), "{outcome:?}");
        let grant = self
            .coord
            .redeem(&mut self.store, joining.token(), &joining.public_key(), now)
            .unwrap();
        joining.finish(grant, now).unwrap()
    }

    fn tls(&self, now: i64) -> CoordinatorTls {
        self.coord
            .tls()
            .with_time_provider(Arc::new(At(now)))
            .unwrap()
    }

    fn connect(&self, creds: &Credentials, now: i64) -> Outcome {
        self.connect_skewed(creds, now, now)
    }

    fn connect_skewed(&self, creds: &Credentials, coord_now: i64, worker_now: i64) -> Outcome {
        handshake(
            &self.tls(coord_now),
            creds.client_config(at(worker_now)).unwrap(),
        )
    }

    fn reopen(self, now: i64) -> Cluster {
        let Cluster {
            mut store, secrets, ..
        } = self;
        let coord = Coordinator::open(&mut store, &secrets, now).unwrap();
        Cluster {
            store,
            secrets,
            coord,
        }
    }
}

#[derive(Debug)]
enum Outcome {
    /// The handshake completed; the coordinator's view of the peer.
    Done(Option<PeerIdentity>),
    /// The coordinator found no configuration for the ClientHello.
    NoConfig,
    Server(Error),
    Client(Error),
}

fn handshake(tls: &CoordinatorTls, client: Arc<ClientConfig>) -> Outcome {
    let mut client = ClientConnection::new(client, coordinator_server_name()).unwrap();
    let mut acceptor = Acceptor::default();
    let accepted = loop {
        let mut buf = Vec::new();
        client.write_tls(&mut buf).unwrap();
        acceptor.read_tls(&mut buf.as_slice()).unwrap();
        match acceptor.accept() {
            Ok(Some(accepted)) => break accepted,
            Ok(None) => continue,
            Err((e, _)) => return Outcome::Server(e),
        }
    };
    let Some(config) = tls.config_for(&accepted.client_hello()) else {
        return Outcome::NoConfig;
    };
    let mut server = match accepted.into_connection(config) {
        Ok(server) => server,
        Err((e, _)) => return Outcome::Server(e),
    };
    for _ in 0..16 {
        if let Err(e) = transfer_to_client(&mut server, &mut client) {
            return Outcome::Client(e);
        }
        if let Err(e) = transfer_to_server(&mut client, &mut server) {
            return Outcome::Server(e);
        }
        if !server.is_handshaking() && !client.is_handshaking() {
            // Deliver anything sent after the handshake (session tickets,
            // if the server offered any), so a reused client config would
            // try to resume next time.
            if let Err(e) = transfer_to_client(&mut server, &mut client) {
                return Outcome::Client(e);
            }
            assert_eq!(server.handshake_kind(), Some(rustls::HandshakeKind::Full));
            return Outcome::Done(tls.identify(&server));
        }
    }
    panic!("handshake didn't finish");
}

fn transfer_to_client(
    server: &mut ServerConnection,
    client: &mut ClientConnection,
) -> Result<(), Error> {
    let mut buf = Vec::new();
    while server.wants_write() {
        server.write_tls(&mut buf).unwrap();
    }
    let mut slice = buf.as_slice();
    while !slice.is_empty() {
        client.read_tls(&mut slice).unwrap();
        client.process_new_packets()?;
    }
    Ok(())
}

fn transfer_to_server(
    client: &mut ClientConnection,
    server: &mut ServerConnection,
) -> Result<(), Error> {
    let mut buf = Vec::new();
    while client.wants_write() {
        client.write_tls(&mut buf).unwrap();
    }
    let mut slice = buf.as_slice();
    while !slice.is_empty() {
        server.read_tls(&mut slice).unwrap();
        server.process_new_packets()?;
    }
    Ok(())
}

/// A plain rustls server, for impostors.
fn handshake_with(server: ServerConfig, client: Arc<ClientConfig>) -> Outcome {
    let mut client = ClientConnection::new(client, coordinator_server_name()).unwrap();
    let mut server = ServerConnection::new(Arc::new(server)).unwrap();
    for _ in 0..16 {
        if let Err(e) = transfer_to_server(&mut client, &mut server) {
            return Outcome::Server(e);
        }
        if let Err(e) = transfer_to_client(&mut server, &mut client) {
            return Outcome::Client(e);
        }
        if !server.is_handshaking() && !client.is_handshaking() {
            return Outcome::Done(None);
        }
    }
    panic!("handshake didn't finish");
}

fn impostor(chain: Vec<CertificateDer<'static>>, key: &KeyPair, alpn: &[u8]) -> ServerConfig {
    let mut config = ServerConfig::builder_with_provider(crate::tls::provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, PrivateKeyDer::Pkcs8(key.pkcs8().to_vec().into()))
        .unwrap();
    config.alpn_protocols = vec![alpn.to_vec()];
    config
}

fn self_signed_ca(now: i64) -> (KeyPair, String, CertificateDer<'static>) {
    let key = KeyPair::generate().unwrap();
    let name = "Impostor CA".to_owned();
    let spec = Spec {
        role: Role::Ca,
        subject: &name,
        issuer: &name,
        public_key: &key.public_key(),
        not_before: now - 60,
        not_after: now + 86_400,
    };
    let cert = x509::certificate(&spec, &x509::random_serial().unwrap(), &key);
    (key, name, cert.into())
}

fn leaf(
    role: Role<'_>,
    issuer: &str,
    issuer_key: &KeyPair,
    now: i64,
) -> (KeyPair, CertificateDer<'static>) {
    let key = KeyPair::generate().unwrap();
    let spec = Spec {
        role,
        subject: "leaf",
        issuer,
        public_key: &key.public_key(),
        not_before: now - 60,
        not_after: now + 86_400,
    };
    let cert = x509::certificate(&spec, &x509::random_serial().unwrap(), issuer_key);
    (key, cert.into())
}

/// The certificate refusal, with rustls' time context dropped.
fn refused_cert(outcome: &Outcome) -> Option<CertificateError> {
    match outcome {
        Outcome::Server(Error::InvalidCertificate(e))
        | Outcome::Client(Error::InvalidCertificate(e)) => Some(match e {
            CertificateError::ExpiredContext { .. } => CertificateError::Expired,
            CertificateError::NotValidYetContext { .. } => CertificateError::NotValidYet,
            other => other.clone(),
        }),
        _ => None,
    }
}

#[test]
fn a_joined_worker_connects_with_mutual_tls() {
    let mut c = cluster(1);
    let creds = c.join("nas", T0);
    assert_eq!(creds.name(), "nas");
    assert_eq!(creds.worker(), WorkerId(1));
    let Outcome::Done(Some(peer)) = c.connect(&creds, T0 + 10) else {
        panic!("refused");
    };
    assert_eq!(
        (peer.worker, peer.name.as_str(), peer.renew_now),
        (WorkerId(1), "nas", false)
    );
    // A second worker gets the next id.
    assert_eq!(c.join("vps", T0).worker(), WorkerId(2));
}

#[test]
fn tokens_are_single_use_and_expire() {
    let mut c = cluster(1);

    // Reused with another key: the replay is refused, and still is after
    // a restart.
    let token = c.token("a", T0);
    let key = KeyPair::generate().unwrap().public_key();
    let other = KeyPair::generate().unwrap().public_key();
    c.coord.redeem(&mut c.store, &token, &key, T0).unwrap();
    assert!(matches!(
        c.coord.redeem(&mut c.store, &token, &other, T0 + 1),
        Err(JoinError::AlreadyUsed)
    ));
    let mut c = c.reopen(T0 + 2);
    assert!(matches!(
        c.coord.redeem(&mut c.store, &token, &other, T0 + 3),
        Err(JoinError::AlreadyUsed)
    ));
    // Once the spent record is forgotten, it's simply unknown.
    c.coord
        .maintain(&mut c.store, &c.secrets, T0 + JOIN_TOKEN_TTL_SECS)
        .unwrap();
    assert!(matches!(
        c.coord
            .redeem(&mut c.store, &token, &key, T0 + JOIN_TOKEN_TTL_SECS),
        Err(JoinError::Unknown)
    ));

    // Expired: valid up to the last second, refused at the TTL.
    let t1 = T0 + 10_000;
    let late = c.token("b", t1);
    assert!(matches!(
        c.coord
            .redeem(&mut c.store, &late, &key, t1 + JOIN_TOKEN_TTL_SECS),
        Err(JoinError::Expired)
    ));
    c.coord
        .redeem(&mut c.store, &late, &key, t1 + JOIN_TOKEN_TTL_SECS - 1)
        .unwrap();
}

/// The grant got lost (or the worker crashed before saving it): asking
/// again with the same token and key gets the same certificate back, until
/// the token expires or the certificate is revoked. Nothing else does.
#[test]
fn a_lost_grant_can_be_asked_for_again_with_the_same_key() {
    let mut c = cluster(1);
    let joining = Joining::new(c.token("nas", T0)).unwrap();
    let first = c
        .coord
        .redeem(&mut c.store, joining.token(), &joining.public_key(), T0)
        .unwrap();
    // The same answer, across a restart too, and it works.
    let mut c = c.reopen(T0 + 1);
    let again = c
        .coord
        .redeem(&mut c.store, joining.token(), &joining.public_key(), T0 + 2)
        .unwrap();
    assert_eq!((again.worker, &again.cert), (first.worker, &first.cert));
    assert_eq!(c.coord.certificates().len(), 1);
    let token = JoinToken::parse(&joining.token().expose()).unwrap();
    let key = joining.public_key();
    let creds = joining.finish(again, T0 + 2).unwrap();
    assert!(matches!(c.connect(&creds, T0 + 3), Outcome::Done(Some(_))));

    // Not with another key, not after the token's expiry, not once revoked.
    assert!(matches!(
        c.coord.redeem(&mut c.store, &token, &[9; 32], T0 + 3),
        Err(JoinError::AlreadyUsed)
    ));
    assert!(matches!(
        c.coord
            .redeem(&mut c.store, &token, &key, T0 + JOIN_TOKEN_TTL_SECS),
        Err(JoinError::AlreadyUsed)
    ));
    c.coord.revoke(&mut c.store, creds.worker()).unwrap();
    assert!(matches!(
        c.coord.redeem(&mut c.store, &token, &key, T0 + 4),
        Err(JoinError::AlreadyUsed)
    ));
}

#[test]
fn a_guessed_token_reads_as_unknown() {
    let mut c = cluster(1);
    let real = c.token("a", T0);
    let key = [5u8; 32];
    let fp = c.coord.ca_fingerprint();
    // A different id altogether.
    let guess = crate::join::JoinToken::new([0xee; 8], &[1; 32], fp);
    assert!(matches!(
        c.coord.redeem(&mut c.store, &guess, &key, T0),
        Err(JoinError::Unknown)
    ));
    // The right id with the wrong secret.
    let forged = crate::join::JoinToken::new(real.id(), &[1; 32], fp);
    assert!(matches!(
        c.coord.redeem(&mut c.store, &forged, &key, T0),
        Err(JoinError::Unknown)
    ));
    // The forgery didn't burn the real token.
    c.coord.redeem(&mut c.store, &real, &key, T0).unwrap();
}

#[test]
fn tokens_are_bound_to_one_live_name() {
    let mut c = cluster(1);
    assert!(matches!(
        c.coord.create_join_token(&mut c.store, "Bad Name", T0),
        Err(ClusterError::InvalidName)
    ));
    let first = c.token("nas", T0);
    let second = c.token("nas", T0);
    let key = [5u8; 32];
    let grant = c.coord.redeem(&mut c.store, &first, &key, T0).unwrap();
    assert_eq!(grant.name, "nas");
    assert!(matches!(
        c.coord.redeem(&mut c.store, &second, &key, T0),
        Err(JoinError::NameInUse)
    ));
    assert!(matches!(
        c.coord.create_join_token(&mut c.store, "nas", T0),
        Err(ClusterError::NameInUse)
    ));
    // Revoking frees the name; the waiting token works now.
    c.coord.revoke(&mut c.store, grant.worker).unwrap();
    c.coord.redeem(&mut c.store, &second, &key, T0).unwrap();
}

#[test]
fn open_tokens_are_capped() {
    let mut c = cluster(1);
    for i in 0..MAX_OPEN_TOKENS {
        c.coord
            .create_join_token(&mut c.store, &format!("w{i}"), T0)
            .unwrap();
    }
    assert!(matches!(
        c.coord.create_join_token(&mut c.store, "one-more", T0),
        Err(ClusterError::TooManyTokens)
    ));
    // Expired ones stop counting.
    c.coord
        .create_join_token(&mut c.store, "later", T0 + JOIN_TOKEN_TTL_SECS)
        .unwrap();
}

#[test]
fn the_join_channel_refuses_a_man_in_the_middle() {
    let mut c = cluster(1);
    let joining = Joining::new(c.token("nas", T0)).unwrap();
    let config = joining.client_config(at(T0)).unwrap();

    // Another Lodestar (or anyone with their own CA).
    let other = cluster(2);
    assert_eq!(
        refused_cert(&handshake(&other.tls(T0), config.clone())),
        Some(CertificateError::UnknownIssuer)
    );

    // An impostor that sends the real CA certificate (it's public) and a
    // leaf naming the real CA as its issuer, but signed with its own key.
    let real_ca = c.coord.ca_certificates()[0].clone();
    let (ca_key, _, _) = self_signed_ca(T0);
    let (key, cert) = leaf(Role::Coordinator, &c.coord.ca_name_for_test(), &ca_key, T0);
    let outcome = handshake_with(
        impostor(vec![cert, real_ca.clone()], &key, ALPN_JOIN),
        config.clone(),
    );
    assert_eq!(
        refused_cert(&outcome),
        Some(CertificateError::BadSignature),
        "{outcome:?}"
    );

    // A worker's real certificate, from the real CA, can't pose as the
    // coordinator: it's only for client authentication.
    let (wkey, wcert) = c.coord.issue_for_test(Role::Worker("evil"), T0);
    let outcome = handshake_with(impostor(vec![wcert, real_ca], &wkey, ALPN_JOIN), config);
    assert!(
        matches!(
            &outcome,
            Outcome::Client(Error::InvalidCertificate(
                CertificateError::InvalidPurposeContext {
                    required: rustls::ExtendedKeyPurpose::ServerAuth,
                    ..
                }
            ))
        ),
        "{outcome:?}"
    );
}

#[test]
fn a_certificate_from_another_cluster_is_refused_both_ways() {
    let mut a = cluster(1);
    let mut b = cluster(2);
    let from_a = a.join("nas", T0);
    let from_b = b.join("nas", T0);
    // A's worker at B: B doesn't trust A's CA.
    assert_eq!(
        refused_cert(&b.connect(&from_a, T0)),
        Some(CertificateError::UnknownIssuer)
    );
    // And A's worker doesn't trust B's coordinator either.
    assert_eq!(
        refused_cert(&handshake(
            &b.tls(T0),
            from_a.client_config(at(T0)).unwrap()
        )),
        Some(CertificateError::UnknownIssuer)
    );
    assert!(matches!(a.connect(&from_a, T0), Outcome::Done(Some(_))));
    assert!(matches!(b.connect(&from_b, T0), Outcome::Done(Some(_))));
}

#[test]
fn an_unrecorded_certificate_from_the_real_ca_is_refused() {
    // As if the CA key leaked: a valid chain the coordinator never issued.
    let c = cluster(1);
    let (key, cert) = c.coord.issue_for_test(Role::Worker("ghost"), T0);
    let config =
        crate::tls::cluster_client_config(&c.coord.ca_certificates(), cert, &key, at(T0)).unwrap();
    assert_eq!(
        refused_cert(&handshake(&c.tls(T0), config)),
        Some(CertificateError::ApplicationVerificationFailure)
    );
}

#[test]
fn revocation_applies_on_the_next_handshake_and_survives_restarts() {
    let mut c = cluster(1);
    let creds = c.join("nas", T0);
    let other = c.join("vps", T0);
    let Outcome::Done(Some(peer)) = c.connect(&creds, T0) else {
        panic!("refused");
    };
    assert_eq!(c.coord.revoke(&mut c.store, creds.worker()).unwrap(), 1);
    assert_eq!(
        refused_cert(&c.connect(&creds, T0 + 1)),
        Some(CertificateError::Revoked)
    );
    assert_eq!(c.coord.revoked_serials(), vec![peer.serial]);
    assert!(matches!(
        c.coord.renew(&mut c.store, &peer, &[1; 32], T0 + 1),
        Err(ClusterError::Revoked)
    ));
    // Others are untouched.
    assert!(matches!(c.connect(&other, T0 + 1), Outcome::Done(Some(_))));

    let c = c.reopen(T0 + 2);
    assert_eq!(
        refused_cert(&c.connect(&creds, T0 + 3)),
        Some(CertificateError::Revoked)
    );
    assert!(matches!(c.connect(&other, T0 + 3), Outcome::Done(Some(_))));
}

#[test]
fn certificates_expire_and_tolerate_five_minutes_of_skew() {
    let mut c = cluster(1);
    let creds = c.join("nas", T0);
    let end = creds.not_after();
    assert_eq!(end, T0 + LEAF_LIFETIME_SECS);

    // The coordinator's clock past the end: refused.
    assert!(matches!(c.connect(&creds, end), Outcome::Done(Some(_))));
    assert_eq!(
        refused_cert(&c.connect(&creds, end + 1)),
        Some(CertificateError::Expired)
    );

    // A worker whose clock is four minutes behind still connects (both
    // certificates are backdated by the skew window)...
    let behind = T0 - CLOCK_SKEW_SECS + 60;
    assert!(matches!(
        c.connect_skewed(&creds, T0, behind),
        Outcome::Done(Some(_))
    ));
    assert!(matches!(
        c.connect_skewed(&creds, behind, T0),
        Outcome::Done(Some(_))
    ));
    // ...but not one past the window: the coordinator's certificate isn't
    // valid yet on the worker's clock, nor the worker's on the coordinator's.
    let far = T0 - CLOCK_SKEW_SECS - 1;
    assert_eq!(
        refused_cert(&c.connect_skewed(&creds, T0, far)),
        Some(CertificateError::NotValidYet)
    );
    assert_eq!(
        refused_cert(&c.connect_skewed(&creds, far, T0)),
        Some(CertificateError::NotValidYet)
    );
}

#[test]
fn renewal_happens_at_half_life_and_keeps_the_worker_connected() {
    let mut c = cluster(1);
    let creds = c.join("nas", T0);
    let half = T0 + LEAF_LIFETIME_SECS / 2;
    assert!(!creds.needs_renewal(half - 1));
    assert!(creds.needs_renewal(half));

    let Outcome::Done(Some(peer)) = c.connect(&creds, half) else {
        panic!("refused");
    };
    let renewal = creds.begin_renewal().unwrap();
    let grant = c
        .coord
        .renew(&mut c.store, &peer, &renewal.public_key(), half)
        .unwrap();
    let renewed = creds.renewed(renewal, grant, half).unwrap();
    assert_eq!(renewed.not_after(), half + LEAF_LIFETIME_SECS);

    // The coordinator renews its own certificate at half-life too.
    let before = c.coord.server_fingerprint();
    c.coord.maintain(&mut c.store, &c.secrets, half).unwrap();
    assert_ne!(c.coord.server_fingerprint(), before);
    let later = T0 + LEAF_LIFETIME_SECS + 60;
    assert!(matches!(c.connect(&renewed, later), Outcome::Done(Some(_))));
    assert_eq!(
        refused_cert(&c.connect(&creds, later)),
        Some(CertificateError::Expired)
    );
}

#[test]
fn a_grant_for_someone_else_is_refused() {
    let mut c = cluster(1);
    let joining = Joining::new(c.token("nas", T0)).unwrap();
    // A grant over a different key (a confused or hostile answer).
    let grant = c
        .coord
        .redeem(&mut c.store, joining.token(), &[9; 32], T0)
        .unwrap();
    assert!(matches!(
        joining.finish(grant, T0),
        Err(ClusterError::BadGrant)
    ));

    // A grant whose CA bundle isn't the one the token pinned.
    let other = cluster(2);
    let joining = Joining::new(c.token("vps", T0)).unwrap();
    let mut grant = c
        .coord
        .redeem(&mut c.store, joining.token(), &joining.public_key(), T0)
        .unwrap();
    grant.cas = other.coord.ca_certificates();
    assert!(matches!(
        joining.finish(grant, T0),
        Err(ClusterError::BadGrant)
    ));

    // A renewal answer for another worker.
    let mine = c.join("mine", T0);
    let theirs = c.join("theirs", T0);
    let Outcome::Done(Some(peer)) = c.connect(&theirs, T0) else {
        panic!("refused");
    };
    let renewal = mine.begin_renewal().unwrap();
    let grant = c
        .coord
        .renew(&mut c.store, &peer, &renewal.public_key(), T0)
        .unwrap();
    assert!(matches!(
        mine.renewed(renewal, grant, T0),
        Err(ClusterError::BadGrant)
    ));
}

#[test]
fn only_tls_1_3_with_a_certificate_and_one_known_protocol() {
    let mut c = cluster(1);
    let creds = c.join("nas", T0);

    // TLS 1.2.
    let mut tls12 = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    tls12.alpn_protocols = vec![ALPN_CLUSTER.to_vec()];
    assert!(matches!(
        handshake(&c.tls(T0), Arc::new(tls12)),
        Outcome::Server(Error::PeerIncompatible(_))
    ));

    // The cluster protocol without a certificate.
    let joining = Joining::new(c.token("vps", T0)).unwrap();
    let mut anonymous = (*joining.client_config(at(T0)).unwrap()).clone();
    anonymous.alpn_protocols = vec![ALPN_CLUSTER.to_vec()];
    assert!(matches!(
        handshake(&c.tls(T0), Arc::new(anonymous)),
        Outcome::Server(Error::NoCertificatesPresented)
    ));

    // No ALPN, an unknown one, or both.
    for alpn in [
        vec![],
        vec![b"h2".to_vec()],
        vec![ALPN_CLUSTER.to_vec(), ALPN_JOIN.to_vec()],
    ] {
        let mut config = (*creds.client_config(at(T0)).unwrap()).clone();
        config.alpn_protocols = alpn;
        assert!(matches!(
            handshake(&c.tls(T0), Arc::new(config)),
            Outcome::NoConfig
        ));
    }

    // A join connection identifies no one, even with a certificate.
    let mut join = (*creds.client_config(at(T0)).unwrap()).clone();
    join.alpn_protocols = vec![ALPN_JOIN.to_vec()];
    assert!(matches!(
        handshake(&c.tls(T0), Arc::new(join)),
        Outcome::Done(None)
    ));
}

#[test]
fn rotation_keeps_the_fleet_connected_through_the_overlap() {
    let mut c = cluster(1);
    let old_ca = c.coord.ca_fingerprint();
    let early = c.join("early", T0);
    let idle = c.join("idle", T0);

    c.coord
        .rotate_ca(&mut c.store, &c.secrets, T0 + 100)
        .unwrap();
    assert!(matches!(
        c.coord.rotate_ca(&mut c.store, &c.secrets, T0 + 101),
        Err(ClusterError::RotationInProgress)
    ));
    // The coordinator still presents the old CA's chain, so tokens pin it.
    assert_eq!(c.coord.ca_fingerprint(), old_ca);
    assert_eq!(c.coord.ca_certificates().len(), 2);

    // An old worker connects and is told to renew now.
    let Outcome::Done(Some(peer)) = c.connect(&early, T0 + 200) else {
        panic!("refused");
    };
    assert!(peer.renew_now);
    let renewal = early.begin_renewal().unwrap();
    let grant = c
        .coord
        .renew(&mut c.store, &peer, &renewal.public_key(), T0 + 200)
        .unwrap();
    let early = early.renewed(renewal, grant, T0 + 200).unwrap();
    let Outcome::Done(Some(peer)) = c.connect(&early, T0 + 300) else {
        panic!("refused");
    };
    assert!(!peer.renew_now);

    // A worker joining mid-rotation gets a new-CA certificate.
    let fresh = c.join("fresh", T0 + 300);

    // A restart mid-rotation keeps it, and the whole fleet still connects.
    let mut c = c.reopen(T0 + 400);
    assert_eq!(c.coord.ca_certificates().len(), 2);
    for (creds, renew_now) in [(&early, false), (&fresh, false), (&idle, true)] {
        let Outcome::Done(Some(peer)) = c.connect(creds, T0 + 400) else {
            panic!("refused");
        };
        assert_eq!(peer.renew_now, renew_now, "{}", creds.name());
    }

    // An open token pinned to the old CA holds the switch until it expires.
    let until = T0 + 100 + ROTATION_OVERLAP_SECS;
    let pinned = c.token("late", until - 60);
    assert_eq!(pinned.ca_fingerprint(), old_ca);
    c.coord.maintain(&mut c.store, &c.secrets, until).unwrap();
    assert_eq!(c.coord.ca_fingerprint(), old_ca);
    let end = until - 60 + JOIN_TOKEN_TTL_SECS;
    c.coord.maintain(&mut c.store, &c.secrets, end).unwrap();
    assert_ne!(c.coord.ca_fingerprint(), old_ca);
    assert_eq!(c.coord.ca_certificates().len(), 1);
    // Everything signed by the old CA is forgotten, and a week on, the rest
    // has expired too.
    assert!(c.coord.certificates().is_empty());
}

/// Connects and renews, as the protocol will at half-life.
fn renew(c: &mut Cluster, creds: Credentials, now: i64) -> Credentials {
    let Outcome::Done(Some(peer)) = c.connect(&creds, now) else {
        panic!("refused");
    };
    let r = creds.begin_renewal().unwrap();
    let grant = c
        .coord
        .renew(&mut c.store, &peer, &r.public_key(), now)
        .unwrap();
    creds.renewed(r, grant, now).unwrap()
}

#[test]
fn after_the_overlap_only_the_new_ca_is_trusted() {
    let mut c = cluster(1);
    let old_ca = c.coord.ca_fingerprint();
    let mut early = c.join("early", T0);
    let idle = c.join("idle", T0);
    c.coord
        .rotate_ca(&mut c.store, &c.secrets, T0 + 100)
        .unwrap();
    let mut fresh = c.join("fresh", T0 + 100);
    let until = T0 + 100 + ROTATION_OVERLAP_SECS;

    // Through the week, the online workers renew at half-life.
    let mut t = T0 + 100;
    while t + LEAF_LIFETIME_SECS / 2 < until {
        t += LEAF_LIFETIME_SECS / 2;
        c.coord.maintain(&mut c.store, &c.secrets, t).unwrap();
        early = renew(&mut c, early, t);
        fresh = renew(&mut c, fresh, t);
    }
    c.coord.maintain(&mut c.store, &c.secrets, until).unwrap();
    assert_ne!(c.coord.ca_fingerprint(), old_ca);
    assert_eq!(c.coord.ca_certificates().len(), 1);

    assert!(matches!(
        c.connect(&early, until + 1),
        Outcome::Done(Some(_))
    ));
    assert!(matches!(
        c.connect(&fresh, until + 1),
        Outcome::Done(Some(_))
    ));
    // The worker that never renewed holds only the old CA, so it no
    // longer recognises the coordinator; and its certificate is off the
    // record, so the coordinator wouldn't take it either.
    assert!(matches!(
        c.connect_skewed(&idle, T0 + 500, until + 1),
        Outcome::Client(Error::InvalidCertificate(CertificateError::UnknownIssuer))
    ));
    // And new tokens pin the new CA.
    let token = c.token("next", until + 2);
    assert_eq!(token.ca_fingerprint(), c.coord.ca_fingerprint());
    let _ = c.join("next2", until + 2);
}

#[test]
fn the_pki_survives_a_restart_and_needs_the_secrets_key() {
    let mut c = cluster(1);
    let creds = c.join("nas", T0);
    let fp = c.coord.ca_fingerprint();
    let c = c.reopen(T0 + 10);
    assert_eq!(c.coord.ca_fingerprint(), fp);
    assert!(matches!(c.connect(&creds, T0 + 11), Outcome::Done(Some(_))));

    // The database alone doesn't open: the keys are sealed outside it.
    let Cluster { mut store, .. } = c;
    let wrong = SecretStore::from_key(&[99; 32]);
    assert!(matches!(
        Coordinator::open(&mut store, &wrong, T0 + 12),
        Err(ClusterError::Secret(_))
    ));
    // No stored value holds a private key in the clear.
    for (_, value) in store.settings_with_prefix("cluster.").unwrap() {
        assert!(
            !value.contains("302e020100300506032b657004220420"),
            "{value}"
        );
    }
    // A record from a newer version (or tampered) is refused, not guessed.
    store
        .set_settings_text(&[("cluster.next_worker", "x")])
        .unwrap();
    assert!(matches!(
        Coordinator::open(&mut store, &SecretStore::from_key(&[1; 32]), T0 + 13),
        Err(ClusterError::Corrupt(_))
    ));
    // A deleted CA isn't quietly replaced while other cluster rows remain:
    // that would orphan every worker.
    store
        .update_settings_text(&[("cluster.next_worker", "2")], &["cluster.ca.current"])
        .unwrap();
    assert!(matches!(
        Coordinator::open(&mut store, &SecretStore::from_key(&[1; 32]), T0 + 14),
        Err(ClusterError::Corrupt(_))
    ));
}

#[test]
fn credentials_round_trip_through_their_file() {
    let mut c = cluster(1);
    let creds = c.join("nas", T0);
    let dir = std::env::temp_dir().join(format!("lodestar-cluster-{}-{}", std::process::id(), T0));
    let _ = std::fs::remove_dir_all(&dir);
    let secrets = SecretStore::from_key(&[42; 32]);
    assert!(Credentials::load(&dir, &secrets).unwrap().is_none());
    // A leftover temporary file from a crash, readable by everyone.
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("credentials.tmp"), "junk").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("credentials.tmp"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }
    creds.save(&dir, &secrets).unwrap();
    assert!(!dir.join("credentials.tmp").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("credentials"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let loaded = Credentials::load(&dir, &secrets).unwrap().unwrap();
    assert_eq!(
        (loaded.worker(), loaded.name(), loaded.not_after()),
        (creds.worker(), "nas", creds.not_after())
    );
    assert!(matches!(c.connect(&loaded, T0 + 1), Outcome::Done(Some(_))));

    // Another machine's secrets key can't open it.
    assert!(Credentials::load(&dir, &SecretStore::from_key(&[1; 32])).is_err());
    // Nor does a tampered file load.
    let path = dir.join("credentials");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("name nas", "name evil")).unwrap();
    assert!(
        Credentials::load(&dir, &secrets).is_ok(),
        "the name isn't sealed, the key is"
    );
    std::fs::write(&path, text.replacen("cert 30", "cert 31", 1)).unwrap();
    assert!(Credentials::load(&dir, &secrets).is_err());
    std::fs::write(&path, format!("{text}extra\n")).unwrap();
    assert!(Credentials::load(&dir, &secrets).is_err());
    std::fs::write(&path, text.replace("LWC1", "LWC2")).unwrap();
    assert!(Credentials::load(&dir, &secrets).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn nothing_secret_prints() {
    let mut c = cluster(1);
    let token = c.coord.create_join_token(&mut c.store, "nas", T0).unwrap();
    let text = token.expose().to_string();
    let joining = Joining::new(JoinToken::parse(&text).unwrap()).unwrap();
    for debug in [
        format!("{token:?}"),
        format!("{joining:?}"),
        format!("{:?}", c.coord),
    ] {
        assert!(!debug.contains(&text[4..20]), "{debug}");
        assert!(debug.len() < 200);
    }
    let creds = c.join("vps", T0);
    let debug = format!("{creds:?}");
    assert!(debug.contains("vps") && !debug.contains("key"), "{debug}");
}

/// One long-lived coordinator configuration and one reused worker
/// configuration, as in production: a reconnect must never resume a
/// session (which would skip the certificate checks).
#[test]
fn reconnects_are_full_handshakes_so_revocation_and_expiry_apply() {
    let mut c = cluster(1);
    let creds = c.join("nas", T0);
    let other = c.join("vps", T0);
    let coord_clock = Arc::new(Clock(T0.into()));
    let worker_clock = Arc::new(Clock(T0.into()));
    let tls = c
        .coord
        .tls()
        .with_time_provider(coord_clock.clone())
        .unwrap();
    let config = creds.client_config(Some(worker_clock.clone())).unwrap();
    let other_config = other.client_config(Some(worker_clock.clone())).unwrap();

    for _ in 0..3 {
        // `handshake` asserts each one is a full handshake.
        assert!(matches!(
            handshake(&tls, config.clone()),
            Outcome::Done(Some(_))
        ));
    }
    c.coord.revoke(&mut c.store, creds.worker()).unwrap();
    assert_eq!(
        refused_cert(&handshake(&tls, config)),
        Some(CertificateError::Revoked)
    );

    // Expiry, on the same long-lived configurations: just before, fine;
    // just after, refused, and the coordinator renews nothing.
    let end = other.not_after();
    coord_clock.set(end);
    worker_clock.set(end);
    let Outcome::Done(Some(peer)) = handshake(&tls, other_config.clone()) else {
        panic!("refused");
    };
    coord_clock.set(end + 1);
    assert_eq!(
        refused_cert(&handshake(&tls, other_config)),
        Some(CertificateError::Expired)
    );
    assert!(matches!(
        c.coord.renew(&mut c.store, &peer, &[1; 32], end + 1),
        Err(ClusterError::Expired)
    ));
}

#[test]
fn renewal_uses_the_record_not_the_callers_identity() {
    let mut c = cluster(1);
    let creds = c.join("nas", T0);
    let Outcome::Done(Some(mut peer)) = c.connect(&creds, T0) else {
        panic!("refused");
    };
    peer.name = "admin".into();
    let grant = c.coord.renew(&mut c.store, &peer, &[1; 32], T0).unwrap();
    assert_eq!(grant.name, "nas");
}

#[test]
fn two_coordinators_on_one_database_spend_a_token_once() {
    let path =
        std::env::temp_dir().join(format!("lodestar-cluster-db-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let secrets = SecretStore::from_key(&[1; 32]);
    let mut store_a = Store::open(&path, StoreConfig::default()).unwrap();
    let mut a = Coordinator::open(&mut store_a, &secrets, T0).unwrap();
    let token = a.create_join_token(&mut store_a, "nas", T0).unwrap();
    let second = a.create_join_token(&mut store_a, "vps", T0).unwrap();
    let mut store_b = Store::open(&path, StoreConfig::default()).unwrap();
    let mut b = Coordinator::open(&mut store_b, &secrets, T0).unwrap();

    let key = [5u8; 32];
    let grant = a.redeem(&mut store_a, &token, &key, T0).unwrap();
    assert!(matches!(
        b.redeem(&mut store_b, &token, &key, T0),
        Err(JoinError::AlreadyUsed)
    ));
    // And worker ids stay unique across both.
    let other = b.redeem(&mut store_b, &second, &key, T0).unwrap();
    assert_ne!(grant.worker, other.worker);
    drop((a, b, store_a, store_b));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
