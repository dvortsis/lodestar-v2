//! The hot-path bench (B-log1b): what an event costs on the DHT and fetch
//! paths, off and on, and the numbers that decide which of their events
//! may be on by default.
//!
//! Ignored tests, run in release by one CI step:
//!
//! ```text
//! cargo test --release -p lodestar --bin lodestar logging::bench -- --ignored --nocapture
//! ```
//!
//! With `LODESTAR_LOG_BENCH_GATE=1` the gates are asserts; without it the
//! numbers are only printed. The cases keep their names so later slices
//! can re-run them:
//! - `hot_off`: (a) an event that's off, one thread: the switch's load and
//!   the counter's increment. Gate: at most 5 ns.
//! - `hot_contended`: (b) the same with 1, 4 and 8 threads, on adjacent
//!   events and on one event, with `Event`'s padded, striped counter and
//!   with the unpadded layout it replaced. Gate: padded, 8 threads on
//!   adjacent events, at most twice (a) per event, (a) as this harness
//!   measures it with one thread.
//! - `hot_on`: (c) an event that's on, carrying a `Torrent`, a `Peer` and
//!   a `Label` as the DHT and fetch events will, through a `Runtime` under
//!   Private: the console (to a sink), the file in a temp folder, the ring
//!   and the fold tables. The same line each time, so folding takes it.
//! - `hot_distinct`: (d) as (c) with a new hash each time, which folding
//!   can't reduce; then each high-volume event's verdict at its peak rate
//!   under [`verdict`].

use std::hint::black_box;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use lodestar_log::{Event, Peer, Record, Sink, Torrent};

use super::state::Applied;
use super::{Runtime, file, unix_now};
use crate::settings::logging::Logging;

/// Whether the gates are asserts (`LODESTAR_LOG_BENCH_GATE=1`).
fn gating() -> bool {
    std::env::var("LODESTAR_LOG_BENCH_GATE").is_ok_and(|v| v == "1")
}

/// (a)'s gate: an event that's off costs at most this.
const OFF_GATE_NS: f64 = 5.0;
/// (b)'s gate: padded, 8 threads on adjacent events, at most this many
/// times (a).
const CONTENDED_GATE: f64 = 2.0;

/// The `default_on` rule's bounds (B-log1b, runner's refinement §2.19):
/// at its peak rate, an event on by default costs at most 1% of one core
/// and adds at most the default folder cap's daily share to the file
/// (256 MiB over 14 days, about 18 MiB a day).
const CPU_SHARE: f64 = 0.01;
const DAILY_BYTES: f64 = 256.0 * 1024.0 * 1024.0 / 14.0;
/// Over either bound by this factor, an event would swamp Detailed too:
/// it's `Trace`, joining only at Everything, with a periodic summary.
const SWAMP: f64 = 10.0;

/// Where an event belongs under the `default_on` rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Within both bounds: may be on by default.
    DefaultOn,
    /// Over a bound: `default_on: false`, joining at Detailed.
    Detailed,
    /// Over a bound by [`SWAMP`]: `Trace`, joining at Everything only,
    /// with a periodic `Info` summary on by default instead.
    Trace,
}

/// An event's place, from its peak rate (lines a second) and the bench's
/// cost of a distinct line ((d): nanoseconds and bytes).
pub fn verdict(rate: f64, ns_per_line: f64, bytes_per_line: f64) -> Verdict {
    let cpu = rate * ns_per_line / 1e9 / CPU_SHARE;
    let disk = rate * bytes_per_line * 86_400.0 / DAILY_BYTES;
    let over = cpu.max(disk);
    if over <= 1.0 {
        Verdict::DefaultOn
    } else if over <= SWAMP {
        Verdict::Detailed
    } else {
        Verdict::Trace
    }
}

/// The peak rates (lines a second) of the events B-log1b adds on the DHT
/// and fetch paths, from the code:
/// - the crawler's Aggressive intensity sends 1,000 queries a second
///   (`lodestar_dht::Intensity::queries_per_sec`), half of them
///   `sample_infohashes` (`SAMPLE_SHARE`); a reply fits a 1,500-byte
///   datagram, so at most about 70 samples: 35,000 sampled hashes a
///   second at most;
/// - `get_peers` and `announce_peer` arrive from other nodes: the actor
///   sizes for 2,000 replies a second at Aggressive (`CONTACTED_CAPACITY`);
/// - fetches: 1,000 at once at Aggressive (`Crawler::fetch_sockets`); a
///   refused connection fails in well under the 4 s connect timeout, so
///   with 50 ms an attempt, 20,000 failures a second at most.
pub const PEAKS: &[(&str, f64)] = &[
    ("dht.key_harvested (sample)", 35_000.0),
    ("dht.key_harvested (get_peers, announce)", 2_000.0),
    ("fetch.failed", 20_000.0),
];

mod events {
    lodestar_log::catalogue! {
        "bench";
        OFF {
            id: "bench.off",
            component: Dht,
            level: Debug,
            default_on: false,
            message: "off",
            fields: { torrent: Hash },
        }
        ON {
            id: "bench.on",
            component: Fetch,
            level: Info,
            message: "Metadata fetch failed",
            fields: { torrent: Hash, peer: Peer, attempt: Label },
        }
    }
}

/// Eight events next to each other, as a catalogue's statics sit.
static ADJACENT: [Event; 8] = [const {
    Event::declare(
        "bench.adjacent",
        lodestar_log::Component::Dht,
        lodestar_log::Level::Debug,
        "adjacent",
        false,
        false,
        None,
        &[],
    )
}; 8];

/// The layout `Event` had before B-log1b: the switch and the counter
/// side by side, and the next event's right after. Bench-only.
struct Unpadded {
    on: AtomicBool,
    count: AtomicU64,
}

impl Unpadded {
    /// What `log_event!` does for an event that's off.
    #[inline]
    fn hit(&self) {
        self.count.fetch_add(1, Ordering::Relaxed);
        if self.on.load(Ordering::Relaxed) {
            unreachable!();
        }
    }
}

static UNPADDED: [Unpadded; 8] = [const {
    Unpadded {
        on: AtomicBool::new(false),
        count: AtomicU64::new(0),
    }
}; 8];

/// What `log_event!` does for an event that's off: the counter, then the
/// switch.
#[inline]
fn hit_off(e: &Event) {
    e.hit();
    if e.is_on() {
        unreachable!();
    }
}

/// Nanoseconds per event of `per_thread` events on each of `threads`
/// threads: the wall time over the events each core ran, so a runner
/// with fewer cores than threads isn't charged for time-slicing.
fn contended(threads: usize, per_thread: u64, f: impl Fn(usize) + Sync) -> f64 {
    let start = std::sync::Barrier::new(threads + 1);
    let t = std::thread::scope(|s| {
        for i in 0..threads {
            let (f, start) = (&f, &start);
            s.spawn(move || {
                start.wait();
                for _ in 0..per_thread {
                    f(black_box(i));
                }
            });
        }
        start.wait();
        Instant::now()
    });
    let wall = t.elapsed();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let parallel = threads.min(cores) as f64;
    wall.as_nanos() as f64 * parallel / (threads as f64 * per_thread as f64)
}

/// The least of a few runs, against the noise of a shared runner.
fn best(runs: usize, mut f: impl FnMut() -> f64) -> f64 {
    (0..runs).map(|_| f()).fold(f64::INFINITY, f64::min)
}

/// (a): one thread, one event that's off, through `log_event!`.
fn off_ns() -> f64 {
    const N: u64 = 50_000_000;
    let h = [7u8; 20];
    best(5, || {
        let t = Instant::now();
        for _ in 0..N {
            lodestar_log::log_event!(events::OFF, torrent = Torrent(black_box(&h)));
        }
        t.elapsed().as_nanos() as f64 / N as f64
    })
}

#[test]
#[ignore = "bench: run in release with --ignored"]
fn hot_off() {
    assert!(!events::OFF::EVENT.is_on());
    let before = events::OFF::EVENT.count();
    let ns = off_ns();
    assert!(
        events::OFF::EVENT.count() > before,
        "off events still count"
    );
    println!("hot_off: (a) {ns:.2} ns per event that's off (gate {OFF_GATE_NS} ns)");
    if gating() {
        assert!(ns <= OFF_GATE_NS, "(a) {ns:.2} ns > {OFF_GATE_NS} ns");
    }
}

#[test]
#[ignore = "bench: run in release with --ignored"]
fn hot_contended() {
    const N: u64 = 20_000_000;
    println!("hot_contended: (a) {:.2} ns through log_event!", off_ns());
    // (b) is held against (a) as this harness measures it (one thread,
    // the same loop and indexing), so the ratio is the contention alone.
    let (mut a, mut eight_adjacent) = (f64::NAN, f64::NAN);
    for threads in [1usize, 4, 8] {
        let padded_adjacent = best(3, || contended(threads, N, |i| hit_off(&ADJACENT[i % 8])));
        let padded_one = best(3, || contended(threads, N, |_| hit_off(&ADJACENT[0])));
        let unpadded_adjacent = best(3, || contended(threads, N, |i| UNPADDED[i % 8].hit()));
        let unpadded_one = best(3, || contended(threads, N, |_| UNPADDED[0].hit()));
        println!(
            "hot_contended: {threads} thread(s), ns per event: padded adjacent {padded_adjacent:.2}, \
             padded one {padded_one:.2}, unpadded adjacent {unpadded_adjacent:.2}, unpadded one {unpadded_one:.2}"
        );
        match threads {
            1 => a = padded_adjacent,
            8 => eight_adjacent = padded_adjacent,
            _ => {}
        }
    }
    let ratio = eight_adjacent / a;
    println!(
        "hot_contended: (b) padded, 8 threads, adjacent: {ratio:.2}x (a) (gate {CONTENDED_GATE}x)"
    );
    if gating() {
        assert!(
            ratio <= CONTENDED_GATE,
            "(b) {eight_adjacent:.2} ns is {ratio:.2}x (a) {a:.2} ns"
        );
    }
}

/// A runtime as the binary has it by default (Private, the file on at
/// every level, folding on), its file in `dir`, its console a sink.
fn runtime(dir: &std::path::Path) -> Runtime {
    let l = Logging {
        file_folder: dir.join("logs").display().to_string(),
        ..Logging::default()
    };
    let rt = Runtime::new(Box::new(std::io::sink()), Applied::compute(&l, unix_now()));
    rt.open_file();
    rt
}

/// Records `n` lines of `bench.on` through `rt`'s `Sink`, each with the
/// hash `hash(i)`. Nanoseconds per line.
fn record_on(rt: &Runtime, n: u64, hash: impl Fn(u64) -> [u8; 20]) -> f64 {
    let peer: std::net::SocketAddr = "203.0.113.9:6881".parse().unwrap();
    let t = Instant::now();
    for i in 0..n {
        let h = hash(i);
        let fields = [
            ("torrent", events::ON::fields::torrent(Torrent(&h))),
            ("peer", events::ON::fields::peer(Peer(peer))),
            ("attempt", events::ON::fields::attempt("connect")),
        ];
        rt.record(&Record {
            event: &events::ON::EVENT,
            fields: black_box(&fields),
        });
    }
    t.elapsed().as_nanos() as f64 / n as f64
}

/// The current file's average line length.
fn bytes_per_line(dir: &std::path::Path) -> f64 {
    let text = std::fs::read_to_string(dir.join("logs").join(file::CURRENT)).unwrap();
    let lines = text.lines().count().max(1);
    text.len() as f64 / lines as f64
}

#[test]
#[ignore = "bench: run in release with --ignored"]
fn hot_on() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path());
    let ns = best(3, || record_on(&rt, 200_000, |_| [0x5a; 20]));
    let bytes = bytes_per_line(dir.path());
    println!(
        "hot_on: (c) {ns:.0} ns per line, the same line each time (folded), \
         {bytes:.0} bytes per written line"
    );
}

/// A hash per `i`, never repeated in a run.
fn distinct(i: u64) -> [u8; 20] {
    let mut h = [0u8; 20];
    h[..8].copy_from_slice(&i.to_le_bytes());
    h[8..16].copy_from_slice(&i.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_le_bytes());
    h
}

#[test]
#[ignore = "bench: run in release with --ignored"]
fn hot_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path());
    let mut offset = 0;
    let ns = best(3, || {
        let from = offset;
        offset += 100_000;
        record_on(&rt, 100_000, |i| distinct(from + i))
    });
    let bytes = bytes_per_line(dir.path());
    println!("hot_distinct: (d) {ns:.0} ns per distinct line, {bytes:.0} bytes per line");
    let mut out = std::io::stdout().lock();
    for (event, rate) in PEAKS {
        let cpu = rate * ns / 1e9 * 100.0;
        let mib = rate * bytes * 86_400.0 / (1024.0 * 1024.0);
        let _ = writeln!(
            out,
            "hot_distinct: {event} at {rate:.0}/s: {cpu:.1}% of a core, {mib:.0} MiB a day: {:?}",
            verdict(*rate, ns, bytes)
        );
    }
}

#[test]
fn the_rule_sorts_by_both_bounds() {
    // 1,000 ns and 200 bytes a line: 10 a second is within both bounds.
    assert_eq!(verdict(10.0, 1_000.0, 200.0), Verdict::DefaultOn);
    // 100 a second is 0.01% CPU but 1.6 GiB a day.
    assert_eq!(verdict(100.0, 1_000.0, 200.0), Verdict::Trace);
    // 5 a second: 82 MiB a day, under 10x the share.
    assert_eq!(verdict(5.0, 1_000.0, 200.0), Verdict::Detailed);
    // CPU alone: 20,000 lines of 1 µs is 2% of a core.
    assert_eq!(verdict(20_000.0, 1_000.0, 0.0), Verdict::Detailed);
    const { assert!(DAILY_BYTES > 18.0 * 1024.0 * 1024.0 && DAILY_BYTES < 19.0 * 1024.0 * 1024.0) };
}
