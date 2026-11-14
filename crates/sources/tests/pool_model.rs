//! The fetch pool under random traffic, in paused time.
//!
//! Each case is a seeded sequence of everything that reaches the pool:
//! discoveries, lookup answers, fetch results (late ones included),
//! evictions, dropped sources, policy changes, a user's interest and time
//! passing. The queue is small, so every address and network is full most
//! of the time and keys are evicted throughout. After every step the queue
//! is snapshotted, which in a debug build compares the places per address
//! and network, and the wait lists, with what the entries say. At the end
//! every key taken in must have ended once: fetched, lost, or still queued.
//!
//! The seeds are fixed, so a run is the same every time; a failure names
//! its seed and step. Every wait is the paused clock's: nothing sleeps.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_sources::pool::spawn;
use lodestar_sources::{
    BoxFuture, Discovery, Fetcher, Found, Funnel, Loss, PeerIpCap, PeerRequest, PoolConfig, PoolIo,
    Shutdown, Stage,
};
use lodestar_store::SourceId;
use tokio::sync::mpsc;
use tokio::time::sleep;

/// The seeds every run uses.
const SEEDS: std::ops::RangeInclusive<u64> = 1..=48;
/// Steps a seed.
const STEPS: usize = 400;
const QUEUE: usize = 24;
const KEYS: u64 = 60;
const T0: i64 = 1_700_000_000;

/// xorshift64*.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len() as u64) as usize]
    }
}

fn src(n: u8) -> SourceId {
    SourceId::new(n).unwrap()
}

fn key(n: u64) -> InfoHash {
    let mut h = [0u8; 20];
    h[..8].copy_from_slice(&n.to_be_bytes());
    InfoHash(h)
}

/// Twelve peers: six addresses in two networks (documentation ranges),
/// two ports each.
fn peers() -> Vec<SocketAddr> {
    (0..12u8)
        .map(|n| {
            let net = [[192, 0, 2], [198, 51, 100]][usize::from(n % 2)];
            let host = 1 + n / 4;
            let port = 1 + u16::from(n % 4 / 2);
            SocketAddr::from(([net[0], net[1], net[2], host], port))
        })
        .collect()
}

/// How each peer answers a fetch, and after how long.
type Scripts = HashMap<SocketAddr, (Duration, Result<(), FailureKind>)>;

struct Fake(Scripts);

impl Fetcher for Fake {
    fn fetch(
        &self,
        peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let (delay, result) = self.0[&peer];
        Box::pin(async move {
            sleep(delay).await;
            result.map(|()| Fetched {
                info: key.0.to_vec(),
                v2: false,
                encrypted: true,
                pex: Vec::new(),
                client: None,
            })
        })
    }
}

/// How each key's stays in the pool began and ended.
#[derive(Default)]
struct Ends {
    taken: Mutex<HashMap<InfoHash, u32>>,
    lost: Mutex<HashMap<InfoHash, u32>>,
}

impl Funnel for Ends {
    fn record(&self, _: i64, _: SourceId, _: Stage, _: Option<Loss>) {}

    fn admitted(&self, key: &InfoHash, _: i64, _: SourceId) {
        *self.taken.lock().unwrap().entry(*key).or_default() += 1;
    }

    fn lost(&self, key: &InfoHash, _: i64, _: SourceId, _: Loss, _: Option<FailureKind>) {
        *self.lost.lock().unwrap().entry(*key).or_default() += 1;
    }
}

struct Policy(HashMap<IpAddr, usize>);

impl PeerIpCap for Policy {
    fn connections_to(&self, ip: IpAddr) -> usize {
        self.0.get(&ip).copied().unwrap_or(2)
    }
}

/// A peer finder that refuses some requests, never answers some, and
/// answers the rest with a few peers after a while.
async fn finder(mut requests: mpsc::Receiver<PeerRequest>, mut rng: Rng, peers: Vec<SocketAddr>) {
    while let Some(request) = requests.recv().await {
        match rng.below(6) {
            0 => {
                let _ = request.admit.send(None);
            }
            1 => {
                let (done, rx) = tokio::sync::oneshot::channel::<Vec<SocketAddr>>();
                let _ = request.admit.send(Some(rx));
                // Held past the pool's lookup wait, then dropped.
                tokio::spawn(async move {
                    sleep(Duration::from_secs(600)).await;
                    drop(done);
                });
            }
            _ => {
                let (done, rx) = tokio::sync::oneshot::channel();
                let _ = request.admit.send(Some(rx));
                let found: Vec<SocketAddr> = (0..rng.below(4)).map(|_| rng.pick(&peers)).collect();
                let after = Duration::from_millis(rng.pick(&[1, 200, 3_000, 8_000]));
                tokio::spawn(async move {
                    sleep(after).await;
                    let _ = done.send(found);
                });
            }
        }
    }
}

async fn run(seed: u64) {
    let mut rng = Rng::new(seed);
    let peers = peers();
    let scripts: Scripts = peers
        .iter()
        .map(|p| {
            let delay = Duration::from_millis(rng.pick(&[1, 50, 2_000, 30_000]));
            let result = match rng.below(3) {
                0 => Ok(()),
                1 => Err(FailureKind::Refused),
                _ => Err(FailureKind::TimedOut),
            };
            (*p, (delay, result))
        })
        .collect();
    let config = PoolConfig {
        max_pending: QUEUE,
        max_lookups: 6,
        max_sockets: 6,
        per_ip: 2,
        max_peers_per_key: 4,
        max_attempts: 2,
        recent: 8,
        lookup_wait: Duration::from_secs(5),
        retry_backoff: Duration::from_secs(20),
        ..PoolConfig::default()
    };
    let (disc, discoveries) = mpsc::channel(4096);
    let (finder_tx, requests) = mpsc::channel(64);
    tokio::spawn(finder(requests, Rng::new(seed ^ 0xf1), peers.clone()));
    let (out_tx, mut out) = mpsc::channel(4096);
    let (stop, shutdown) = Shutdown::new();
    let ends = Arc::new(Ends::default());
    let (handle, task) = spawn(
        config,
        Arc::new(Fake(scripts)),
        PoolIo {
            discoveries,
            peer_finders: vec![finder_tx],
            out: out_tx,
            funnel: ends.clone(),
            shutdown,
        },
        HashMap::new(),
        Vec::new(),
    );
    let mut fetched: HashMap<InfoHash, u32> = HashMap::new();
    let mut dropped = [false; 4];
    for step in 0..STEPS {
        let n = rng.below(KEYS);
        let some = |rng: &mut Rng| -> Vec<SocketAddr> {
            (0..rng.below(4)).map(|_| rng.pick(&peers)).collect()
        };
        match rng.below(20) {
            0..=8 => {
                let source = 1 + rng.below(3) as u8;
                let mut found = Found::key(key(n), 0).with_peers(some(&mut rng));
                found.private_source = rng.below(8) == 0;
                found.interest = rng.below(10) == 0;
                dropped[usize::from(source)] = false;
                let d = Discovery {
                    source: src(source),
                    found,
                    at: T0,
                    reply: false,
                };
                disc.try_send(d).expect("room for a discovery");
            }
            9..=11 => {
                let d = Discovery {
                    source: src(1 + rng.below(3) as u8),
                    found: Found::key(key(n), 0).with_peers(some(&mut rng)),
                    at: T0,
                    reply: true,
                };
                disc.try_send(d).expect("room for a discovery");
            }
            12 => {
                handle.interest(key(n)).await;
            }
            13 => {
                let source = 1 + rng.below(3) as u8;
                if !dropped[usize::from(source)] {
                    dropped[usize::from(source)] = true;
                    handle.drop_source(src(source)).await;
                }
            }
            14 => {
                let policy = Policy(HashMap::from([(
                    rng.pick(&peers).ip(),
                    rng.below(3) as usize,
                )]));
                handle.set_ip_cap(Some(Arc::new(policy))).await;
            }
            15 => handle.set_ip_cap(None).await,
            _ => {
                let ms = rng.pick(&[1, 30, 700, 6_000, 25_000, 90_000]);
                sleep(Duration::from_millis(ms)).await;
            }
        }
        while let Ok(got) = out.try_recv() {
            *fetched.entry(got.key).or_default() += 1;
        }
        // The recount runs in the snapshot; a pool that died answers
        // nothing and has finished.
        let queue = handle.snapshot().await;
        assert!(
            !task.is_finished(),
            "seed {seed} step {step}: the pool stopped"
        );
        assert!(
            queue.len() <= QUEUE,
            "seed {seed} step {step}: {} queued",
            queue.len()
        );
    }
    stop.send(true).unwrap();
    let left = task
        .await
        .unwrap_or_else(|e| panic!("seed {seed}: the pool stopped: {e}"));
    while let Ok(got) = out.try_recv() {
        *fetched.entry(got.key).or_default() += 1;
    }
    // Every stay ended once: fetched, lost, or still queued at the end.
    let (taken, lost) = (ends.taken.lock().unwrap(), ends.lost.lock().unwrap());
    for n in 0..KEYS {
        let k = key(n);
        let get = |m: &HashMap<InfoHash, u32>| m.get(&k).copied().unwrap_or(0);
        let queued = left.iter().filter(|q| q.info_hash == k).count() as u32;
        assert!(queued <= 1, "seed {seed}: key {n} queued {queued} times");
        assert_eq!(
            get(&taken),
            get(&fetched) + get(&lost) + queued,
            "seed {seed}: key {n} was taken {} times, fetched {}, lost {}, left queued {queued}",
            get(&taken),
            get(&fetched),
            get(&lost),
        );
    }
}

#[tokio::test(start_paused = true)]
async fn random_traffic_keeps_the_pool_consistent() {
    for seed in SEEDS {
        run(seed).await;
    }
}
