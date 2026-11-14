//! The fetch pool's byte budget under random traffic, in paused time.
//!
//! Each seed runs a minute of claims against one [`Budget`]: small whole
//! claims, and large ones that reserve a first window and then grow, some
//! past the grown claims' cap. Every claim comes from one of a few
//! networks. Claims hold their room for a while, then become finished
//! results or are dropped; a consumer that sometimes lags takes the
//! results. Throughout, the budget's bounds are checked, and no wait for
//! room may last longer than [`MAX_WAIT`] while traffic goes on. At the
//! end every count is back to zero.
//!
//! The seeds are fixed, so a run is the same every time; a failure names
//! its seed. Every wait is the paused clock's: nothing sleeps.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::time::{sleep, timeout};

use super::{Budget, Reservation, net_of};

/// The seeds every run uses.
const SEEDS: std::ops::RangeInclusive<u64> = 1..=64;
/// The budgets the seeds run with: a test-sized one, and a worker's.
const LIMIT: usize = 1_000_000;
const WORKER: usize = 4 << 20;
/// How long new claims keep coming.
const TRAFFIC: Duration = Duration::from_secs(60);
/// The longest any wait for room may take while claims come and go.
const MAX_WAIT: Duration = Duration::from_secs(30);
const NETWORKS: u64 = 6;

/// xorshift64*.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `lo..=hi`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn net(n: u64) -> super::Net {
    net_of(std::net::IpAddr::from([198, 51, n as u8, 1])).network()
}

/// The budget's bounds, read together.
fn check(b: &Budget, seed: u64) {
    let s = b.lock();
    let beside = b.limit - b.floor();
    assert!(
        s.grown <= b.grown_cap() || s.grown_nets.len() <= 1,
        "seed {seed}: grown claims past their cap ({} in {} networks)",
        s.grown,
        s.grown_nets.len()
    );
    assert!(
        s.partial + s.grown <= beside || s.grown_nets.len() <= 1,
        "seed {seed}: large claims in the floor ({} partial, {} grown)",
        s.partial,
        s.grown
    );
    let share = beside / super::PARTIAL_NET_SHARE;
    for (n, held) in &s.partial_nets {
        assert!(
            *held <= share + super::SMALL_CLAIM,
            "seed {seed}: network {n:?} holds {held} in partial claims"
        );
    }
}

/// Waits for `reserve`, failing the seed if it takes past [`MAX_WAIT`].
async fn granted(
    what: &str,
    seed: u64,
    b: &Budget,
    sizes: (usize, usize),
    reserve: impl std::future::Future<Output = ()>,
) {
    if timeout(MAX_WAIT, reserve).await.is_err() {
        let s = b.lock();
        panic!(
            "seed {seed}: a {what} of {sizes:?} waited past {MAX_WAIT:?}: reserved {} held {} \
             partial {} small {} grown {} nets {:?} partial_nets {:?} queue {} partials {} \
             growth {:?} starved {:?} needs {:?}",
            s.reserved,
            s.held,
            s.partial,
            s.small,
            s.grown,
            s.grown_nets.len(),
            s.partial_nets,
            s.queue.len(),
            s.partials.len(),
            s.growth,
            s.starved,
            s.needs
        );
    }
}

/// One claim's life: its reservations, its hold, and its end.
/// Large dicts' sizes grow with the budget; small ones don't.
fn scale(b: &Budget) -> u64 {
    b.limit / 1_000_000
}

async fn claim(b: Arc<Budget>, seed: u64, mut rng: Rng) {
    let k = scale(&b);
    let (window, total) = if rng.chance(80) {
        let n = rng.range(1_000, 60_000) as usize;
        (n, n)
    } else if rng.chance(85) {
        (64 << 10, rng.range(100_000 * k, 400_000 * k) as usize)
    } else {
        // Past the grown claims' cap: it grows only alone.
        (64 << 10, rng.range(550_000 * k, 700_000 * k) as usize)
    };
    let net = rng.range(1, NETWORKS);
    let hold = Duration::from_millis(rng.range(50, 1_000));
    let settle = rng.chance(60);
    life(b, seed, rng, net, (window, total), hold, settle).await;
}

/// A claim from `net` of `sizes` (first window, whole), held for `hold`
/// once it has all its room, then settled as a result or dropped.
async fn life(
    b: Arc<Budget>,
    seed: u64,
    mut rng: Rng,
    net_id: u64,
    (window, total): (usize, usize),
    hold: Duration,
    settle: bool,
) {
    let r = Reservation::for_peer(b.clone(), Some(net(net_id)));
    granted(
        "first reservation",
        seed,
        &b,
        (window, total),
        r.reserve(window, total),
    )
    .await;
    if window < total {
        sleep(Duration::from_millis(rng.range(50, 800))).await;
        granted("growth", seed, &b, (total, total), r.reserve(total, total)).await;
    }
    sleep(hold).await;
    if settle {
        r.settle(r.bytes());
    }
}

/// Growth pressure: mid-sized growths from rotating networks, each
/// starting before the last has ended, so grown claims never all end at
/// once; now and then one past the cap joins them.
async fn growth_pressure(b: Arc<Budget>, seed: u64, mut rng: Rng, length: Duration) {
    let k = scale(&b);
    let start = tokio::time::Instant::now();
    let mut claims = tokio::task::JoinSet::new();
    let mut n = 0u64;
    while start.elapsed() < length {
        n += 1;
        let total = if rng.chance(4) {
            rng.range(550_000 * k, 700_000 * k)
        } else {
            rng.range(80_000 * k, 200_000 * k)
        } as usize;
        let hold = Duration::from_millis(rng.range(1_000, 1_600));
        claims.spawn(life(
            b.clone(),
            seed,
            Rng::new(rng.next()),
            n % NETWORKS + 1,
            (64 << 10, total),
            hold,
            false,
        ));
        sleep(Duration::from_millis(rng.range(450, 700))).await;
    }
    while let Some(ended) = claims.join_next().await {
        if let Err(e) = ended {
            std::panic::resume_unwind(e.into_panic());
        }
    }
}

/// Small churn: small claims arrive fast and all become results, while
/// now and then a large claim needs room the results hold.
async fn small_churn(b: Arc<Budget>, seed: u64, mut rng: Rng, length: Duration) {
    // The large claims keep their size whatever the budget: the consumer
    // keeps pace with the small ones only.
    let start = tokio::time::Instant::now();
    let mut claims = tokio::task::JoinSet::new();
    while start.elapsed() < length {
        let (sizes, hold) = if rng.chance(3) {
            (
                (64 << 10, rng.range(300_000, 500_000) as usize),
                Duration::from_millis(500),
            )
        } else {
            let n = rng.range(20_000, 60_000) as usize;
            ((n, n), Duration::from_millis(rng.range(100, 300)))
        };
        claims.spawn(life(
            b.clone(),
            seed,
            Rng::new(rng.next()),
            rng.range(1, NETWORKS),
            sizes,
            hold,
            true,
        ));
        sleep(Duration::from_millis(rng.range(40, 80))).await;
    }
    while let Some(ended) = claims.join_next().await {
        if let Err(e) = ended {
            std::panic::resume_unwind(e.into_panic());
        }
    }
}

/// Takes finished results, at a rate that changes and sometimes stops;
/// while `steady` is set (a small churn), at a steady rate that, with the
/// test-sized budget, is about as fast as small results arrive and no
/// faster.
async fn consumer(b: Arc<Budget>, mut rng: Rng, done: Arc<AtomicBool>, steady: Arc<AtomicBool>) {
    while !done.load(Ordering::Relaxed) {
        let steady = steady.load(Ordering::Relaxed);
        let pause = if !steady && rng.chance(5) {
            rng.range(500, 2_000)
        } else {
            100
        };
        sleep(Duration::from_millis(pause)).await;
        let (held, _) = b.usage();
        let k = scale(&b);
        let take = if steady {
            rng.range(55_000 * k, 95_000 * k)
        } else {
            rng.range(50_000 * k, 200_000 * k)
        };
        b.release_held(held.min(take));
    }
    let (held, _) = b.usage();
    b.release_held(held);
}

async fn run(seed: u64, limit: usize) {
    let b = Arc::new(Budget::new(limit));
    let mut rng = Rng::new(seed);
    let done = Arc::new(AtomicBool::new(false));
    // Each seed: random traffic, then one of the two patterns.
    let churn = seed.is_multiple_of(2);
    let steady = Arc::new(AtomicBool::new(false));
    let taker = tokio::spawn(consumer(
        b.clone(),
        Rng::new(seed ^ 0xC0FFEE),
        done.clone(),
        steady.clone(),
    ));
    let checker = tokio::spawn({
        let (b, done) = (b.clone(), done.clone());
        async move {
            while !done.load(Ordering::Relaxed) {
                check(&b, seed);
                sleep(Duration::from_millis(10)).await;
            }
        }
    });
    let start = tokio::time::Instant::now();
    let mut claims = tokio::task::JoinSet::new();
    while start.elapsed() < TRAFFIC {
        claims.spawn(claim(b.clone(), seed, Rng::new(rng.next())));
        sleep(Duration::from_millis(rng.range(150, 650))).await;
    }
    let pattern = Duration::from_secs(rng.range(40, 60));
    if churn {
        steady.store(true, Ordering::Relaxed);
        small_churn(b.clone(), seed, Rng::new(rng.next()), pattern).await;
        steady.store(false, Ordering::Relaxed);
    } else {
        growth_pressure(b.clone(), seed, Rng::new(rng.next()), pattern).await;
    }
    while let Some(ended) = claims.join_next().await {
        if let Err(e) = ended {
            std::panic::resume_unwind(e.into_panic());
        }
    }
    done.store(true, Ordering::Relaxed);
    taker.await.unwrap();
    if let Err(e) = checker.await {
        std::panic::resume_unwind(e.into_panic());
    }
    let s = b.lock();
    assert_eq!((s.reserved, s.held), (0, 0), "seed {seed}: bytes left");
    assert_eq!(
        (s.partial, s.small, s.grown),
        (0, 0, 0),
        "seed {seed}: counts left"
    );
    assert!(
        s.needs.is_empty() && s.grown_nets.is_empty() && s.partial_nets.is_empty(),
        "seed {seed}: maps left"
    );
    assert!(
        s.queue.is_empty() && s.partials.is_empty() && s.growth.is_empty() && s.starved.is_none(),
        "seed {seed}: waiters left"
    );
}

fn seeds(limit: usize) {
    for seed in SEEDS {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        rt.block_on(run(seed, limit));
    }
}

/// Every seed: the bounds hold throughout, no wait for room outlasts
/// [`MAX_WAIT`], and the budget ends empty.
#[test]
fn the_budget_keeps_its_bounds_and_serves_every_claim() {
    seeds(LIMIT);
}

/// The same with a worker's budget, where a network's first windows have
/// room for only a few.
#[test]
fn a_workers_budget_keeps_its_bounds_and_serves_every_claim() {
    seeds(WORKER);
}
