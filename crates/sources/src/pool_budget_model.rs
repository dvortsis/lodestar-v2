//! The fetch pool's byte budget under random traffic, in paused time.
//!
//! Each seed runs a minute of claims against one [`Budget`]: small whole
//! claims, and large ones that reserve peer-wire's first request (the
//! first window and one more piece) and then grow, some past the grown
//! claims' cap, and some whose first window never comes. Every claim comes
//! from one of a few networks. Claims hold their room for a while, then
//! become finished results or are dropped; a consumer that sometimes lags
//! takes the results. Throughout, the budget's bounds are checked, and no
//! wait for room may last longer than [`MAX_WAIT`] while traffic goes on,
//! nor a large claim's first window longer than [`CHURN_WAIT`] while small
//! claims churn. At the end every count is back to zero.
//!
//! [`MAX_WAIT`] holds at both budgets at the default seeds, and at ten
//! times as many (`POOL_MODEL_SEEDS=640`). Before X-pool-past-cap-room, at
//! the test-sized budget with 640 seeds, seven first reservations waited
//! 30–44 s: a claim held back by its network's share lost its place in
//! the queue each time, to younger claims of its own network. It now keeps
//! it. `POOL_MODEL_MEASURE=1` counts waits past their bound instead of
//! failing the seed, for comparing rules.
//!
//! The seeds are fixed, so a run is the same every time; a failure names
//! its seed. Every wait is the paused clock's: nothing sleeps.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::time::{sleep, timeout};

use lodestar_peer_wire::message::METADATA_PIECE;

use super::{Budget, FetchedKey, Reservation, net_of};

/// The seeds every run uses; `POOL_MODEL_SEEDS=n` runs seeds 1 to n
/// instead (640 for a 10x run).
const SEEDS: u64 = 64;
/// The budgets the seeds run with: a test-sized one, and a worker's.
const LIMIT: usize = 1_000_000;
const WORKER: usize = 4 << 20;
/// How long new claims keep coming.
const TRAFFIC: Duration = Duration::from_secs(60);
/// The longest any wait for room may take while claims come and go.
const MAX_WAIT: Duration = Duration::from_secs(30);
/// The longest a large claim's first window may wait while small claims
/// churn: a fetch's deadline. Steady small traffic mustn't keep it out
/// (the budget's `starved` rule).
const CHURN_WAIT: Duration = Duration::from_secs(8);
const NETWORKS: u64 = 6;

/// Room for a full PEX list in a result, as peer-wire's `result_bound`
/// has it: 200 peers.
const PEX_BOUND: usize = 200 * std::mem::size_of::<std::net::SocketAddr>();
/// What peer-wire reserves first, with no client name, for a dict of more
/// than five pieces, in bytes: the window and one more piece (peer-wire's
/// `FIRST_REQUEST`, 5 pieces; CR-UTP-FIRST-WINDOW) and the result's bound.
/// It is the largest first reservation of a partial claim.
const FIRST_REQUEST_BYTES: usize = 5 * METADATA_PIECE + PEX_BOUND;
/// The same for a dict of exactly five pieces: the window alone.
const FIVE_PIECE_FIRST_BYTES: usize = 4 * METADATA_PIECE + PEX_BOUND;

/// What peer-wire reserves first for a result bound of `total` (a dict and
/// the bound, no client name): [`FIRST_REQUEST_BYTES`] for a dict over five
/// pieces, [`FIVE_PIECE_FIRST_BYTES`] for one of five, and the whole of a
/// smaller one, so no whole claim is over [`super::SMALL_CLAIM`].
/// [`peer_wire_reserves_these_first`] pins it to the real fetch, as
/// `tests/pool.rs`'s `first_reservation` is pinned there.
fn first_of(total: usize) -> usize {
    let dict = total.saturating_sub(PEX_BOUND);
    if dict > 5 * METADATA_PIECE {
        FIRST_REQUEST_BYTES
    } else if dict > 4 * METADATA_PIECE {
        FIVE_PIECE_FIRST_BYTES
    } else {
        total
    }
}

/// What a run did, per thread (`seeds` resets them, so a test's count is
/// its own whichever thread runs it). `--nocapture` prints them.
#[derive(Default, Clone, Copy, Debug)]
struct Counts {
    /// First reservations.
    claims: u64,
    growths: u64,
    /// Waits for room over 1 s.
    slow_waits: u64,
    /// The most any network held in partial claims.
    most_net_partial: u64,
    /// Waits past their bound ([`MAX_WAIT`], or [`CHURN_WAIT`] for a large
    /// claim among small churn). Always 0 unless `POOL_MODEL_MEASURE` is
    /// set: otherwise the first fails its seed.
    over_bound: u64,
    /// The longest wait for room, in ms, and its seed.
    worst_wait_ms: u64,
    worst_seed: u64,
    /// The most grown claims held together, and the most finished results
    /// held, at any check.
    most_grown: u64,
    most_held: u64,
}

thread_local! {
    static COUNTS: Cell<Counts> = Cell::new(Counts::default());
}

fn count(f: impl FnOnce(&mut Counts)) {
    COUNTS.with(|c| {
        let mut n = c.get();
        f(&mut n);
        c.set(n);
    });
}

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
    // Within the budget. A lone claim may pass it (`grant`'s `others ==
    // 0`), so one dict larger than the budget is still fetched; no claim
    // here is that large.
    assert!(
        s.reserved + s.held <= b.limit,
        "seed {seed}: {} reserved + {} held over {}",
        s.reserved,
        s.held,
        b.limit
    );
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
    // Waiting partial claims are in order of place, each in one queue,
    // and no network's apart queue is empty.
    assert!(
        s.partials
            .iter()
            .zip(s.partials.iter().skip(1))
            .all(|(x, y)| x.place < y.place),
        "seed {seed}: partial claims out of order"
    );
    let mut apart = 0;
    for q in s.apart.values() {
        assert!(!q.is_empty(), "seed {seed}: an empty apart queue");
        assert!(
            q.iter()
                .zip(q.iter().skip(1))
                .all(|(x, y)| x.place < y.place),
            "seed {seed}: an apart queue out of order"
        );
        apart += q.len();
        assert!(q.iter().all(|w| s.apart_ids.contains(&w.id)));
    }
    assert_eq!(apart, s.apart_ids.len(), "seed {seed}: apart ids");
    assert!(
        s.partials.iter().all(|w| !s.apart_ids.contains(&w.id)),
        "seed {seed}: a claim in both queues"
    );
    // Only the head of the partial claims' queue goes before whole claims.
    assert!(
        s.starved.is_none_or(|id| s.head() == Some(id)),
        "seed {seed}: a starved claim that isn't the head"
    );
    if let Some(w) = &s.wait {
        assert!(
            w.passes <= w.before,
            "seed {seed}: a waiting growth passed {} times with {} windows in when it began",
            w.passes,
            w.before
        );
    }
    // A network's first partial claim is admitted whatever its size, and
    // any later one only while the network stays within its share
    // (`net_partials_full`): so a network holds its share, or one first
    // request alone.
    count(|c| {
        c.most_grown = c.most_grown.max(s.grown);
        c.most_held = c.most_held.max(s.held);
    });
    let share = beside / super::PARTIAL_NET_SHARE;
    let one = (std::mem::size_of::<FetchedKey>() + FIRST_REQUEST_BYTES) as u64;
    for (n, held) in &s.partial_nets {
        count(|c| c.most_net_partial = c.most_net_partial.max(*held));
        assert!(
            *held <= share.max(one),
            "seed {seed}: network {n:?} holds {held} in partial claims"
        );
    }
}

/// Waits for `reserve`, failing the seed if it takes past `bound`.
async fn granted(
    what: &str,
    (seed, bound): (u64, Duration),
    b: &Budget,
    sizes: (usize, usize),
    reserve: impl std::future::Future<Output = ()>,
) {
    let asked = tokio::time::Instant::now();
    if what == "growth" {
        count(|c| c.growths += 1);
    } else {
        count(|c| c.claims += 1);
    }
    // Measuring, a wait past its bound is counted, not failed, up to ten
    // times the bound.
    let measure = std::env::var_os("POOL_MODEL_MEASURE").is_some();
    let got = timeout(if measure { bound * 10 } else { bound }, reserve).await;
    let took = asked.elapsed();
    count(|c| {
        if took > Duration::from_secs(1) {
            c.slow_waits += 1;
        }
        if got.is_ok() && took > bound {
            c.over_bound += 1;
            println!("seed {seed}: a {what} of {sizes:?} waited {took:?}");
        }
        let ms = u64::try_from(took.as_millis()).unwrap_or(u64::MAX);
        if ms > c.worst_wait_ms {
            c.worst_wait_ms = ms;
            c.worst_seed = seed;
        }
    });
    if got.is_err() {
        let s = b.lock();
        panic!(
            "seed {seed}: a {what} of {sizes:?} waited past {bound:?}: reserved {} held {} \
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
    } else if rng.chance(15) {
        // Announces close to the most a dict may be, and its first window
        // never completes: it never asks to grow, and ends when the window
        // is due.
        let most = super::largest_dict(b.limit as usize) as u64;
        let total = rng.range(most / 2, most) as usize;
        let r = Reservation::for_peer(b.clone(), Some(net(rng.range(1, NETWORKS))));
        granted(
            "first reservation",
            (seed, MAX_WAIT),
            &b,
            (first_of(total), total),
            r.reserve(first_of(total), total),
        )
        .await;
        sleep(lodestar_peer_wire::FIRST_WINDOW_DEADLINE).await;
        return;
    } else if rng.chance(85) {
        let total = rng.range(100_000 * k, 400_000 * k) as usize;
        (first_of(total), total)
    } else {
        // Past the grown claims' cap: it grows only alone.
        let total = rng.range(550_000 * k, 700_000 * k) as usize;
        (first_of(total), total)
    };
    let net = rng.range(1, NETWORKS);
    let hold = Duration::from_millis(rng.range(50, 1_000));
    let settle = rng.chance(60);
    life(b, (seed, MAX_WAIT), rng, net, (window, total), hold, settle).await;
}

/// A claim from `net` of `sizes` (first request, whole), held for `hold`
/// once it has all its room, then settled as a result or dropped. Its
/// first reservation may wait up to `first_wait`; any other, [`MAX_WAIT`].
async fn life(
    b: Arc<Budget>,
    (seed, first_wait): (u64, Duration),
    mut rng: Rng,
    net_id: u64,
    (window, total): (usize, usize),
    hold: Duration,
    settle: bool,
) {
    let r = Reservation::for_peer(b.clone(), Some(net(net_id)));
    granted(
        "first reservation",
        (seed, first_wait),
        &b,
        (window, total),
        r.reserve(window, total),
    )
    .await;
    if window < total {
        sleep(Duration::from_millis(rng.range(50, 800))).await;
        granted(
            "growth",
            (seed, MAX_WAIT),
            &b,
            (total, total),
            r.reserve(total, total),
        )
        .await;
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
            (seed, MAX_WAIT),
            Rng::new(rng.next()),
            n % NETWORKS + 1,
            (first_of(total), total),
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
        let (sizes, hold, first_wait) = if rng.chance(3) {
            let total = rng.range(300_000, 500_000) as usize;
            (
                (first_of(total), total),
                Duration::from_millis(500),
                CHURN_WAIT,
            )
        } else {
            let n = rng.range(20_000, 60_000) as usize;
            ((n, n), Duration::from_millis(rng.range(100, 300)), MAX_WAIT)
        };
        claims.spawn(life(
            b.clone(),
            (seed, first_wait),
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
        s.queue.is_empty()
            && s.partials.is_empty()
            && s.apart.is_empty()
            && s.apart_ids.is_empty()
            && s.growth.is_empty()
            && s.starved.is_none(),
        "seed {seed}: waiters left"
    );
}

fn seeds(limit: usize) {
    let n = std::env::var("POOL_MODEL_SEEDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(SEEDS);
    COUNTS.with(|c| c.set(Counts::default()));
    for seed in 1..=n {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        rt.block_on(run(seed, limit));
    }
    let counts = COUNTS.with(Cell::get);
    println!("budget {limit}, {n} seeds: {counts:?}");
    if std::env::var_os("POOL_MODEL_MEASURE").is_none() {
        assert_eq!(counts.over_bound, 0, "waits past their bound");
    }
}

/// Every seed: the bounds hold throughout, no wait for room outlasts
/// [`MAX_WAIT`], and the budget ends empty.
#[test]
fn the_budget_keeps_its_bounds_and_serves_every_claim() {
    seeds(LIMIT);
}

/// The same with a worker's budget, where a network's first requests have
/// room for only a few.
#[test]
fn a_workers_budget_keeps_its_bounds_and_serves_every_claim() {
    seeds(WORKER);
}

/// The first `(now, total)` peer-wire's real fetch reserves for a dict of
/// `size` bytes, from a scripted peer over an in-memory pipe that
/// announces `size` and the client name "Lodestar". `tests/pool.rs` has
/// the same peer for its `first_reservation`: change them together.
async fn first_reserved(size: usize) -> (usize, usize) {
    use lodestar_peer_wire::message::{self, HANDSHAKE_LEN};
    use lodestar_peer_wire::{FetchOptions, Reserve, fetch_on_stream};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let key = lodestar_metainfo::InfoHash([7; 20]);
    let (ours, mut theirs) = tokio::io::duplex(1 << 16);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = std::sync::Mutex::new(Some(tx));
    let opts = FetchOptions {
        reserve: Some(Reserve(Arc::new(move |now, total| {
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send((now, total));
            }
            // Never granted: the fetch stops here, before any request.
            Box::pin(std::future::pending())
        }))),
        ..FetchOptions::default()
    };
    let fetch = tokio::spawn(async move { fetch_on_stream(ours, &key, &opts, false).await });
    // One deadline for the whole exchange, so a fetch that stops
    // talking fails the test instead of hanging it.
    let got = timeout(Duration::from_secs(10), async {
        let mut hello = [0u8; HANDSHAKE_LEN];
        theirs.read_exact(&mut hello).await.unwrap();
        theirs
            .write_all(&message::handshake(&key.0, b"-PW0001-firstrequest", true))
            .await
            .unwrap();
        let ext = message::our_extended_handshake(Some(size));
        let mut frame = ((ext.len() + 2) as u32).to_be_bytes().to_vec();
        frame.extend([20, 0]);
        frame.extend(ext);
        theirs.write_all(&frame).await.unwrap();
        rx.await.unwrap()
    })
    .await
    .expect("peer-wire reserves at the extended handshake, within 10 s");
    fetch.abort();
    got
}

/// The model's sizes are peer-wire's: what the real fetch reserves first,
/// and in all, for dicts from 1 KiB to 8 pieces, at and either side of
/// each step, less the scripted peer's 8-byte client name.
#[tokio::test]
async fn peer_wire_reserves_these_first() {
    let name = "Lodestar".len();
    let p = METADATA_PIECE;
    for size in [1 << 10, 4 * p, 4 * p + 1, 5 * p, 5 * p + 1, 6 * p, 8 * p] {
        let (now, total) = first_reserved(size).await;
        assert_eq!(total, size + PEX_BOUND + name, "a dict of {size}: whole");
        assert_eq!(
            now,
            first_of(size + PEX_BOUND) + name,
            "a dict of {size}: first"
        );
    }
    assert_eq!(first_of(6 * p + PEX_BOUND), FIRST_REQUEST_BYTES);
    assert_eq!(first_of(5 * p + PEX_BOUND), FIVE_PIECE_FIRST_BYTES);
}
