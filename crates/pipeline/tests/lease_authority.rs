//! One lease authority for the whole fleet (WK-0, architecture §19.3):
//! the pipeline's built-in worker and remote workers (through the cluster
//! server sharing the pipeline's authority) never hold the same key at
//! once, "harvested" is counted once per key, and the funnel reconciles
//! (harvested = Σ terminal + in flight). A single-machine install creates
//! no cluster records.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lodestar_cluster::proto::*;
use lodestar_cluster::{
    BatchAnswer, Claim, Claimed, Clock, ClusterServer, Coordinator, InboundSink, JoinToken,
    Joining, Lease, LeaseAuthority, LeaseEvents, ManualClock, Pki, ServerConfig, ServerHooks,
    Token, WorkerConfig, WorkerId, WorkerSession, connect, join,
};
use lodestar_integrations::SecretStore;
use lodestar_metainfo::InfoHash;
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::{
    BoxFuture, Consent, Found, Kind, Loss, Manifest, Network, PluginError, PoolConfig, Priority,
    Risk, SourceCtx, SourcePlugin, Stage,
};
use lodestar_store::{SourceId, Store, StoreConfig};
use proptest::prelude::*;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------
// The authority on its own: a property test over local and remote claims.

#[derive(Default)]
struct Counted {
    harvested: AtomicUsize,
    lost: AtomicUsize,
}

impl LeaseEvents for Counted {
    fn harvested(&self, _: &InfoHash, _: &Lease) {
        self.harvested.fetch_add(1, Ordering::Relaxed);
    }
    fn lost(&self, _: &InfoHash, _: &Lease, _: u8) {
        self.lost.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
enum Op {
    /// Worker 0 is the built-in one; 1 and 2 are remote.
    Claim {
        worker: u32,
        key: u8,
        source: u8,
    },
    Batch {
        worker: u32,
        keys: Vec<u8>,
    },
    Commit {
        key: u8,
    },
    Lose {
        key: u8,
    },
    /// A report from `worker` with whatever token it last saw for `key`.
    Report {
        worker: u32,
        key: u8,
        loss: bool,
    },
    Heartbeat {
        worker: u32,
    },
    Tick {
        secs: i64,
    },
    Expire,
    Evict {
        worker: u32,
    },
    Regrant {
        to: Vec<u32>,
        per: usize,
    },
    Requeue {
        key: u8,
    },
}

fn op() -> impl Strategy<Value = Op> {
    let worker = 0..3u32;
    prop_oneof![
        5 => (worker.clone(), 0..8u8, 0..4u8).prop_map(|(worker, key, source)| Op::Claim { worker, key, source }),
        2 => (worker.clone(), proptest::collection::vec(0..8u8, 1..6)).prop_map(|(worker, keys)| Op::Batch { worker, keys }),
        2 => (0..8u8).prop_map(|key| Op::Commit { key }),
        2 => (0..8u8).prop_map(|key| Op::Lose { key }),
        3 => (worker.clone(), 0..8u8, any::<bool>()).prop_map(|(worker, key, loss)| Op::Report { worker, key, loss }),
        2 => worker.clone().prop_map(|worker| Op::Heartbeat { worker }),
        1 => (1..400i64).prop_map(|secs| Op::Tick { secs }),
        1 => Just(Op::Expire),
        1 => (1..3u32).prop_map(|worker| Op::Evict { worker }),
        2 => (proptest::collection::vec(worker, 1..3), 1..4usize).prop_map(|(to, per)| Op::Regrant { to, per }),
        1 => (0..8u8).prop_map(|key| Op::Requeue { key }),
    ]
}

type Seen = HashMap<(u32, u8), Token>;

/// Records a grant: a token is new for its key or the same lease again,
/// and never one issued for another key.
fn note(
    worker: u32,
    k: u8,
    claimed: Claimed,
    issued: &mut HashMap<u8, Vec<Token>>,
    all: &mut HashSet<Token>,
    seen: &mut Seen,
) -> Result<(), TestCaseError> {
    if let Claimed::Granted(t) = claimed {
        let list = issued.entry(k).or_default();
        if !list.contains(&t) {
            prop_assert!(all.insert(t), "token reused across keys");
            list.push(t);
        }
        seen.insert((worker, k), t);
    }
    Ok(())
}

fn key(n: u8) -> InfoHash {
    InfoHash([n; 20])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// However local and remote claims, reports, expiries, evictions and
    /// re-grants interleave: a key has at most one current lease, a token
    /// is never issued twice, and harvested = ended + in flight.
    #[test]
    fn a_key_is_never_held_twice_and_the_funnel_reconciles(ops in proptest::collection::vec(op(), 0..150)) {
        let events = Arc::new(Counted::default());
        let a = LeaseAuthority::new(9, 600, events.clone());
        let mut now = 0i64;
        let mut ended = 0usize;
        // Every token issued per key, and the token each worker last saw.
        let mut issued: HashMap<u8, Vec<Token>> = HashMap::new();
        let mut all: HashSet<Token> = HashSet::new();
        let mut seen: Seen = HashMap::new();
        for op in ops {
            match op {
                Op::Claim { worker, key: k, source } => {
                    let c = Claim { key: key(k), source: SourceId::new(source).unwrap(), strategy: 0, at: now };
                    let claimed = a.claim(WorkerId(worker), c, now);
                    if let Claimed::Held { holder, token } = claimed {
                        prop_assert!(a.current(&key(k), token).is_some());
                        if holder == WorkerId(worker) {
                            seen.insert((worker, k), token);
                        }
                    }
                    note(worker, k, claimed, &mut issued, &mut all, &mut seen)?;
                }
                Op::Batch { worker, keys } => {
                    let claims: Vec<Claim> = keys.iter().map(|&k| Claim { key: key(k), source: SourceId::new(0).unwrap(), strategy: 0, at: now }).collect();
                    for (&k, answer) in keys.iter().zip(a.claim_batch(WorkerId(worker), &claims, now, None)) {
                        if let BatchAnswer::Claimed { claimed, .. } = answer {
                            note(worker, k, claimed, &mut issued, &mut all, &mut seen)?;
                        }
                    }
                }
                Op::Commit { key: k } => {
                    if let Some(l) = a.get(&key(k)) && a.release(&key(k), l.token).is_some() {
                        ended += 1;
                    }
                }
                Op::Lose { key: k } => {
                    if let Some(l) = a.get(&key(k)) && a.lost(&key(k), l.token, Loss::NoPeers as u8).is_some() {
                        ended += 1;
                    }
                }
                Op::Report { worker, key: k, loss } => {
                    if let Some(&t) = seen.get(&(worker, k)) {
                        let holds = a.get(&key(k)).is_some_and(|l| l.holder == WorkerId(worker) && l.token == t);
                        let r = a.report(WorkerId(worker), &key(k), t, loss.then_some(1));
                        prop_assert_eq!(r.is_some(), holds, "only the holder's current token counts");
                        if r.is_some() {
                            ended += 1;
                        }
                    }
                }
                Op::Heartbeat { worker } => a.heartbeat(WorkerId(worker), now),
                Op::Tick { secs } => now += secs,
                Op::Expire => {
                    a.expire(now);
                }
                Op::Evict { worker } => {
                    a.evict(WorkerId(worker));
                }
                Op::Regrant { to, per } => {
                    let to: Vec<WorkerId> = to.into_iter().map(WorkerId).collect();
                    for (w, keys) in a.regrant_pending(&to, per, now) {
                        for (h, t) in keys {
                            let k = h.0[0];
                            note(w.0, k, Claimed::Granted(t), &mut issued, &mut all, &mut seen)?;
                        }
                    }
                }
                Op::Requeue { key: k } => {
                    if let Some(l) = a.get(&key(k)) {
                        a.requeue(&key(k), l.token);
                    }
                }
            }
            // At most one current lease per key, and it's the newest token.
            for (k, tokens) in &issued {
                let current: Vec<&Token> = tokens.iter().filter(|t| a.current(&key(*k), **t).is_some()).collect();
                prop_assert!(current.len() <= 1, "key {} held twice: {:?}", k, current);
                if let Some(t) = current.first() {
                    prop_assert_eq!(*t, tokens.last().unwrap());
                }
            }
            let harvested = events.harvested.load(Ordering::Relaxed);
            prop_assert_eq!(harvested, ended + a.len() + a.pending(), "harvested = ended + in flight");
        }
    }
}

// ---------------------------------------------------------------------
// The real pipeline, with a remote worker over mutual TLS on its authority.

fn manifest(id: &'static str, kinds: &'static [Kind]) -> Manifest {
    Manifest {
        id,
        name: id,
        version: "1",
        kinds,
        networks: &[Network::Bt],
        endpoints: &[],
        risk: Risk::Low,
        consent: Consent::None,
        default_priority: Priority::Normal,
        enabled_by_default: true,
        always_adult: false,
        finds_private: false,
    }
}

struct Scripted {
    manifest: Manifest,
    rx: std::sync::Mutex<Option<mpsc::Receiver<Found>>>,
}

fn scripted(id: &'static str) -> (Arc<Scripted>, mpsc::Sender<Found>) {
    let (tx, rx) = mpsc::channel(64);
    let s = Scripted {
        manifest: manifest(id, &[Kind::Discovery]),
        rx: std::sync::Mutex::new(Some(rx)),
    };
    (Arc::new(s), tx)
}

impl SourcePlugin for Scripted {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        let mut rx = self.rx.lock().unwrap().take().expect("run once");
        Box::pin(async move {
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    Some(f) = rx.recv() => {
                        ctx.emitter.emit(f).await.map_err(|_| PluginError("closed".into()))?;
                    }
                }
            }
        })
    }
}

fn config() -> PipelineConfig {
    PipelineConfig {
        pool: PoolConfig {
            // Keys with no peers stay in flight for the whole test.
            lookup_wait: Duration::from_secs(600),
            ..PoolConfig::default()
        },
        flush_every: Duration::from_millis(100),
        batch_wait: Duration::from_millis(20),
        ..PipelineConfig::default()
    }
}

/// Polls with a deadline (standards §2: no fixed sleeps).
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn cluster_rows(store: &Store) -> usize {
    store.settings_with_prefix_raw("cluster.").unwrap().len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_machine_install_creates_no_cluster_records() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let (feed, feed_tx) = scripted("feed");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![feed]).await.unwrap();
    let monitor = pipeline.monitor();
    feed_tx.send(Found::key(key(1), 0)).await.unwrap();
    until("the key is leased", || {
        monitor.lease_authority().get(&key(1)).is_some()
    })
    .await;
    assert_eq!(
        monitor.lease_authority().get(&key(1)).unwrap().holder,
        WorkerId::LOCAL
    );
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(cluster_rows(&store), 0, "no CA, no server key, no worker");
}

struct Remote {
    server: Arc<ClusterServer>,
    session: WorkerSession,
    clock: Arc<ManualClock>,
}

/// A cluster server on the pipeline's authority, with one remote worker
/// joined and connected over mutual TLS on loopback.
/// Records what the server hands the built-in worker.
#[derive(Default)]
struct Sink {
    local_hints: std::sync::Mutex<Vec<Vec<HintItem>>>,
}

impl InboundSink for Sink {
    fn local_hints(&self, hints: Vec<HintItem>) {
        self.local_hints.lock().unwrap().push(hints);
    }
}

async fn remote(db: &std::path::Path, authority: Arc<LeaseAuthority>, sink: Arc<Sink>) -> Remote {
    let mut store = Store::open(db, StoreConfig::default()).unwrap();
    let secrets = SecretStore::from_key(&[3; 32]);
    let clock = Arc::new(ManualClock::starting_now());
    let coordinator = Coordinator::open(&mut store, &secrets, clock.now_secs()).unwrap();
    let pki = Pki {
        coordinator,
        store,
        secrets,
    };
    let hooks = ServerHooks {
        authority,
        stored: None,
        sink: Some(sink),
        admission: None,
    };
    let config = ServerConfig {
        auto_tick: false,
        ..ServerConfig::default()
    };
    let server = ClusterServer::with_hooks(pki, clock.clone(), config, hooks).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(server.clone().serve(listener));
    let dyn_clock: Arc<dyn Clock> = clock.clone();
    let token = server.create_join_token("nas").await.unwrap();
    let joining = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
    let creds = join(&addr, joining, dyn_clock.clone(), WAIT).await.unwrap();
    let link = connect(&addr, &creds, dyn_clock.clone(), WAIT)
        .await
        .unwrap();
    let config = WorkerConfig {
        auto_heartbeat: false,
        ..WorkerConfig::default()
    };
    let session = WorkerSession::start(
        link,
        Hello::new(creds.worker().0, Capabilities::default()),
        dyn_clock,
        config,
    )
    .await
    .unwrap();
    Remote {
        server,
        session,
        clock,
    }
}

fn item(clock: &ManualClock, n: u8) -> ClaimItem {
    ClaimItem {
        key: [n; 20],
        // The feed plugin is source 0.
        source: 5,
        strategy: 0,
        discovered_ms: clock.now_ms(),
        hints: vec!["192.0.2.1:6881".parse().unwrap()],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_and_remote_workers_share_one_authority_and_the_funnel_reconciles() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let (feed, feed_tx) = scripted("feed");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![feed]).await.unwrap();
    let monitor = pipeline.monitor();
    let authority = monitor.lease_authority().clone();
    let stats = monitor.lease_stats();
    let granted = || stats.granted.load(Ordering::Relaxed);

    // The built-in worker finds key 1.
    feed_tx.send(Found::key(key(1), 0)).await.unwrap();
    until("key 1 leased locally", || authority.get(&key(1)).is_some()).await;

    let sink = Arc::new(Sink::default());
    let r = remote(&db, authority.clone(), sink.clone()).await;
    // The remote worker finds keys 1 (held locally), 2 and 3.
    let grants = r
        .session
        .claim(vec![
            item(&r.clock, 1),
            item(&r.clock, 2),
            item(&r.clock, 3),
        ])
        .await
        .unwrap();
    assert_eq!(
        grants[0],
        GrantItem::Denied {
            key: [1; 20],
            reason: DenyReason::HeldElsewhere
        }
    );
    let token = |g: &GrantItem| match g {
        GrantItem::Granted { token, .. } => *token,
        other => panic!("{other:?}"),
    };
    let (t2, t3) = (token(&grants[1]), token(&grants[2]));
    // Its peer hint for key 1 went to the built-in worker, in memory.
    let local = authority.get(&key(1)).unwrap();
    assert_eq!(
        *sink.local_hints.lock().unwrap(),
        vec![vec![HintItem {
            key: [1; 20],
            token: local.token.0,
            hints: vec!["192.0.2.1:6881".parse().unwrap()],
        }]]
    );
    assert_eq!(authority.get(&key(1)).unwrap().holder, WorkerId::LOCAL);
    let remote_id = WorkerId(r.session.welcome().worker);
    assert_eq!(authority.get(&key(2)).unwrap().holder, remote_id);
    assert_eq!(granted(), 3, "each key harvested once, whoever got it");

    // The built-in worker finds key 2 too: held remotely, not harvested
    // again and not leased twice.
    feed_tx.send(Found::key(key(2), 1)).await.unwrap();
    feed_tx.send(Found::key(key(4), 0)).await.unwrap();
    until("key 4 leased locally", || authority.get(&key(4)).is_some()).await;
    assert_eq!(authority.get(&key(2)).unwrap().holder, remote_id);
    let feed_bit = 1u64 << pipeline.sources()[0].id.get();
    assert_eq!(
        authority.get(&key(2)).unwrap().sources,
        (1 << 5) | feed_bit,
        "the local source merged into the remote lease"
    );
    assert_eq!(granted(), 4);

    // The remote worker loses key 2 and fetches key 3; a stale report and a
    // second report change nothing.
    r.session
        .report(vec![
            OutcomeItem {
                key: [2; 20],
                token: t2,
                outcome: Outcome::Lost {
                    reason: Loss::Unreachable as u8,
                },
            },
            OutcomeItem {
                key: [3; 20],
                token: t3,
                outcome: Outcome::Fetched,
            },
            OutcomeItem {
                key: [2; 20],
                token: t2,
                outcome: Outcome::Lost { reason: 1 },
            },
        ])
        .await
        .unwrap();
    r.session.heartbeat().await.unwrap();
    let s = r.server.snapshot();
    assert_eq!(
        (s.harvested, s.outcomes, s.fetched, s.stale_reports),
        (2, 1, 1, 1)
    );
    // Fetched isn't the end: key 3's lease stays for its commit under the
    // same token. Keys 1 and 4 are in flight locally.
    assert!(authority.current(&key(3), Token(t3)).is_some());
    assert_eq!(authority.len() + authority.pending(), 3);
    let in_flight = stats.outstanding.load(Ordering::Relaxed);
    assert_eq!(in_flight, 3);
    r.server.shutdown();

    let store = pipeline.shutdown().await.unwrap();
    let total = |stage: Stage, reason: Option<u8>| -> i64 {
        store
            .cohorts(0, i64::MAX)
            .unwrap()
            .iter()
            .filter(|c| c.stage == stage as u8 && reason.is_none_or(|r| c.reason == r))
            .map(|c| c.count)
            .sum()
    };
    // harvested = Σ terminal (stored + lost, from the store) + in flight.
    assert_eq!(total(Stage::Harvested, None), 4);
    assert_eq!(total(Stage::Lost, Some(Loss::Unreachable as u8)), 1);
    assert_eq!(
        total(Stage::Harvested, None),
        total(Stage::Stored, None) + total(Stage::Lost, None) + in_flight as i64
    );
}
