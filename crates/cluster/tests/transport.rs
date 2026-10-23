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
use tokio::net::{TcpListener, TcpSocket, TcpStream};

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
    let server = ClusterServer::new(pki, clock.clone(), config, stored).unwrap();
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
        })
        .collect()
}

fn token_of(item: &GrantItem) -> u64 {
    match item {
        GrantItem::Granted { token, .. } => *token,
        GrantItem::Denied { .. } => panic!("denied: {item:?}"),
    }
}

fn fetched(key: [u8; 20], token: u64) -> OutcomeItem {
    OutcomeItem {
        key,
        token,
        outcome: Outcome::Fetched,
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

async fn from(ip: &str, addr: &str) -> TcpStream {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind(format!("{ip}:0").parse().unwrap()).unwrap();
    socket.connect(addr.parse().unwrap()).await.unwrap()
}

/// Whether the server closed the connection without a word.
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
            ..found(&*c.clock, &[1])[0]
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
        assert_eq!(a.leases(), 2);
    })
    .await;
}
