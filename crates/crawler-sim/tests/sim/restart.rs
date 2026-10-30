//! CR-SIM-restart: two legs of one scenario with a clean restart between
//! them, and what leg 2's samples show against the restrictions leg 1 left.
//!
//! Leg 1 starts fresh with `dht.safe_restart` on, so it suppresses samples
//! for the first-start window, then samples and shuts down cleanly (which
//! commits the restart proof). Leg 2 starts from the same data and witness
//! dirs after a gap, against the same network, which kept its state
//! (sample windows, intervals) and its record of every sample asked.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::{Config, RestartConfig};
use lodestar_dht::restart::{Clock, Reason};
use lodestar_sources::PoolConfig;
use tokio::time::{Instant, sleep, sleep_until};

use crate::log::Log;
use crate::net::{Net, SampleAsk};
use crate::rng::hash;
use crate::run::{self, Leg, Report, Seeds, T0};
use crate::scenario::{Restart, Scenario};

/// The sampler's cooldown floor and ceiling (`node::sampler::MIN_INTERVAL`
/// and `MAX_INTERVAL`, crate-private there). A reply's interval is clamped
/// to them; a timeout, an error or an unsupported reply cools for the
/// ceiling, and so does a sample still in flight at a clean stop (sealed
/// to the stop plus the ceiling plus the query timeout).
const COOLDOWN_FLOOR: Duration = Duration::from_secs(60);
const COOLDOWN_CEILING: Duration = Duration::from_secs(6 * 3600);

/// The simulated wall clock: a fixed base plus elapsed virtual time. Its
/// `mono` is tokio's, the clock the sampler keeps; both carry across the
/// legs because they run in one runtime.
#[derive(Debug)]
struct SimClock {
    start: Instant,
}

impl Clock for SimClock {
    fn wall(&self) -> Duration {
        Duration::from_secs(T0 as u64) + Instant::now().duration_since(self.start)
    }

    fn mono(&self) -> Instant {
        Instant::now()
    }
}

/// The run's data dir and witness dir, side by side in a fresh temp dir
/// (the witness must live outside the data dir), removed on drop.
struct Dirs {
    root: PathBuf,
}

impl Dirs {
    fn new(s: &Scenario) -> Dirs {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lodestar-crawler-sim-{}-{}-{}-{}",
            std::process::id(),
            s.name,
            s.seed,
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).expect("data dir");
        Dirs { root }
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The earliest a node may be sampled again after its last leg-1 sample,
/// by the sampler's rules: the reply's time plus its interval, clamped to
/// the cooldown floor and ceiling; the ceiling for an error reply; and,
/// when no reply reached the node before the stop, the ceiling from the
/// send (a timeout cools from later than that; a sealed sample from the
/// stop). A lower bound on the real deadline in every case, so a sample
/// before it is always a violation.
fn earliest_resample(ask: &SampleAsk, stop: Instant) -> Instant {
    let unanswered = ask.sent + COOLDOWN_CEILING;
    match ask.reply {
        Some((at, interval)) if at <= stop => {
            let wait = interval.map_or(COOLDOWN_CEILING, |s| {
                Duration::from_secs(s.into()).clamp(COOLDOWN_FLOOR, COOLDOWN_CEILING)
            });
            // A reply due at the stop instant may or may not have been read.
            if at == stop {
                (at + wait).min(unanswered)
            } else {
                at + wait
            }
        }
        _ => unanswered,
    }
}

/// Leg-wide sample counts, from the network's record.
#[derive(Debug, Default, PartialEq, Eq)]
struct Asks {
    sent: u64,
    /// Reached the node with a BEP 51 interval.
    answered: u64,
    /// Reached the node as a KRPC error (busy, or 204 from a non-BEP 51 node).
    errors: u64,
    /// Lost, rate-limited, dead or unknown, or still in flight at the stop.
    unanswered: u64,
}

fn count(asks: &[SampleAsk], stop: Instant) -> Asks {
    let mut c = Asks::default();
    for a in asks {
        c.sent += 1;
        match a.reply {
            Some((at, Some(_))) if at <= stop => c.answered += 1,
            Some((at, None)) if at <= stop => c.errors += 1,
            _ => c.unanswered += 1,
        }
    }
    c
}

fn secs(d: Duration) -> u64 {
    d.as_secs()
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    s: &Scenario,
    r: Restart,
    mut dht: Config,
    pool_cfg: PoolConfig,
    net: Net,
    seeds: Seeds,
    log: Arc<Mutex<Log>>,
    start: Instant,
    mut text: String,
) -> Report {
    let dirs = Dirs::new(s);
    dht.state_path = Some(dirs.root.join("data").join("dht-state.bin"));
    dht.sample_start_delay = None;
    dht.restart = Some(RestartConfig {
        witness_dir: Some(dirs.root.join("witness")),
        clock: Arc::new(SimClock { start }),
        inline_io: true,
    });
    let suppression = COOLDOWN_CEILING + dht.query_timeout;
    let hour = Duration::from_secs(3600);

    // Leg 1: a first start (full suppression), then sampling.
    let leg = Leg::start(
        dht.clone(),
        pool_cfg.clone(),
        s.swarm.fetch,
        net,
        seeds,
        &log,
        start,
        Vec::new(),
    );
    sleep_until(start + hour).await;
    let leg1_first_hour = leg.stored.load(Ordering::Relaxed);
    sleep_until(start + suppression + Duration::from_secs(u64::from(r.leg1_minutes) * 60)).await;
    let stop = Instant::now();
    let one = leg.stop().await;
    assert_eq!(
        Instant::now(),
        stop,
        "a clean shutdown takes no virtual time"
    );
    run::conservation(&one.dht, one.fetched, one.funnel_fetched, one.stored);
    let mut net = one.net;
    let mut port = one.port;
    net.drain(&mut port);
    drop(port);
    net.cut();
    let leg1 = net.asks.len();
    let c1 = count(&net.asks, stop);
    assert_eq!(
        c1.sent, one.dht.samples_sent,
        "leg 1: network saw every sample"
    );
    let in_suppression = net
        .asks
        .iter()
        .filter(|a| a.sent < start + suppression)
        .count();
    assert_eq!(
        in_suppression, 0,
        "leg 1 sampled inside its first-start suppression"
    );
    assert!(c1.answered > 0, "leg 1 never sampled after its suppression");
    let first1 = net.asks.first().map(|a| a.sent.duration_since(start));

    // Leg 2: the same dirs and network after the gap.
    sleep(Duration::from_secs(r.gap_s)).await;
    let mut dht2 = dht;
    if !r.safe_restart {
        dht2.restart = None;
    }
    let start2 = Instant::now();
    let leg = Leg::start(
        dht2,
        pool_cfg,
        s.swarm.fetch,
        net,
        Seeds {
            node: hash(&[seeds.node, 2]),
            fetch: seeds.fetch,
        },
        &log,
        start,
        one.queued,
    );
    let minutes = leg.minutes(start2, s.minutes).await;
    let end2 = Instant::now();
    let d2 = leg.node.stats();
    let stored_at_end = leg.stored.load(Ordering::Relaxed);
    let pool_end = leg.pool_end();
    let _ = writeln!(
        text,
        "\n# leg 1: a first start with dht.safe_restart on, sampling {} min after its suppression",
        r.leg1_minutes
    );
    let _ = writeln!(
        text,
        "suppression={}s samples_in_suppression={in_suppression} first_sample_at={}s reason_at_stop={} stored={} stored_first_hour={leg1_first_hour}",
        secs(suppression),
        first1.map_or(0, secs),
        one.dht.sample_suppression_reason.as_str(),
        one.stored
    );
    let _ = writeln!(
        text,
        "samples: sent={} answered={} errors={} unanswered={} sample_failures={} sample_stale={}",
        c1.sent,
        c1.answered,
        c1.errors,
        c1.unanswered,
        one.dht.sample_failures,
        one.dht.sample_stale
    );
    let _ = writeln!(
        text,
        "\n# leg 2: {}s later, dht.safe_restart {}",
        r.gap_s,
        if r.safe_restart { "on" } else { "off" }
    );
    run::write_minutes(&mut text, &minutes);
    run::write_totals(&mut text, &d2, leg.pool.stats(), &leg.funnel);
    let two = leg.stop().await;
    run::conservation(&two.dht, two.fetched, two.funnel_fetched, two.stored);
    let mut net = two.net;
    let mut port = two.port;
    net.drain(&mut port);
    run::write_net(
        &mut text,
        &net.stats,
        one.dropped + two.dropped,
        two.queued.len(),
    );

    // What leg 2's samples show against what leg 1 left.
    let mut earliest: HashMap<SocketAddr, Instant> = HashMap::new();
    for a in &net.asks[..leg1] {
        earliest.insert(a.to, earliest_resample(a, stop));
    }
    let asks2 = &net.asks[leg1..];
    let c2 = count(asks2, Instant::now());
    assert_eq!(
        c2.sent, two.dht.samples_sent,
        "leg 2: network saw every sample"
    );
    let mut first2: HashMap<SocketAddr, Instant> = HashMap::new();
    for a in asks2 {
        first2.entry(a.to).or_insert(a.sent);
    }
    let early: Vec<String> = asks2
        .iter()
        .filter(|a| earliest.get(&a.to).is_some_and(|due| a.sent < *due))
        .take(5)
        .map(|a| {
            format!(
                "{} at +{:?}, due +{:?}",
                a.to,
                a.sent.duration_since(start2),
                earliest[&a.to].saturating_duration_since(start2)
            )
        })
        .collect();
    assert!(
        early.is_empty(),
        "leg 2 sampled nodes before their restored obligation expired: {early:?}"
    );
    let restricted = earliest.values().filter(|due| **due > start2).count();
    let due: Vec<SocketAddr> = earliest
        .iter()
        .filter(|(_, d)| **d > start2 && **d <= end2)
        .map(|(a, _)| *a)
        .collect();
    let mut delays: Vec<u64> = due
        .iter()
        .filter_map(|a| first2.get(a).map(|t| secs(t.duration_since(earliest[a]))))
        .collect();
    delays.sort_unstable();
    let expired_at_restart = earliest.values().filter(|d| **d <= start2).count();
    let expired_resampled = earliest
        .iter()
        .filter(|(a, d)| **d <= start2 && first2.contains_key(*a))
        .count();
    let fresh = first2.keys().filter(|a| !earliest.contains_key(*a)).count();
    let first_sample = first2.values().min().map(|t| t.duration_since(start2));
    let (restored, pending, refused, honoured) = (
        d2.restart_obligations_restored,
        d2.restart_obligations_pending,
        d2.restart_obligations_refused,
        d2.restart_obligations_honoured,
    );
    let _ = writeln!(text, "\n# restart: leg 2 against leg 1's restrictions");
    let _ = writeln!(
        text,
        "reason={} restart_obligations_restored={restored} restart_obligations_pending={pending} restart_obligations_refused={refused} restart_obligations_honoured={honoured}",
        d2.sample_suppression_reason.as_str()
    );
    let _ = writeln!(
        text,
        "leg1_nodes_sampled={} restricted_at_restart={restricted} expired_at_restart={expired_at_restart} expired_resampled={expired_resampled}",
        earliest.len()
    );
    let _ = writeln!(
        text,
        "expiring_in_leg2={} resampled_after_expiry={} delay_after_expiry_s median={} max={}",
        due.len(),
        delays.len(),
        delays.get(delays.len() / 2).copied().unwrap_or(0),
        delays.last().copied().unwrap_or(0)
    );
    let _ = writeln!(
        text,
        "sampled_before_expiry=0 fresh_nodes_sampled={fresh} first_sample_after_start={}",
        first_sample.map_or_else(|| "none".to_string(), |d| format!("{}s", secs(d)))
    );
    let _ = writeln!(
        text,
        "samples: sent={} answered={} errors={} unanswered={} sample_failures={} sample_stale={}",
        c2.sent,
        c2.answered,
        c2.errors,
        c2.unanswered,
        two.dht.sample_failures,
        two.dht.sample_stale
    );
    let first_hour: u64 = minutes.iter().take(60).map(|m| m.stored).sum();
    let _ = writeln!(
        text,
        "restart_cost: stored in the first {} min, fresh start={leg1_first_hour} leg 2={first_hour}",
        minutes.len().min(60)
    );

    if r.safe_restart {
        assert_eq!(d2.sample_suppression_reason, Reason::Resumed, "{text}");
        assert_eq!(
            restored,
            pending + honoured,
            "restored = pending + honoured"
        );
        assert!(
            restored as usize >= restricted,
            "restored {restored} < {restricted} known restricted at the restart"
        );
        assert!(
            first_sample.is_some_and(|d| d <= Duration::from_secs(60)),
            "sampling didn't resume at once: {first_sample:?}"
        );
    } else {
        assert_eq!(d2.sample_suppression_reason, Reason::Off, "{text}");
        if end2 < start2 + suppression {
            assert!(asks2.is_empty(), "sampled inside the full suppression");
        }
    }
    drop(dirs);
    run::finish(
        text,
        &log,
        d2,
        net.stats,
        stored_at_end,
        run::steady(&minutes, |m| m.stored),
        pool_end,
    )
}
