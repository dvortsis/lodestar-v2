//! The cluster protocol end to end (§19.2, §19.3, §19.6, §19.8): over real
//! mutual TLS on loopback and over the in-process local worker link.
//!
//! Time is a [`ManualClock`] shared by the coordinator and its workers, and
//! heartbeats and ticks are driven by hand, so every assertion is about
//! what happened, not how fast. Wall-clock time appears only as hang
//! guards and in the pre-auth handshake timeout.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lodestar_cluster::proto::*;
use lodestar_cluster::*;
use lodestar_integrations::SecretStore;
use lodestar_metainfo::InfoHash;
use lodestar_store::{Store, StoreConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(target_os = "linux")]
use tokio::net::TcpSocket;
use tokio::net::{TcpListener, TcpStream};

const GUARD: Duration = Duration::from_secs(60);
const WAIT: Duration = Duration::from_secs(15);
const SECRETS_KEY: [u8; 32] = [7; 32];

struct Coord {
    server: Arc<ClusterServer>,
    addr: String,
    clock: Arc<ManualClock>,
    db: PathBuf,
    _dir: tempfile::TempDir,
}

fn config() -> ServerConfig {
    ServerConfig {
        auto_tick: false,
        ..ServerConfig::default()
    }
}

async fn coordinator(config: ServerConfig) -> Coord {
    coordinator_with(config, None).await
}

async fn coordinator_with(config: ServerConfig, stored: Option<Arc<dyn StoredKeys>>) -> Coord {
    coordinator_events(config, stored, Arc::new(NoEvents)).await
}

/// A coordinator whose lease authority reports to `events`.
async fn coordinator_events(
    config: ServerConfig,
    stored: Option<Arc<dyn StoredKeys>>,
    events: Arc<dyn LeaseEvents>,
) -> Coord {
    coordinator_hooks(config, stored, events, None).await
}

/// A coordinator with every hook given.
async fn coordinator_hooks(
    config: ServerConfig,
    stored: Option<Arc<dyn StoredKeys>>,
    events: Arc<dyn LeaseEvents>,
    admission: Option<Arc<dyn GrantAdmission>>,
) -> Coord {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let secrets = SecretStore::from_key(&SECRETS_KEY);
    let clock = Arc::new(ManualClock::starting_now());
    let coordinator = Coordinator::open(&mut store, &secrets, clock.now_secs()).unwrap();
    let pki = Pki {
        coordinator,
        store,
        secrets,
    };
    let epoch = u32::try_from(clock.now_secs().rem_euclid(1 << 32)).unwrap_or(0);
    let hooks = ServerHooks {
        authority: Arc::new(LeaseAuthority::new(epoch, config.lease_ttl_secs, events)),
        stored,
        sink: None,
        admission,
    };
    let server = ClusterServer::with_hooks(pki, clock.clone(), config, hooks).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(server.clone().serve(listener));
    Coord {
        server,
        addr,
        clock,
        db,
        _dir: dir,
    }
}

impl Coord {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    async fn join(&self, name: &str) -> Credentials {
        let token = self.server.create_join_token(name).await.unwrap();
        let joining = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
        join(&self.addr, joining, self.clock(), WAIT).await.unwrap()
    }

    async fn link(&self, creds: &Credentials) -> WorkerLink {
        connect(&self.addr, creds, self.clock(), WAIT)
            .await
            .unwrap()
    }

    async fn session_with(&self, creds: &Credentials, clock: Arc<dyn Clock>) -> WorkerSession {
        let link = connect(&self.addr, creds, clock.clone(), WAIT)
            .await
            .unwrap();
        start(link, creds.worker().0, clock).await.unwrap()
    }

    async fn session(&self, creds: &Credentials) -> WorkerSession {
        self.session_with(creds, self.clock()).await
    }

    async fn local(&self) -> WorkerSession {
        start(self.server.connect_local(), 0, self.clock())
            .await
            .unwrap()
    }

    /// The database as another process (a CLI command) opens it.
    fn other_process(&self) -> (Coordinator, Store, SecretStore) {
        let mut store = Store::open(&self.db, StoreConfig::default()).unwrap();
        let secrets = SecretStore::from_key(&SECRETS_KEY);
        let coordinator = Coordinator::open(&mut store, &secrets, self.clock.now_secs()).unwrap();
        (coordinator, store, secrets)
    }

    fn advance(&self, secs: u64) {
        self.clock.advance(Duration::from_secs(secs));
    }
}

fn manual() -> WorkerConfig {
    WorkerConfig {
        auto_heartbeat: false,
        ..WorkerConfig::default()
    }
}

async fn start(
    link: WorkerLink,
    worker: u32,
    clock: Arc<dyn Clock>,
) -> Result<WorkerSession, ClientError> {
    let caps = Capabilities {
        cores: 4,
        ram_mb: 2048,
        external_ips: vec!["192.0.2.10".parse().unwrap()],
        companions: 0,
        egress: 0,
    };
    WorkerSession::start(link, Hello::new(worker, caps), clock, manual()).await
}

fn key(n: u8) -> [u8; 20] {
    [n; 20]
}

fn found(clock: &dyn Clock, keys: &[u8]) -> Vec<ClaimItem> {
    keys.iter()
        .map(|&n| ClaimItem {
            key: key(n),
            source: 1,
            strategy: 2,
            discovered_ms: clock.now_ms(),
            hints: vec![],
        })
        .collect()
}

fn token_of(item: &GrantItem) -> u64 {
    match item {
        GrantItem::Granted { token, .. } => *token,
        GrantItem::Denied { .. } => panic!("denied: {item:?}"),
    }
}

/// A terminal outcome. (`Fetched` keeps the lease until its commit, WK-0.)
fn fetched(key: [u8; 20], token: u64) -> OutcomeItem {
    OutcomeItem {
        key,
        token,
        outcome: Outcome::PeerFound { reason: 10 },
    }
}

async fn guard<F: std::future::Future<Output = ()>>(f: F) {
    tokio::time::timeout(GUARD, f)
        .await
        .expect("hang guard: the test didn't finish");
}

/// Waits for an event, skipping `Assign`.
async fn event(session: &mut WorkerSession) -> WorkerEvent {
    loop {
        match tokio::time::timeout(WAIT, session.next_event())
            .await
            .expect("no event")
        {
            Some(WorkerEvent::Assign(_)) => {}
            Some(e) => return e,
            None => panic!("events ended"),
        }
    }
}

/// Polls a condition on the server (for effects of raw TCP connections,
/// which have no reply to wait on).
async fn until(server: &ClusterServer, what: impl Fn(&FleetSnapshot) -> bool) {
    tokio::time::timeout(WAIT, async {
        while !what(&server.snapshot()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never reached: {:?}", server.snapshot()));
}

/// The lease lifecycle both transports must run identically: claim and
/// grant with fencing tokens, a claim for a key held elsewhere, outcomes,
/// a stale report ignored, a drain, and the funnel reconciling.
async fn lifecycle(c: &Coord, mut a: WorkerSession, b: WorkerSession) {
    let welcome = a.welcome().clone();
    assert_eq!(welcome.major, PROTOCOL_MAJOR);
    assert_eq!((welcome.heartbeat_ms, welcome.lease_ttl_secs), (5_000, 600));

    let ack = a.heartbeat().await.unwrap();
    assert_eq!((ack.offset_ms, ack.skewed, ack.leases), (0, false, 0));

    let grants = a.claim(found(&*c.clock, &[1, 2, 3])).await.unwrap();
    assert_eq!(grants.len(), 3);
    let tokens: Vec<u64> = grants.iter().map(token_of).collect();
    assert!(tokens[0] < tokens[1] && tokens[1] < tokens[2]);
    assert!(
        grants
            .iter()
            .all(|g| matches!(g, GrantItem::Granted { ttl_secs: 600, .. }))
    );
    assert_eq!(a.leases(), 3);

    // The same key from another worker: held elsewhere, no second lease.
    let other = b.claim(found(&*c.clock, &[2, 4])).await.unwrap();
    assert_eq!(
        other[0],
        GrantItem::Denied {
            key: key(2),
            reason: DenyReason::HeldElsewhere
        }
    );
    let b_token = token_of(&other[1]);
    // Claiming its own key again gives the holder the same lease.
    let again = a.claim(found(&*c.clock, &[1])).await.unwrap();
    assert_eq!(token_of(&again[0]), tokens[0]);

    a.report(vec![fetched(key(1), tokens[0]), fetched(key(2), tokens[1])])
        .await
        .unwrap();
    // A report from the wrong worker, and a duplicate: both stale.
    b.report(vec![fetched(key(3), tokens[2])]).await.unwrap();
    a.report(vec![fetched(key(1), tokens[0])]).await.unwrap();
    // A heartbeat answered means everything before it was handled.
    assert_eq!(a.heartbeat().await.unwrap().leases, 1);
    b.heartbeat().await.unwrap();
    let s = c.server.snapshot();
    assert_eq!((s.harvested, s.outcomes, s.stale_reports), (4, 2, 2));
    assert_eq!(s.leases, 2);

    // Drain: a finishes what it holds, says so, and is stopped.
    assert!(c.server.drain(WorkerId(a.welcome().worker)));
    assert_eq!(event(&mut a).await, WorkerEvent::Drain);
    let refused = a.claim(found(&*c.clock, &[9])).await.unwrap();
    assert_eq!(
        refused[0],
        GrantItem::Denied {
            key: key(9),
            reason: DenyReason::Draining
        }
    );
    a.report(vec![OutcomeItem {
        key: key(3),
        token: tokens[2],
        outcome: Outcome::Lost { reason: 1 },
    }])
    .await
    .unwrap();
    a.drained().await.unwrap();
    assert_eq!(
        event(&mut a).await,
        WorkerEvent::Ended(Some(StopReason::Drained))
    );

    b.report(vec![fetched(key(4), b_token)]).await.unwrap();
    b.heartbeat().await.unwrap();
    let s = c.server.snapshot();
    // harvested = Σ terminal outcomes + in flight (§19.8).
    assert_eq!(s.harvested, 4);
    assert_eq!(s.outcomes, 4);
    assert_eq!(s.leases + s.pending, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_lifecycle_over_mutual_tls() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let vps = c.join("vps").await;
        assert_ne!(nas.worker(), vps.worker());
        let a = c.session(&nas).await;
        let b = c.session(&vps).await;
        assert_eq!(a.welcome().worker, nas.worker().0);
        lifecycle(&c, a, b).await;
        assert_eq!(c.server.snapshot().join_requests, 2);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_lifecycle_over_the_in_process_link() {
    guard(async {
        let c = coordinator(config()).await;
        let a = c.local().await;
        assert_eq!(a.welcome().worker, WorkerId::LOCAL.0);
        let vps = c.join("vps").await;
        let b = c.session(&vps).await;
        lifecycle(&c, a, b).await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_lease_is_granted_again_and_the_old_holder_is_fenced() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut a = c.session(&nas).await;
        let mut b = c.local().await;
        let old = token_of(&a.claim(found(&*c.clock, &[1])).await.unwrap()[0]);

        // a goes silent. Three heartbeats later its connection is closed,
        // but its lease stands until the TTL.
        c.advance(16);
        b.heartbeat().await.unwrap();
        c.server.tick().await;
        assert_eq!(
            event(&mut a).await,
            WorkerEvent::Ended(Some(StopReason::HeartbeatTimeout))
        );
        assert_eq!(c.server.snapshot().leases, 1);

        // Past the TTL, the key goes to the live worker under a new token.
        c.advance(590);
        b.heartbeat().await.unwrap();
        c.server.tick().await;
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key(), key(1));
        let new = token_of(&items[0]);
        assert_ne!(new, old);
        assert_eq!(b.leases(), 1);

        // a comes back and reports late: stale, ignored. b's report counts.
        let a = c.session(&nas).await;
        a.report(vec![fetched(key(1), old)]).await.unwrap();
        a.heartbeat().await.unwrap();
        b.report(vec![fetched(key(1), new)]).await.unwrap();
        b.heartbeat().await.unwrap();
        let s = c.server.snapshot();
        assert_eq!((s.harvested, s.outcomes, s.stale_reports), (1, 1, 1));
        assert_eq!((s.regrants, s.leases, s.pending), (1, 0, 0));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_from_another_process_closes_the_connection_within_one_heartbeat() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut a = c.session(&nas).await;
        let mut b = c.local().await;
        let held = token_of(&a.claim(found(&*c.clock, &[1])).await.unwrap()[0]);

        // `lodestar cluster revoke nas`, from a CLI process on the same
        // database.
        let (mut cli, mut store, _secrets) = c.other_process();
        assert_eq!(cli.revoke(&mut store, nas.worker()).unwrap(), 1);
        drop((cli, store));

        // The next tick (one heartbeat) reloads the PKI and closes it.
        let ticks = c.server.snapshot().ticks;
        b.heartbeat().await.unwrap();
        c.server.tick().await;
        assert_eq!(
            event(&mut a).await,
            WorkerEvent::Ended(Some(StopReason::Revoked))
        );
        assert_eq!(c.server.snapshot().ticks, ticks + 1);
        assert_eq!(c.server.snapshot().revoked, 1);
        // Its lease went straight to a live worker (§19.6).
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        assert_eq!(items[0].key(), key(1));
        assert_ne!(token_of(&items[0]), held);
        // And it can't come back.
        assert!(connect(&c.addr, &nas, c.clock(), WAIT).await.is_err());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_through_the_server_applies_at_once() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut a = c.session(&nas).await;
        assert_eq!(c.server.revoke(nas.worker()).await.unwrap(), 1);
        assert_eq!(
            event(&mut a).await,
            WorkerEvent::Ended(Some(StopReason::Revoked))
        );
        assert_eq!(c.server.snapshot().ticks, 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_made_by_another_process_can_be_redeemed() {
    guard(async {
        let c = coordinator(config()).await;
        let (mut cli, mut store, _secrets) = c.other_process();
        let token = cli
            .create_join_token(&mut store, "pi", c.clock.now_secs())
            .unwrap();
        let joining = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
        let creds = join(&c.addr, joining, c.clock(), WAIT).await.unwrap();
        let a = c.session(&creds).await;
        a.heartbeat().await.unwrap();

        // Used once: the same token is refused, like any bad token.
        let again = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
        assert!(matches!(
            join(&c.addr, again, c.clock(), WAIT).await,
            Err(ClientError::Refused)
        ));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_handshake_is_cut_by_the_timeout() {
    guard(async {
        let c = coordinator(ServerConfig {
            handshake_timeout: Duration::from_millis(1000),
            ..config()
        })
        .await;
        // Half a TLS record header, then nothing.
        let mut slow = TcpStream::connect(&c.addr).await.unwrap();
        slow.write_all(&[0x16, 0x03]).await.unwrap();
        let mut buf = [0u8; 16];
        let n = tokio::time::timeout(WAIT, slow.read(&mut buf))
            .await
            .expect("not cut")
            .unwrap_or(0);
        assert_eq!(n, 0, "closed without an answer");
        until(&c.server, |s| {
            s.handshake_timeouts == 1 && s.handshaking == 0
        })
        .await;

        // A worker that authenticates and then never says Hello is cut too.
        let nas = c.join("nas").await;
        let mut link = c.link(&nas).await;
        let stopped = tokio::time::timeout(WAIT, link.recv())
            .await
            .expect("not cut");
        assert!(matches!(
            stopped,
            Ok(Some(ToWorker::Stop(StopReason::Protocol)))
        ));
    })
    .await;
}

#[cfg(target_os = "linux")]
async fn from(ip: &str, addr: &str) -> TcpStream {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind(format!("{ip}:0").parse().unwrap()).unwrap();
    socket.connect(addr.parse().unwrap()).await.unwrap()
}

/// Whether the server closed the connection without a word.
#[cfg(target_os = "linux")]
async fn closed(stream: &mut TcpStream) -> bool {
    let mut buf = [0u8; 1];
    matches!(
        tokio::time::timeout(WAIT, stream.read(&mut buf)).await,
        Ok(Ok(0) | Err(_))
    )
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_caps_refuse_the_excess_before_authentication() {
    guard(async {
        let c = coordinator(ServerConfig {
            max_pending: 3,
            max_pending_per_ip: 1,
            ..config()
        })
        .await;
        // Linux routes all of 127/8 to loopback, so these are four addresses.
        let _a = from("127.0.0.1", &c.addr).await;
        until(&c.server, |s| s.handshaking == 1).await;
        let mut same_ip = from("127.0.0.1", &c.addr).await;
        assert!(closed(&mut same_ip).await);
        let _b = from("127.0.0.2", &c.addr).await;
        let _c = from("127.0.0.3", &c.addr).await;
        until(&c.server, |s| s.handshaking == 3).await;
        let mut over_total = from("127.0.0.4", &c.addr).await;
        assert!(closed(&mut over_total).await);
        assert_eq!(c.server.snapshot().refused, 2);

        // The slots come back when those connections go.
        drop((_a, _b, _c));
        until(&c.server, |s| s.handshaking == 0).await;
        let nas = c.join("nas").await;
        // The join connection holds its slot until it has closed.
        until(&c.server, |s| s.handshaking == 0).await;
        let a = c.session(&nas).await;
        a.heartbeat().await.unwrap();
        // An authenticated session doesn't hold a pre-auth slot.
        assert_eq!(c.server.snapshot().handshaking, 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_major_version_or_identity_is_refused() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut link = c.link(&nas).await;
        let hello = Hello {
            major: PROTOCOL_MAJOR + 1,
            ..Hello::new(nas.worker().0, Capabilities::default())
        };
        link.send(ToCoordinator::Hello(hello)).await.unwrap();
        assert_eq!(
            link.recv().await.unwrap(),
            Some(ToWorker::Stop(StopReason::VersionMismatch {
                coordinator_major: PROTOCOL_MAJOR
            }))
        );
        assert_eq!(link.recv().await.unwrap(), None);

        let link = c.link(&nas).await;
        let err = start(link, nas.worker().0 + 1, c.clock())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ClientError::Stopped(StopReason::IdentityMismatch)
        ));
        let s = c.server.snapshot();
        assert_eq!((s.version_refused, s.sessions), (1, 0));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_connection_replaces_the_first() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut first = c.session(&nas).await;
        let second = c.session(&nas).await;
        assert_eq!(
            event(&mut first).await,
            WorkerEvent::Ended(Some(StopReason::Replaced))
        );
        second.heartbeat().await.unwrap();
        let s = c.server.snapshot();
        assert_eq!(s.workers.len(), 1);
        assert!(s.workers[0].connected);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clock_offsets_are_measured_and_a_minute_off_is_flagged() {
    guard(async {
        let c = coordinator(config()).await;
        let fast = c.join("fast").await;
        let close = c.join("close").await;
        let fast_clock: Arc<dyn Clock> = Arc::new(OffsetClock {
            base: c.clock(),
            offset_ms: 90_000,
        });
        let close_clock: Arc<dyn Clock> = Arc::new(OffsetClock {
            base: c.clock(),
            offset_ms: -10_000,
        });
        let mut f = c.session_with(&fast, fast_clock).await;
        let n = c.session_with(&close, close_clock).await;
        for _ in 0..2 {
            let ack = f.heartbeat().await.unwrap();
            assert_eq!((ack.offset_ms, ack.skewed), (90_000, true));
            let ack = n.heartbeat().await.unwrap();
            assert_eq!((ack.offset_ms, ack.skewed), (-10_000, false));
        }
        assert_eq!(
            event(&mut f).await,
            WorkerEvent::ClockSkewed { offset_ms: 90_000 }
        );
        let s = c.server.snapshot();
        assert_eq!(s.skew_flags, 1, "flagged once, not every heartbeat");
        let flagged: Vec<bool> = s.workers.iter().map(|w| w.skewed).collect();
        assert_eq!(flagged, vec![true, false]);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renewal_over_the_protocol_is_rate_limited() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let a = c.session(&nas).await;
        let renewal = nas.begin_renewal().unwrap();
        assert_eq!(
            a.renew(renewal.public_key()).await.unwrap().unwrap_err(),
            RenewRefusal::TooSoon
        );
        c.advance(RENEW_MIN_INTERVAL_SECS as u64);
        let grant = a.renew(renewal.public_key()).await.unwrap().unwrap();
        let renewed = nas.renewed(renewal, grant, c.clock.now_secs()).unwrap();
        assert!(renewed.not_after() > nas.not_after());
        let next = renewed.begin_renewal().unwrap();
        assert_eq!(
            a.renew(next.public_key()).await.unwrap().unwrap_err(),
            RenewRefusal::TooSoon
        );
        // The local worker has no certificate to renew.
        let local = c.local().await;
        assert_eq!(
            local.renew([1; 32]).await.unwrap().unwrap_err(),
            RenewRefusal::Failed
        );
        // The renewed certificate connects.
        let b = c.session(&renewed).await;
        b.heartbeat().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pause_stands_when_the_sessions_certificate_expires_after_a_renewal() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut a = c.session(&nas).await;
        token_of(&a.claim(found(&*c.clock, &[7])).await.unwrap()[0]);
        assert!(c.server.pause(nas.worker()));
        c.advance(RENEW_MIN_INTERVAL_SECS as u64);
        let renewal = nas.begin_renewal().unwrap();
        let grant = a.renew(renewal.public_key()).await.unwrap().unwrap();
        let renewed = nas.renewed(renewal, grant, c.clock.now_secs()).unwrap();

        // The session's own certificate expires; the renewed one stands.
        let left = nas.not_after() - c.clock.now_secs() + 1;
        c.advance(left.unsigned_abs());
        assert!(c.clock.now_secs() <= renewed.not_after());
        a.heartbeat().await.unwrap();
        c.server.tick().await;
        assert!(matches!(event(&mut a).await, WorkerEvent::Ended(_)));
        let w = &c.server.snapshot().workers;
        assert!(
            w.iter().any(|s| s.worker == nas.worker() && s.paused),
            "{w:?}"
        );
        assert_eq!(c.server.authority().held_by(nas.worker()), 1, "lease kept");

        // Back with the renewed certificate: still paused.
        let b = c.session(&renewed).await;
        let denied = b.claim(found(&*c.clock, &[1])).await.unwrap();
        assert!(
            matches!(
                denied[0],
                GrantItem::Denied {
                    reason: DenyReason::Draining,
                    ..
                }
            ),
            "{denied:?}"
        );
        assert_eq!(c.server.authority().held_by(nas.worker()), 1);

        // Once no certificate stands, the worker is forgotten and its
        // lease goes back.
        let left = renewed.not_after() - c.clock.now_secs() + 1;
        c.advance(left.unsigned_abs());
        b.heartbeat().await.unwrap();
        c.server.tick().await;
        let s = c.server.snapshot();
        assert!(!s.workers.iter().any(|x| x.worker == nas.worker()), "{s:?}");
        assert_eq!(c.server.authority().held_by(nas.worker()), 0);
        assert_eq!(s.leases + s.pending, 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_worker_with_a_lapsed_session_is_forgotten() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut a = c.session(&nas).await;
        token_of(&a.claim(found(&*c.clock, &[7])).await.unwrap()[0]);
        c.advance(RENEW_MIN_INTERVAL_SECS as u64);
        let renewal = nas.begin_renewal().unwrap();
        let grant = a.renew(renewal.public_key()).await.unwrap().unwrap();
        nas.renewed(renewal, grant, c.clock.now_secs()).unwrap();
        let left = nas.not_after() - c.clock.now_secs() + 1;
        c.advance(left.unsigned_abs());
        a.heartbeat().await.unwrap();
        c.server.tick().await;
        assert!(matches!(event(&mut a).await, WorkerEvent::Ended(_)));
        assert_eq!(c.server.authority().held_by(nas.worker()), 1);
        // Revoked while away: every certificate goes, and so does it.
        c.server.revoke(nas.worker()).await.unwrap();
        let s = c.server.snapshot();
        assert!(!s.workers.iter().any(|x| x.worker == nas.worker()), "{s:?}");
        assert_eq!(c.server.authority().held_by(nas.worker()), 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_session_starts_paused_when_its_pause_is_saved() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let saved = nas.worker();
        c.server
            .set_pause_source(Arc::new(move |w: WorkerId| w == saved));
        let a = c.session(&nas).await;
        let denied = a.claim(found(&*c.clock, &[1])).await.unwrap();
        assert!(matches!(
            denied[0],
            GrantItem::Denied {
                reason: DenyReason::Draining,
                ..
            }
        ));
        // A live resume lifts it for this session.
        assert!(c.server.resume(nas.worker()));
        token_of(&a.claim(found(&*c.clock, &[1])).await.unwrap()[0]);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticks_run_maintain_hourly_so_the_coordinator_certificate_renews() {
    guard(async {
        let c = coordinator(config()).await;
        let server_cert = || {
            let store = Store::open(&c.db, StoreConfig::default()).unwrap();
            store.setting_text("cluster.server").unwrap().unwrap()
        };
        let before = server_cert();
        c.server.tick().await;
        assert_eq!(server_cert(), before);
        // Past half its life, the next hourly upkeep renews it.
        c.advance((LEAF_LIFETIME_SECS / 2) as u64 + 1);
        c.server.tick().await;
        assert_ne!(server_cert(), before);
        assert_eq!(c.server.snapshot().pki_errors, 0);
        // And workers still connect to the renewed certificate.
        let nas = c.join("nas").await;
        let a = c.session(&nas).await;
        a.heartbeat().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeats_run_by_themselves_when_asked() {
    guard(async {
        let c = coordinator(ServerConfig {
            heartbeat: Duration::from_millis(100),
            ..config()
        })
        .await;
        let nas = c.join("nas").await;
        let link = c.link(&nas).await;
        let auto = WorkerConfig::default();
        let a = WorkerSession::start(
            link,
            Hello::new(nas.worker().0, Capabilities::default()),
            c.clock(),
            auto,
        )
        .await
        .unwrap();
        a.set_stats(Stats {
            fetched: 42,
            ..Stats::default()
        })
        .await
        .unwrap();
        until(&c.server, |s| {
            s.workers.first().is_some_and(|w| w.stats.fetched == 42)
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_every_session() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut a = c.session(&nas).await;
        let mut local = c.local().await;
        c.server.shutdown();
        for s in [&mut a, &mut local] {
            assert_eq!(
                event(s).await,
                WorkerEvent::Ended(Some(StopReason::Shutdown))
            );
        }
    })
    .await;
}

/// Keys 100 and up are "in the store".
struct HighKeysStored;

impl StoredKeys for HighKeysStored {
    fn stored(&self, keys: &[InfoHash]) -> Vec<bool> {
        keys.iter().map(|k| k.0[0] >= 100).collect()
    }
}

/// A store check for re-grants (#607's reviews): claims see nothing
/// stored; a re-grant's check answers `Held`, `Stored` or `Unknown` for
/// the keys listed, comes back short while `short`, and with a `gate`,
/// signals and waits once before answering.
#[derive(Default)]
struct RegrantCheck {
    held: std::sync::Mutex<Vec<[u8; 20]>>,
    stored: std::sync::Mutex<Vec<[u8; 20]>>,
    unknown: std::sync::Mutex<Vec<[u8; 20]>>,
    short: std::sync::atomic::AtomicBool,
    gate: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
}

impl StoredKeys for RegrantCheck {
    fn stored(&self, keys: &[InfoHash]) -> Vec<bool> {
        vec![false; keys.len()]
    }

    fn key_checks(&self, keys: &[InfoHash]) -> Vec<KeyCheck> {
        let gate = self.gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            let _ = release.recv();
        }
        if self.short.load(std::sync::atomic::Ordering::SeqCst) {
            return Vec::new();
        }
        let (held, stored, unknown) = (
            self.held.lock().unwrap(),
            self.stored.lock().unwrap(),
            self.unknown.lock().unwrap(),
        );
        keys.iter()
            .map(|k| {
                if held.contains(&k.0) {
                    KeyCheck::Held
                } else if stored.contains(&k.0) {
                    KeyCheck::Stored
                } else if unknown.contains(&k.0) {
                    KeyCheck::Unknown
                } else {
                    KeyCheck::Free
                }
            })
            .collect()
    }
}

/// The losses the lease authority reported: key, holder, reason.
#[derive(Default)]
struct Losses(std::sync::Mutex<Vec<([u8; 20], WorkerId, u8)>>);

impl LeaseEvents for Losses {
    fn lost(&self, key: &InfoHash, lease: &Lease, reason: u8) {
        self.0.lock().unwrap().push((key.0, lease.holder, reason));
    }
}

/// `a` claims `keys` and goes silent; past the TTL its leases wait for a
/// re-grant, and the tick that would re-grant them runs.
async fn leases_left_by_a_silent_worker(c: &Coord, b: &WorkerSession, keys: &[u8]) {
    let nas = c.join("nas").await;
    let a = c.session(&nas).await;
    for g in a.claim(found(&*c.clock, keys)).await.unwrap() {
        token_of(&g);
    }
    c.advance(16);
    b.heartbeat().await.unwrap();
    c.server.tick().await;
    c.advance(590);
    b.heartbeat().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stored_or_held_key_is_never_granted_again() {
    guard(async {
        let check = Arc::new(RegrantCheck::default());
        let losses = Arc::new(Losses::default());
        let c = coordinator_events(config(), Some(check.clone()), losses.clone()).await;
        let mut b = c.local().await;
        leases_left_by_a_silent_worker(&c, &b, &[1, 2, 3]).await;
        // Stored or held since they were claimed.
        check.held.lock().unwrap().push(key(2));
        check.stored.lock().unwrap().push(key(3));
        c.server.tick().await;
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        let keys: Vec<[u8; 20]> = items.iter().map(|i| i.key()).collect();
        assert_eq!(keys, vec![key(1)]);
        // Key 2 ended lost, blocked; key 3, stored, ended with no loss.
        assert!(c.server.lease(&InfoHash(key(2))).is_none());
        assert!(c.server.lease(&InfoHash(key(3))).is_none());
        let reported: Vec<([u8; 20], u8)> = losses
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|&(k, _, r)| (k, r))
            .collect();
        assert_eq!(reported, vec![(key(2), 7)]);
        let s = c.server.snapshot();
        assert_eq!((s.regrants, s.leases, s.pending), (1, 1, 0));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_regrant_whose_check_fails_grants_nothing() {
    guard(async {
        let check = Arc::new(RegrantCheck::default());
        let c = coordinator_with(config(), Some(check.clone())).await;
        let mut b = c.local().await;
        leases_left_by_a_silent_worker(&c, &b, &[1, 2]).await;
        check.short.store(true, std::sync::atomic::Ordering::SeqCst);
        c.server.tick().await;
        // The tick has finished its re-grant: nothing went out, nothing
        // ended, and both keys wait.
        let s = c.server.snapshot();
        assert_eq!((s.regrants, s.leases, s.pending), (0, 0, 2));
        assert_eq!(s.check_failures, 1);

        // A key whose read fails waits; the other goes out.
        check
            .short
            .store(false, std::sync::atomic::Ordering::SeqCst);
        check.unknown.lock().unwrap().push(key(2));
        b.heartbeat().await.unwrap();
        c.server.tick().await;
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        let keys: Vec<[u8; 20]> = items.iter().map(|i| i.key()).collect();
        assert_eq!(keys, vec![key(1)]);
        let s = c.server.snapshot();
        assert_eq!((s.regrants, s.leases, s.pending), (1, 1, 1));
        // A key's own failed read is the store check's to count, not this.
        assert_eq!(s.check_failures, 1);

        // Once it answers, the next tick grants it.
        check.unknown.lock().unwrap().clear();
        b.heartbeat().await.unwrap();
        c.server.tick().await;
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        assert_eq!(items[0].key(), key(2));
        let s = c.server.snapshot();
        assert_eq!((s.regrants, s.leases, s.pending), (2, 2, 0));
    })
    .await;
}

/// The lock is released while the check runs; a key whose lease changed
/// meanwhile is left as it is when the lock is taken again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_that_changed_during_the_check_is_left_alone() {
    guard(async {
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let check = Arc::new(RegrantCheck::default());
        let c = coordinator_with(config(), Some(check.clone())).await;
        let vps = c.join("vps").await;
        let b = c.session(&vps).await;
        leases_left_by_a_silent_worker(&c, &b, &[1]).await;
        *check.gate.lock().unwrap() = Some((entered_tx, release_rx));
        let ticking = {
            let server = c.server.clone();
            tokio::spawn(async move { server.tick().await })
        };
        entered.await.unwrap();
        // While the check waits, the lease it would send to b expires
        // (b is still connected) and the key waits again.
        assert_eq!(
            c.server.lease(&InfoHash(key(1))).unwrap().holder,
            vps.worker()
        );
        c.advance(700);
        assert_eq!(c.server.authority().expire(c.clock.now_secs()), 1);
        release.send(()).unwrap();
        ticking.await.unwrap();
        // Not sent under the dead token, not taken from the waiting list.
        let s = c.server.snapshot();
        assert_eq!((s.regrants, s.leases, s.pending), (0, 0, 1));
        assert!(b.leases() == 0);
    })
    .await;
}

/// A re-grant cancelled while its check runs (an admin's revoke whose
/// request went away) leaves no key reserved for its target.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_regrant_leaves_no_lease_behind() {
    guard(async {
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let check = Arc::new(RegrantCheck::default());
        let c = coordinator_with(config(), Some(check.clone())).await;
        let mut b = c.local().await;
        leases_left_by_a_silent_worker(&c, &b, &[1]).await;
        *check.gate.lock().unwrap() = Some((entered_tx, release_rx));
        let ticking = {
            let server = c.server.clone();
            tokio::spawn(async move { server.tick().await })
        };
        entered.await.unwrap();
        assert_eq!(c.server.snapshot().leases, 1, "reserved for b");
        ticking.abort();
        assert!(ticking.await.unwrap_err().is_cancelled());
        let s = c.server.snapshot();
        assert_eq!((s.regrants, s.leases, s.pending), (0, 0, 1));
        // The check's thread finishes on its own; the next tick grants.
        release.send(()).unwrap();
        b.heartbeat().await.unwrap();
        c.server.tick().await;
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        assert_eq!(items[0].key(), key(1));
    })
    .await;
}

/// #615's review: a claim grants only a free key. A stored or held one is
/// denied as stored; one whose check couldn't answer as busy, so the worker
/// asks again later instead of taking it as stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_denies_an_unknown_key_as_busy() {
    guard(async {
        let check = Arc::new(RegrantCheck::default());
        check.held.lock().unwrap().push(key(2));
        check.stored.lock().unwrap().push(key(3));
        check.unknown.lock().unwrap().push(key(4));
        let c = coordinator_with(config(), Some(check.clone())).await;
        let a = c.local().await;
        let grants = a.claim(found(&*c.clock, &[1, 2, 3, 4])).await.unwrap();
        assert_eq!(grants.len(), 4);
        token_of(&grants[0]);
        let denied = |n: u8, reason| GrantItem::Denied {
            key: key(n),
            reason,
        };
        assert_eq!(grants[1], denied(2, DenyReason::Stored));
        assert_eq!(grants[2], denied(3, DenyReason::Stored));
        assert_eq!(grants[3], denied(4, DenyReason::Busy));
        // Once its check answers, the same key is granted.
        check.unknown.lock().unwrap().clear();
        let again = a.claim(found(&*c.clock, &[4])).await.unwrap();
        token_of(&again[0]);
        let s = c.server.snapshot();
        assert_eq!((s.harvested, s.leases), (2, 2));
        // Counted by cause, per key (X-wk-busy-metrics).
        assert_eq!((s.busy_unknown, s.busy_check_failed, s.busy), (1, 0, 0));
    })
    .await;
}

/// X-lease-report-atomic: a worker's floor drop (`Lost` as blocked, 7) is
/// refused to another worker's claim as soon as it's handled, with no store
/// check or sink to help: the lease authority refuses it itself. Another
/// loss leaves its key free.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_floor_drop_is_refused_to_the_next_claim() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let vps = c.join("vps").await;
        let a = c.session(&nas).await;
        let b = c.session(&vps).await;
        let grants = a.claim(found(&*c.clock, &[1, 2])).await.unwrap();
        let tokens: Vec<u64> = grants.iter().map(token_of).collect();
        a.report(vec![
            OutcomeItem {
                key: key(1),
                token: tokens[0],
                outcome: Outcome::Lost { reason: 7 },
            },
            OutcomeItem {
                key: key(2),
                token: tokens[1],
                outcome: Outcome::Lost { reason: 1 },
            },
        ])
        .await
        .unwrap();
        // A heartbeat answered means the report was handled.
        assert_eq!(a.heartbeat().await.unwrap().leases, 0);
        let other = b.claim(found(&*c.clock, &[1, 2])).await.unwrap();
        assert_eq!(
            other[0],
            GrantItem::Denied {
                key: key(1),
                reason: DenyReason::Stored
            }
        );
        token_of(&other[1]);
    })
    .await;
}

/// A store check that comes back short.
struct ShortCheck;

impl StoredKeys for ShortCheck {
    fn stored(&self, _keys: &[InfoHash]) -> Vec<bool> {
        Vec::new()
    }
}

/// A claim whose store check fails is denied whole (#607's review): it
/// fails closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_whose_check_fails_is_denied() {
    guard(async {
        let c = coordinator_with(config(), Some(Arc::new(ShortCheck))).await;
        let a = c.local().await;
        let grants = a.claim(found(&*c.clock, &[1, 2])).await.unwrap();
        assert_eq!(grants.len(), 2);
        for (g, n) in grants.iter().zip([1, 2]) {
            assert_eq!(
                *g,
                GrantItem::Denied {
                    key: key(n),
                    reason: DenyReason::Busy
                }
            );
        }
        let s = c.server.snapshot();
        assert_eq!((s.harvested, s.leases, s.check_failures), (0, 0, 1));
        // One failed check, two keys denied (X-wk-busy-metrics).
        assert_eq!((s.busy_check_failed, s.busy_unknown, s.busy), (2, 0, 0));
    })
    .await;
}

/// Admits at most this many new leases per claim (§19.5 backpressure).
struct AdmitAtMost(usize);

impl GrantAdmission for AdmitAtMost {
    fn admit(&self, _worker: WorkerId, wanted: usize) -> usize {
        wanted.min(self.0)
    }
}

/// X-wk-busy-metrics: keys over what the admission allows are denied as
/// busy and counted as held back, per key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keys_held_back_are_counted_as_busy() {
    guard(async {
        let c = coordinator_hooks(
            config(),
            None,
            Arc::new(NoEvents),
            Some(Arc::new(AdmitAtMost(1))),
        )
        .await;
        let vps = c.join("vps").await;
        let a = c.session(&vps).await;
        let grants = a.claim(found(&*c.clock, &[1, 2, 3])).await.unwrap();
        assert_eq!(grants.len(), 3);
        token_of(&grants[0]);
        for (g, n) in grants[1..].iter().zip([2, 3]) {
            assert_eq!(
                *g,
                GrantItem::Denied {
                    key: key(n),
                    reason: DenyReason::Busy
                }
            );
        }
        let s = c.server.snapshot();
        assert_eq!((s.busy, s.busy_check_failed, s.busy_unknown), (2, 0, 0));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stored_keys_are_denied_without_a_lease() {
    guard(async {
        let c = coordinator_with(config(), Some(Arc::new(HighKeysStored))).await;
        let a = c.local().await;
        let grants = a.claim(found(&*c.clock, &[1, 200])).await.unwrap();
        token_of(&grants[0]);
        assert_eq!(
            grants[1],
            GrantItem::Denied {
                key: key(200),
                reason: DenyReason::Stored
            }
        );
        let s = c.server.snapshot();
        assert_eq!((s.harvested, s.leases), (1, 1));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_claim_stops_the_session() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut link = c.link(&nas).await;
        link.send(ToCoordinator::Hello(Hello::new(
            nas.worker().0,
            Capabilities::default(),
        )))
        .await
        .unwrap();
        assert!(matches!(link.recv().await, Ok(Some(ToWorker::Welcome(_)))));
        assert!(matches!(link.recv().await, Ok(Some(ToWorker::Assign(_)))));
        // Source 64 doesn't exist (sources are a 64-bit set).
        let bad = ClaimItem {
            source: 64,
            ..found(&*c.clock, &[1])[0].clone()
        };
        link.send(ToCoordinator::Claim(ClaimBatch {
            batch: 1,
            items: vec![bad],
        }))
        .await
        .unwrap();
        assert_eq!(
            link.recv().await.unwrap(),
            Some(ToWorker::Stop(StopReason::Protocol))
        );
        let s = c.server.snapshot();
        assert_eq!((s.protocol_errors, s.harvested), (1, 0));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_times_are_corrected_by_the_measured_offset() {
    guard(async {
        let c = coordinator(config()).await;
        let fast = c.join("fast").await;
        let fast_clock: Arc<dyn Clock> = Arc::new(OffsetClock {
            base: c.clock(),
            offset_ms: 120_000,
        });
        let a = c.session_with(&fast, fast_clock.clone()).await;
        a.heartbeat().await.unwrap();
        a.claim(found(&*fast_clock, &[1])).await.unwrap();
        let lease = c.server.lease(&InfoHash(key(1))).unwrap();
        assert_eq!(lease.discovered_at, c.clock.now_secs());
        assert_eq!(lease.holder, fast.worker());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_a_disconnected_worker_returns_its_leases() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let pi = c.join("pi").await;
        let mut b = c.local().await;

        // Through the server.
        let a = c.session(&nas).await;
        token_of(&a.claim(found(&*c.clock, &[1])).await.unwrap()[0]);
        drop(a);
        until(&c.server, |s| {
            s.workers
                .iter()
                .all(|w| w.worker != nas.worker() || !w.connected)
        })
        .await;
        c.server.revoke(nas.worker()).await.unwrap();
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        assert_eq!(items[0].key(), key(1));

        // From another process, picked up by the next tick.
        let p = c.session(&pi).await;
        token_of(&p.claim(found(&*c.clock, &[2])).await.unwrap()[0]);
        drop(p);
        until(&c.server, |s| {
            s.workers
                .iter()
                .all(|w| w.worker != pi.worker() || !w.connected)
        })
        .await;
        let (mut cli, mut store, _secrets) = c.other_process();
        cli.revoke(&mut store, pi.worker()).unwrap();
        drop((cli, store));
        b.heartbeat().await.unwrap();
        c.server.tick().await;
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        assert_eq!(items[0].key(), key(2));
        let s = c.server.snapshot();
        assert_eq!((s.leases, s.pending, s.regrants), (2, 0, 2));
        assert_eq!(b.leases(), 2);
    })
    .await;
}

/// A store check that waits until the test lets it go.
struct HeldCheck {
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl StoredKeys for HeldCheck {
    fn stored(&self, keys: &[InfoHash]) -> Vec<bool> {
        if let Some(tx) = self.entered.lock().unwrap().take() {
            let _ = tx.send(());
        }
        let _ = self.release.lock().unwrap().recv();
        vec![false; keys.len()]
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_waiting_on_the_store_check_gets_nothing_once_revoked() {
    guard(async {
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let check = HeldCheck {
            entered: std::sync::Mutex::new(Some(entered_tx)),
            release: std::sync::Mutex::new(release_rx),
        };
        let c = coordinator_with(config(), Some(Arc::new(check))).await;
        let nas = c.join("nas").await;
        let a = Arc::new(c.session(&nas).await);
        let claiming = {
            let a = a.clone();
            let items = found(&*c.clock, &[1]);
            tokio::spawn(async move { a.claim(items).await })
        };
        entered.await.unwrap();
        c.server.revoke(nas.worker()).await.unwrap();
        release.send(()).unwrap();
        // Either the denial arrives before the stop, or the session ends
        // first; never a grant.
        if let Ok(items) = claiming.await.unwrap() {
            assert_eq!(
                items[0],
                GrantItem::Denied {
                    key: key(1),
                    reason: DenyReason::Draining
                }
            );
        }
        let s = c.server.snapshot();
        assert_eq!((s.harvested, s.leases, s.pending), (0, 0, 0));
        assert!(c.server.lease(&InfoHash(key(1))).is_none());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_survives_a_reconnect_until_resumed() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let mut a = c.session(&nas).await;
        assert!(c.server.drain(nas.worker()));
        assert_eq!(event(&mut a).await, WorkerEvent::Drain);
        drop(a);

        let mut a = c.session(&nas).await;
        assert_eq!(event(&mut a).await, WorkerEvent::Drain, "asked again");
        let denied = a.claim(found(&*c.clock, &[1])).await.unwrap();
        assert!(matches!(
            denied[0],
            GrantItem::Denied {
                reason: DenyReason::Draining,
                ..
            }
        ));

        assert!(c.server.resume(nas.worker()));
        assert!(!c.server.resume(nas.worker()));
        token_of(&a.claim(found(&*c.clock, &[1])).await.unwrap()[0]);
    })
    .await;
}

/// A local worker that gives up on replies after 300 ms, behind a store
/// check the test releases one claim at a time.
async fn impatient() -> (
    Coord,
    WorkerSession,
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let check = HeldCheck {
        entered: std::sync::Mutex::new(Some(entered_tx)),
        release: std::sync::Mutex::new(release_rx),
    };
    let c = coordinator_with(config(), Some(Arc::new(check))).await;
    let config = WorkerConfig {
        reply_timeout: Duration::from_millis(300),
        ..manual()
    };
    let a = WorkerSession::start(
        c.server.connect_local(),
        Hello::new(0, Capabilities::default()),
        c.clock(),
        config,
    )
    .await
    .unwrap();
    (c, a, entered, release)
}

/// Every event that arrives within a short wait, `Assign` aside.
async fn quiet_events(session: &mut WorkerSession) -> Vec<WorkerEvent> {
    let mut out = Vec::new();
    while let Ok(Some(e)) =
        tokio::time::timeout(Duration::from_millis(300), session.next_event()).await
    {
        if !matches!(e, WorkerEvent::Assign(_)) {
            out.push(e);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_reply_after_its_caller_gave_up_arrives_as_an_event() {
    guard(async {
        let (c, mut a, entered, release) = impatient().await;
        let claim = a.claim(found(&*c.clock, &[1]));
        let (r, _) = tokio::join!(claim, async {
            entered.await.unwrap();
        });
        assert!(matches!(r, Err(ClientError::Timeout)));
        // No other command goes through the driver before the reply.
        release.send(()).unwrap();
        let WorkerEvent::Regranted(items) = event(&mut a).await else {
            panic!("the late grant was lost");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key(), key(1));
        assert_eq!(a.leases(), 1);
        assert_eq!(c.server.snapshot().leases, 1);
    })
    .await;
}

/// A worker session whose coordinator is the test itself, over the
/// in-process link, so the order of every message is exact.
async fn scripted() -> (CoordinatorLink, WorkerSession) {
    let (mut coord, worker) = local_pair(16);
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(1_000_000));
    let config = WorkerConfig {
        reply_timeout: Duration::from_millis(200),
        ..manual()
    };
    let hello = Hello::new(0, Capabilities::default());
    let (session, ()) = tokio::join!(
        async {
            WorkerSession::start(worker, hello, clock, config)
                .await
                .unwrap()
        },
        async {
            assert!(matches!(
                coord.recv().await,
                Ok(Some(ToCoordinator::Hello(_)))
            ));
            coord
                .send(ToWorker::Welcome(Welcome {
                    major: PROTOCOL_MAJOR,
                    minor: PROTOCOL_MINOR,
                    worker: 0,
                    heartbeat_ms: 5_000,
                    lease_ttl_secs: 600,
                    renew_now: false,
                }))
                .await
                .unwrap();
        }
    );
    (coord, session)
}

async fn claimed(coord: &mut CoordinatorLink) -> u64 {
    match coord.recv().await.unwrap() {
        Some(ToCoordinator::Claim(c)) => c.batch,
        other => panic!("expected a claim: {other:?}"),
    }
}

fn granted(batch: u64, n: u8, token: u64) -> ToWorker {
    ToWorker::Grants(GrantBatch {
        batch,
        items: vec![GrantItem::Granted {
            key: key(n),
            token,
            ttl_secs: 600,
        }],
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retried_claim_hands_the_key_over_once() {
    guard(async {
        let (mut coord, mut a) = scripted().await;
        let clock = ManualClock::new(0);

        // The late reply arrives while the retry is outstanding: only the
        // retry's reply hands the key over.
        let first = a.claim(found(&clock, &[1]));
        let (r, b1) = tokio::join!(first, claimed(&mut coord));
        assert!(matches!(r, Err(ClientError::Timeout)));
        let retry = a.claim(found(&clock, &[1]));
        let (r, ()) = tokio::join!(retry, async {
            let b2 = claimed(&mut coord).await;
            coord.send(granted(b1, 1, 77)).await.unwrap();
            coord.send(granted(b2, 1, 77)).await.unwrap();
        });
        assert_eq!(token_of(&r.unwrap()[0]), 77);
        assert!(quiet_events(&mut a).await.is_empty());
        assert_eq!(a.leases(), 1);

        // A repeated late reply for a key already held under that token
        // isn't handed over again.
        coord.send(granted(b1, 1, 77)).await.unwrap();
        assert!(quiet_events(&mut a).await.is_empty());

        // Two claims for one key both time out: their late replies hand
        // it over exactly once.
        let lost = a.claim(found(&clock, &[3]));
        let (r, b4) = tokio::join!(lost, claimed(&mut coord));
        assert!(matches!(r, Err(ClientError::Timeout)));
        let lost = a.claim(found(&clock, &[3]));
        let (r, b5) = tokio::join!(lost, claimed(&mut coord));
        assert!(matches!(r, Err(ClientError::Timeout)));
        coord.send(granted(b4, 3, 79)).await.unwrap();
        coord.send(granted(b5, 3, 79)).await.unwrap();
        assert_eq!(
            quiet_events(&mut a).await,
            vec![WorkerEvent::Regranted(vec![GrantItem::Granted {
                key: key(3),
                token: 79,
                ttl_secs: 600
            }])]
        );
        assert_eq!(a.leases(), 2);

        // The late reply arrives with no retry: it's an event, once.
        let lost = a.claim(found(&clock, &[2]));
        let (r, b3) = tokio::join!(lost, claimed(&mut coord));
        assert!(matches!(r, Err(ClientError::Timeout)));
        coord.send(granted(b3, 2, 78)).await.unwrap();
        assert_eq!(
            quiet_events(&mut a).await,
            vec![WorkerEvent::Regranted(vec![GrantItem::Granted {
                key: key(2),
                token: 78,
                ttl_secs: 600
            }])]
        );
        assert_eq!(a.leases(), 3);
    })
    .await;
}

// Lease reconciliation after a reconnect (WK-11b).

fn held(k: u8, token: u64) -> HeldLease {
    HeldLease { key: key(k), token }
}

fn done(returned: u32, unknown: u32, leases: u32) -> Option<ReconcileAnswer> {
    Some(ReconcileAnswer::Done {
        returned,
        unknown,
        leases,
    })
}

/// A worker that reconnected lists what it still holds. The coordinator
/// gives back the leases from earlier sessions the list leaves out, as an
/// expiry would (granted again under new tokens, harvested once), keeps
/// the ones handed to it on this session, and counts listed leases it
/// doesn't have without creating them. The counts then agree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnected_worker_gets_back_only_the_leases_its_list_leaves_out() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let first = c.session(&nas).await;
        let grants = first.claim(found(&*c.clock, &[1, 2, 3])).await.unwrap();
        let t: Vec<u64> = grants.iter().map(token_of).collect();
        // The connection drops; the worker comes back knowing only key 1.
        drop(first);
        let mut a = c.session(&nas).await;
        // Its own claim on key 3, answered with the lease on this session,
        // and a new key: both on their way, or here, when it lists.
        let again = a.claim(found(&*c.clock, &[3, 4])).await.unwrap();
        assert_eq!(token_of(&again[0]), t[2]);
        assert_eq!(a.heartbeat().await.unwrap().leases, 4);
        let answer = a
            .holding(vec![held(1, t[0]), held(9, 12_345)])
            .await
            .unwrap();
        assert_eq!(answer, done(1, 1, 3));
        let WorkerEvent::Regranted(items) = event(&mut a).await else {
            panic!("no re-grant");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key(), key(2));
        assert_ne!(token_of(&items[0]), t[1]);
        assert_eq!(c.server.lease(&InfoHash(key(9))), None, "never created");
        let s = c.server.snapshot();
        assert_eq!((s.reconcile_returned, s.reconcile_unknown), (1, 1));
        assert_eq!((s.harvested, s.regrants, s.outcomes), (4, 1, 0));
        assert_eq!(a.heartbeat().await.unwrap().leases, 4);
        // One round per heartbeat interval: a second at once is refused.
        assert_eq!(
            a.holding(vec![]).await.unwrap(),
            Some(ReconcileAnswer::Refused)
        );
        c.advance(5);
        // A list of everything it holds now returns nothing.
        let all = vec![
            held(1, t[0]),
            held(2, token_of(&items[0])),
            held(3, t[2]),
            held(4, token_of(&again[1])),
        ];
        assert_eq!(a.holding(all).await.unwrap(), done(0, 0, 4));
    })
    .await;
}

/// A list can only give back its sender's own leases: another worker's
/// lease, listed or not, is never touched, and a lease whose result is in
/// the coordinator's hands (`Fetched`) stays for its commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_list_never_touches_another_workers_lease_or_a_fetched_one() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let pc = c.join("pc").await;
        let a = c.session(&nas).await;
        let b = c.session(&pc).await;
        let ta: Vec<u64> = a
            .claim(found(&*c.clock, &[1, 2]))
            .await
            .unwrap()
            .iter()
            .map(token_of)
            .collect();
        let tb = token_of(&b.claim(found(&*c.clock, &[3])).await.unwrap()[0]);
        a.report(vec![OutcomeItem {
            key: key(2),
            token: ta[1],
            outcome: Outcome::Fetched,
        }])
        .await
        .unwrap();
        a.heartbeat().await.unwrap();
        drop(a);
        let a = c.session(&nas).await;
        // b's lease, claimed under a's name.
        assert_eq!(a.holding(vec![held(3, tb)]).await.unwrap(), done(1, 1, 1));
        let lease = |k| c.server.lease(&InfoHash(key(k))).map(|l| l.token.0);
        assert_eq!(lease(3), Some(tb));
        assert_eq!(lease(2), Some(ta[1]), "fetched: kept for its commit");
        assert_ne!(lease(1), Some(ta[0]), "returned, granted again");
        let lease = c.server.lease(&InfoHash(key(3))).unwrap();
        assert_eq!(lease.holder, pc.worker());
    })
    .await;
}

/// A paused (quarantined) worker's list is refused and changes nothing;
/// once it's resumed, its list counts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_workers_list_is_refused() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let first = c.session(&nas).await;
        let t = token_of(&first.claim(found(&*c.clock, &[1])).await.unwrap()[0]);
        drop(first);
        let a = c.session(&nas).await;
        assert!(c.server.pause(nas.worker()));
        assert_eq!(
            a.holding(vec![]).await.unwrap(),
            Some(ReconcileAnswer::Refused)
        );
        assert_eq!(
            c.server.lease(&InfoHash(key(1))).map(|l| l.token.0),
            Some(t)
        );
        assert_eq!(c.server.snapshot().reconcile_refused, 1);
        assert!(c.server.resume(nas.worker()));
        assert_eq!(a.holding(vec![]).await.unwrap(), done(1, 0, 0));
    })
    .await;
}

/// The list is a snapshot. A lease committed between the list's making
/// and its landing stays committed (listed, it's counted unknown, never
/// created again); a listed lease whose result was out commits after it
/// as usual; and a lease returned and then reported lost is a stale
/// report, never a second loss.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_list_racing_commits_and_reports_loses_and_double_counts_nothing() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let w = nas.worker();
        let first = c.session(&nas).await;
        let t: Vec<u64> = first
            .claim(found(&*c.clock, &[1, 2, 3]))
            .await
            .unwrap()
            .iter()
            .map(token_of)
            .collect();
        drop(first);
        let a = c.session(&nas).await;
        let authority = c.server.authority();
        let k = |n| InfoHash(key(n));
        // Key 1's batch commits while the list is on its way.
        assert!(authority.commit(w, &k(1), Token(t[0])).is_some());
        let answer = a.holding(vec![held(1, t[0]), held(3, t[2])]).await.unwrap();
        assert_eq!(answer, done(1, 1, 1));
        assert_eq!(c.server.lease(&k(1)), None, "committed, not created");
        // Key 3's result was out when the list was built: it commits.
        assert!(authority.commit(w, &k(3), Token(t[2])).is_some());
        // Key 2 went back; the worker's late loss report (`PeerFound`) is
        // stale.
        let before = c.server.snapshot();
        a.report(vec![fetched(key(2), t[1])]).await.unwrap();
        a.heartbeat().await.unwrap();
        let s = c.server.snapshot();
        assert_eq!(s.stale_reports, before.stale_reports + 1);
        assert_eq!(s.outcomes, before.outcomes);
        assert_eq!(s.harvested, 3);
    })
    .await;
}

/// A long list goes in parts. Parts out of order, or of a round the
/// session never started, are refused once, at the round's last part.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_list_goes_in_parts_and_parts_out_of_order_are_refused() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let a = c.session(&nas).await;
        let many: Vec<HeldLease> = (0..MAX_HOLDING_BATCH as u64 + 5)
            .map(|i| HeldLease {
                key: key(7),
                token: i,
            })
            .collect();
        let n = u32::try_from(many.len()).unwrap();
        assert_eq!(a.holding(many).await.unwrap(), done(0, n, 0));
        drop(a);
        c.advance(5);

        let mut link = c.link(&nas).await;
        link.send(ToCoordinator::Hello(Hello::new(
            nas.worker().0,
            Capabilities::default(),
        )))
        .await
        .unwrap();
        let part = |round, part, last| {
            ToCoordinator::Holding(Holding {
                round,
                part,
                last,
                leases: HeldList::default(),
            })
        };
        link.send(part(5, 1, true)).await.unwrap();
        link.send(part(6, 0, false)).await.unwrap();
        link.send(part(6, 2, true)).await.unwrap();
        link.send(part(7, 0, false)).await.unwrap();
        link.send(part(7, 1, true)).await.unwrap();
        let mut answers = Vec::new();
        while answers.len() < 3 {
            match tokio::time::timeout(WAIT, link.recv()).await.unwrap() {
                Ok(Some(ToWorker::Reconciled(r))) => answers.push(r),
                Ok(Some(_)) => {}
                other => panic!("{other:?}"),
            }
        }
        let refused = |round| Reconciled {
            round,
            answer: ReconcileAnswer::Refused,
        };
        assert_eq!(answers[..2], [refused(5), refused(6)]);
        assert_eq!(answers[2].round, 7);
        assert!(matches!(answers[2].answer, ReconcileAnswer::Done { .. }));
        assert_eq!(c.server.snapshot().reconcile_refused, 2);
    })
    .await;
}

/// A coordinator older than minor 1 is never sent a list: the call says
/// so and sends nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_older_coordinator_is_never_sent_a_list() {
    guard(async {
        let (mut coord, worker) = local_pair(16);
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::starting_now());
        let (session, ()) = tokio::join!(start(worker, 1, clock), async {
            assert!(matches!(
                coord.recv().await,
                Ok(Some(ToCoordinator::Hello(_)))
            ));
            coord
                .send(ToWorker::Welcome(Welcome {
                    major: PROTOCOL_MAJOR,
                    minor: 0,
                    worker: 1,
                    heartbeat_ms: 5_000,
                    lease_ttl_secs: 600,
                    renew_now: false,
                }))
                .await
                .unwrap();
        });
        let session = session.unwrap();
        assert_eq!(session.holding(vec![held(1, 1)]).await.unwrap(), None);
        // The next thing on the wire is what came after: no list.
        let stats = Stats::default();
        session.set_stats(stats).await.unwrap();
        let beat = tokio::spawn(async move {
            let _ = session.heartbeat().await;
            session
        });
        assert!(matches!(
            tokio::time::timeout(WAIT, coord.recv()).await.unwrap(),
            Ok(Some(ToCoordinator::Heartbeat(_)))
        ));
        beat.abort();
    })
    .await;
}

/// The gap before `Fetched`: the worker marked a lease's result committed
/// (its batch was accepted) and listed it before its `Fetched` report went
/// out. Listed, the lease is never given back: the mark and the commit
/// then land on it as usual, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lease_committed_before_its_fetched_report_is_listed_and_kept() {
    guard(async {
        let c = coordinator(config()).await;
        let nas = c.join("nas").await;
        let first = c.session(&nas).await;
        let t = token_of(&first.claim(found(&*c.clock, &[1])).await.unwrap()[0]);
        drop(first);
        let a = c.session(&nas).await;
        assert_eq!(a.holding(vec![held(1, t)]).await.unwrap(), done(0, 0, 1));
        a.report(vec![OutcomeItem {
            key: key(1),
            token: t,
            outcome: Outcome::Fetched,
        }])
        .await
        .unwrap();
        a.heartbeat().await.unwrap();
        let k = InfoHash(key(1));
        let w = nas.worker();
        assert!(c.server.authority().commit(w, &k, Token(t)).is_some());
        let s = c.server.snapshot();
        assert_eq!(
            (s.fetched, s.stale_reports, s.reconcile_returned),
            (1, 0, 0)
        );
        assert_eq!((s.leases, s.pending, s.regrants), (0, 0, 0));
    })
    .await;
}

/// A re-grant reserved for the worker while its store check runs is
/// marked for the worker's session before the check: a list landing
/// meanwhile, without it, leaves it, and the grant then arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_regrant_on_its_way_is_never_returned_by_a_list() {
    guard(async {
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let check = Arc::new(RegrantCheck::default());
        let c = coordinator_with(config(), Some(check.clone())).await;
        let vps = c.join("vps").await;
        let mut b = c.session(&vps).await;
        leases_left_by_a_silent_worker(&c, &b, &[1]).await;
        *check.gate.lock().unwrap() = Some((entered_tx, release_rx));
        let ticking = {
            let server = c.server.clone();
            tokio::spawn(async move { server.tick().await })
        };
        entered.await.unwrap();
        let k = InfoHash(key(1));
        let reserved = c.server.lease(&k).unwrap();
        assert_eq!(reserved.holder, vps.worker());
        assert_eq!(b.holding(vec![]).await.unwrap(), done(0, 0, 1));
        release.send(()).unwrap();
        ticking.await.unwrap();
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        assert_eq!(token_of(&items[0]), reserved.token.0);
        assert_eq!(c.server.lease(&k).map(|l| l.token), Some(reserved.token));
        assert_eq!(c.server.snapshot().reconcile_returned, 0);
    })
    .await;
}

/// A re-grant already in the session's queue when the list lands (no
/// store check: it's queued at once) is kept too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_regrant_is_never_returned_by_a_list() {
    guard(async {
        let c = coordinator(config()).await;
        let vps = c.join("vps").await;
        let mut b = c.session(&vps).await;
        leases_left_by_a_silent_worker(&c, &b, &[1]).await;
        c.server.tick().await;
        let k = InfoHash(key(1));
        let queued = c.server.lease(&k).unwrap();
        assert_eq!(queued.holder, vps.worker());
        // b hasn't read the grant: its list doesn't name it.
        assert_eq!(b.holding(vec![]).await.unwrap(), done(0, 0, 1));
        let WorkerEvent::Regranted(items) = event(&mut b).await else {
            panic!("no re-grant");
        };
        assert_eq!(token_of(&items[0]), queued.token.0);
        assert_eq!(c.server.lease(&k).map(|l| l.token), Some(queued.token));
    })
    .await;
}
