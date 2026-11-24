//! The plugin WebSocket client (NOS-2b) against a real `wss://` server on
//! loopback, over a plugin's egress: TLS against a test CA, the endpoint
//! policy and host list, the upgrade, and messages both ways. Every wait
//! has a deadline; nothing sleeps for a fixed time.

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lodestar_integrations::BoxFuture;
use lodestar_integrations::net::{NetPolicy, Resolve};
use lodestar_sources::egress::NoCredentials;
use lodestar_sources::{
    Egress, EgressFactory, Endpoint, Endpoints, HttpError, PluginEgress, Roots, WsConnect, WsError,
    WsLimits, WsMessage, WsStream,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The deadline for every wait.
const WAIT: Duration = Duration::from_secs(20);

/// Every name resolves to loopback, so TLS can verify a real name.
struct Loopback;

impl Resolve for Loopback {
    fn resolve<'a>(&'a self, _: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
    }
}

/// The email tests' throwaway CA and `mail.test` leaf
/// (`crates/integrations/tests/fixtures/email/README.md`).
fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../integrations/tests/fixtures/email/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[derive(Clone, Copy)]
enum Mode {
    /// Upgrades and echoes every message.
    Echo,
    /// Answers the upgrade with 403.
    Refuse,
}

/// A TLS server on loopback for `mail.test`.
struct Server {
    port: u16,
    accepted: Arc<AtomicUsize>,
}

impl Server {
    async fn start(mode: Mode) -> Server {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(fixture("server.der"))],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fixture("server.key.der"))),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    match mode {
                        Mode::Refuse => {
                            let mut buf = [0u8; 4096];
                            let _ = tls.read(&mut buf).await;
                            let _ = tls
                                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                                .await;
                            let _ = tls.shutdown().await;
                        }
                        Mode::Echo => {
                            let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
                                return;
                            };
                            while let Some(Ok(m)) = ws.next().await {
                                if (m.is_text() || m.is_binary()) && ws.send(m).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        Server { port, accepted }
    }

    fn url(&self) -> String {
        format!("wss://mail.test:{}/relay", self.port)
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

async fn recv(s: &mut Box<dyn WsStream>) -> Result<Option<WsMessage>, WsError> {
    tokio::time::timeout(WAIT, s.recv())
        .await
        .expect("recv timed out")
}

fn factory() -> EgressFactory {
    EgressFactory::with_resolver(Arc::new(NoCredentials), Arc::new(Loopback))
        .with_roots(Roots::Only(vec![CertificateDer::from(fixture("ca.der"))]))
}

async fn egress(urls: &[&str]) -> PluginEgress {
    factory()
        .build(
            "relay",
            &Egress::Direct,
            Endpoints::user_entered(NetPolicy::download_client()).only_origins(urls),
        )
        .await
}

async fn connect(egress: &PluginEgress, url: &str) -> Result<Box<dyn WsStream>, WsError> {
    tokio::time::timeout(WAIT, egress.connect(url, WsLimits::default()))
        .await
        .expect("connect timed out")
}

#[tokio::test]
async fn a_session_opens_over_tls_and_echoes_both_kinds() {
    let server = Server::start(Mode::Echo).await;
    let url = server.url();
    let egress = egress(&[&url]).await;
    let mut session = connect(&egress, &url).await.unwrap();
    session
        .send(WsMessage::Text(r#"["REQ","s",{}]"#.into()))
        .await
        .unwrap();
    session
        .send(WsMessage::Binary(vec![0, 1, 2]))
        .await
        .unwrap();
    assert_eq!(
        recv(&mut session).await,
        Ok(Some(WsMessage::Text(r#"["REQ","s",{}]"#.into())))
    );
    assert_eq!(
        recv(&mut session).await,
        Ok(Some(WsMessage::Binary(vec![0, 1, 2])))
    );
    session.close().await;
    assert_eq!(recv(&mut session).await, Ok(None));
    let counters = egress.status().counters;
    assert_eq!(counters.answered, 1);
    assert_eq!(counters.transport_failures, 0);
}

#[tokio::test]
async fn a_refused_upgrade_reports_its_status() {
    let server = Server::start(Mode::Refuse).await;
    let url = server.url();
    let egress = egress(&[&url]).await;
    assert_eq!(
        connect(&egress, &url).await.err(),
        Some(WsError::Handshake(Some(403)))
    );
    assert_eq!(egress.status().counters.answered, 1);
}

#[tokio::test]
async fn a_certificate_for_another_name_is_refused_without_naming_it() {
    let server = Server::start(Mode::Echo).await;
    let url = format!("wss://other.test:{}/", server.port);
    let egress = egress(&[&url]).await;
    let err = connect(&egress, &url).await.err().unwrap();
    let text = err.to_string();
    assert!(
        matches!(err, WsError::Connect(HttpError::Transport(_))),
        "{err:?}"
    );
    assert!(text.contains("certificate"), "{text}");
    assert!(
        !text.contains("other.test") && !text.contains("mail.test"),
        "{text}"
    );
    assert_eq!(server.accepted(), 1);
    assert_eq!(egress.status().counters.transport_failures, 1);
}

#[tokio::test]
async fn the_platform_roots_dont_trust_a_test_ca() {
    let server = Server::start(Mode::Echo).await;
    let url = server.url();
    let egress = EgressFactory::with_resolver(Arc::new(NoCredentials), Arc::new(Loopback))
        .build(
            "relay",
            &Egress::Direct,
            Endpoints::user_entered(NetPolicy::download_client()).only_origins(&[&url]),
        )
        .await;
    assert!(matches!(
        connect(&egress, &url).await,
        Err(WsError::Connect(HttpError::Transport(_)))
    ));
}

#[tokio::test]
async fn the_endpoint_policy_refuses_before_connecting() {
    let server = Server::start(Mode::Echo).await;
    let url = server.url();
    let egress = egress(&[&url]).await;
    for bad in [
        format!("ws://mail.test:{}/relay", server.port),
        format!("https://mail.test:{}/relay", server.port),
        format!("wss://user:secret@mail.test:{}/relay", server.port),
        format!("wss://mail.test:{}/relay#x", server.port),
        // Not on the host list: another port, another host.
        format!(
            "wss://mail.test:{}/relay",
            server.port.wrapping_add(1).max(1)
        ),
        "wss://elsewhere.test/".to_owned(),
    ] {
        let err = connect(&egress, &bad).await.err().unwrap();
        assert!(
            matches!(&err, WsError::Connect(HttpError::Transport(t)) if t.starts_with("refused by the endpoint policy")),
            "{bad}: {err:?}"
        );
        assert!(!err.to_string().contains("secret"), "{err}");
    }
    assert_eq!(server.accepted(), 0, "nothing reached the server");
    assert_eq!(egress.status().counters.policy_refusals, 6);
}

#[tokio::test]
async fn a_public_only_plugin_cant_reach_loopback() {
    let server = Server::start(Mode::Echo).await;
    let egress = factory()
        .build(
            "relay",
            &Egress::Direct,
            Endpoints::manifest(&[Endpoint {
                protocol: "wss",
                host: "mail.test",
            }]),
        )
        .await;
    // On the list, but it resolves to loopback: refused after the lookup.
    let err = connect(&egress, "wss://mail.test/").await.err().unwrap();
    assert!(err.to_string().contains("loopback"), "{err}");
    // The server's own port isn't on the list (a manifest's wss is 443).
    let err = connect(&egress, &server.url()).await.err().unwrap();
    assert!(
        err.to_string().contains("refused by the endpoint policy"),
        "{err}"
    );
    assert_eq!(server.accepted(), 0);
}

#[tokio::test]
async fn an_unbuildable_route_refuses_every_session() {
    let server = Server::start(Mode::Echo).await;
    let url = server.url();
    let egress = factory()
        .build(
            "relay",
            &Egress::Interface("not a valid name/".into()),
            Endpoints::user_entered(NetPolicy::download_client()).only_origins(&[&url]),
        )
        .await;
    let err = connect(&egress, &url).await.err().unwrap();
    assert!(err.route_fault(), "{err:?}");
    assert_eq!(server.accepted(), 0);
    assert_eq!(egress.status().counters.kill_switch_refusals, 1);
}
