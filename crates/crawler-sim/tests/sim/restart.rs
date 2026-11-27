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

use lodestar_dht::node::{Config, RestartConfig, testing};
use lodestar_dht::restart::{Clock, Reason};
use lodestar_sources::PoolConfig;
use tokio::time::{Instant, sleep, sleep_until};

use crate::log::Log;
use crate::net::{Net, SampleAsk};
use crate::rng::hash;
use crate::run::{self, Leg, Report, Seeds, T0};
use crate::scenario::{Restart, Scenario};

/// The sampler's cooldown floor and ceiling. A reply's interval is clamped
/// to them; a timeout, an error or an unsupported reply cools for the
/// ceiling, and so does a sample still in flight at a clean stop (sealed
/// to the stop plus the ceiling plus the query timeout). With
/// `dht.sample_backoff_escalate` on, a node's first failure in a row cools
/// for the first failure's backoff instead (a KRPC 204 still for the
/// ceiling).
const COOLDOWN_FLOOR: Duration = testing::SAMPLE_MIN_INTERVAL;
const COOLDOWN_CEILING: Duration = testing::SAMPLE_MAX_INTERVAL;
const FIRST_FAILURE: Duration = testing::SAMPLE_FIRST_FAILURE_BACKOFF;

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
/// before it is always a violation. A reply due at the stop instant may not
/// have been read, but then the sample was sealed to the stop plus the
/// ceiling, which is later than `at + wait` anyway.
///
/// `escalate` is the query timeout with `dht.sample_backoff_escalate` on,
/// `None` with it off (the model as it was). With it on, a failure (no
/// reply, or an error other than a 204) cools for the ceiling only when
/// `prev`, the node's sample before this one, failed too and this one was
/// sent within a ceiling of it: the sampler then still held the streak,
/// which lasts until the earlier failure plus the ceiling, and that
/// failure came after `prev`'s send. Otherwise it cools for the first
/// failure's backoff. That's a lower bound too, given that every reply
/// arrives within the query timeout (the scenario's slowest is 1.5 s,
/// against 4 s): a later one is read here as an answer, with its
/// interval, though the sampler timed the sample out and cooled it for
/// less. Nothing drops a streak early: a full
/// table keeps it (split off) or moves it to the spill filter as a
/// cooldown to its end (split on), and an abort keeps it. And an
/// unanswered sample sent less than the query timeout before the stop
/// can't have timed out, so it was sealed: the stop plus the ceiling.
/// (With escalation off, the send plus the ceiling is close to that.)
fn earliest_resample(
    ask: &SampleAsk,
    prev: Option<&SampleAsk>,
    stop: Instant,
    escalate: Option<Duration>,
) -> Instant {
    let failed = |a: &SampleAsk| match a.reply {
        Some((at, interval)) if at <= stop => interval.is_none() && !a.unknown,
        _ => true,
    };
    let backoff = if escalate.is_some()
        && !prev.is_some_and(|p| failed(p) && ask.sent < p.sent + COOLDOWN_CEILING)
    {
        FIRST_FAILURE
    } else {
        COOLDOWN_CEILING
    };
    match ask.reply {
        Some((at, interval)) if at <= stop => {
            at + interval.map_or(
                if ask.unknown {
                    COOLDOWN_CEILING
                } else {
                    backoff
                },
                |s| Duration::from_secs(s.into()).clamp(COOLDOWN_FLOOR, COOLDOWN_CEILING),
            )
        }
        _ if escalate.is_some_and(|timeout| ask.sent + timeout > stop) => stop + COOLDOWN_CEILING,
        _ => ask.sent + backoff,
    }
}

/// Every leg-1 node's earliest re-sample, from its last sample and, with
/// escalation on, the one before it.
fn earliest_by_node(
    asks: &[SampleAsk],
    stop: Instant,
    escalate: Option<Duration>,
) -> HashMap<SocketAddr, Instant> {
    let mut earliest = HashMap::new();
    let mut last: HashMap<SocketAddr, &SampleAsk> = HashMap::new();
    for a in asks {
        let prev = last.insert(a.to, a);
        earliest.insert(a.to, earliest_resample(a, prev, stop, escalate));
    }
    earliest
}

/// Leg-2 samples sent before their node's earliest allowed re-sample.
fn sampled_early<'a>(
    earliest: &HashMap<SocketAddr, Instant>,
    asks: &'a [SampleAsk],
) -> Vec<&'a SampleAsk> {
    asks.iter()
        .filter(|a| earliest.get(&a.to).is_some_and(|due| a.sent < *due))
        .collect()
}

/// Ties the actor's `sample_failures` to the network's record: every error
/// and unanswered sample is a failure (an error reply, or a timeout),
/// except those still in flight at the stop, which are sealed instead.
/// Those can only be samples sent within a query timeout plus a tick of
/// the stop that got no reply by then. Returns how many were sealed.
fn sealed_at_stop(
    c: &Asks,
    sample_failures: u64,
    asks: &[SampleAsk],
    stop: Instant,
    window: Duration,
) -> u64 {
    let failed = c.errors + c.unanswered;
    assert!(
        sample_failures <= failed,
        "sample_failures {sample_failures} > errors + unanswered {failed}"
    );
    let flying = asks
        .iter()
        .filter(|a| a.sent + window > stop && a.reply.is_none_or(|(at, _)| at > stop))
        .count() as u64;
    let sealed = failed - sample_failures;
    assert!(
        sealed <= flying,
        "{sealed} unanswered samples neither failed nor in flight at the stop ({flying})"
    );
    sealed
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
    let escalate = dht.sample_backoff_escalate.then_some(dht.query_timeout);
    let split = dht.sampler_cooldown_split;
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
    // Load-bearing: `Net::drain` stamps the samples sent while stopping
    // with the time it runs, and the bounds treat that as their send time.
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
    let in_flight = dht.query_timeout + dht.tick;
    let sealed1 = sealed_at_stop(&c1, one.dht.sample_failures, &net.asks, stop, in_flight);
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
        "samples: sent={} answered={} errors={} unanswered={} sample_failures={} sealed={sealed1} sample_stale={}",
        c1.sent,
        c1.answered,
        c1.errors,
        c1.unanswered,
        one.dht.sample_failures,
        one.dht.sample_stale
    );
    // X-split-resume-gate: the cooldowns leg 1 moved out of a full table
    // (a gauge: live ones as of the stop's last tick; since
    // X-split-spill-horizon, ended ones don't count, swept or not).
    let spilled1 = one.dht.sample_spill_slots;
    if split {
        let _ = writeln!(text, "spill_slots_at_stop={spilled1}");
    }
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
    let _ = writeln!(
        text,
        "# (the network's tallies cover both legs; `sample` leaves out the samples sent while stopping)"
    );

    // What leg 2's samples show against what leg 1 left.
    let earliest = earliest_by_node(&net.asks[..leg1], stop, escalate);
    let asks2 = &net.asks[leg1..];
    let stop2 = Instant::now();
    let c2 = count(asks2, stop2);
    let sealed2 = sealed_at_stop(&c2, two.dht.sample_failures, asks2, stop2, in_flight);
    assert_eq!(
        c2.sent, two.dht.samples_sent,
        "leg 2: network saw every sample"
    );
    let mut first2: HashMap<SocketAddr, Instant> = HashMap::new();
    for a in asks2 {
        first2.entry(a.to).or_insert(a.sent);
    }
    let early: Vec<String> = sampled_early(&earliest, asks2)
        .into_iter()
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
        "samples: sent={} answered={} errors={} unanswered={} sample_failures={} sealed={sealed2} sample_stale={}",
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
        // With the split on, a node can be held by the spill filter
        // instead (one live slot each; the gauge counts live slots only,
        // so ended ones can't make up for a node the snapshot lost).
        let held = restored + if split { spilled1 } else { 0 };
        assert!(
            held as usize >= restricted,
            "restored {restored} (+{} spilled) < {restricted} known restricted at the restart",
            held - restored
        );
        assert!(
            first_sample.is_some_and(|d| d <= Duration::from_secs(60)),
            "sampling didn't resume at once: {first_sample:?}"
        );
        assert_eq!(
            expired_resampled, expired_at_restart,
            "nodes free at the restart were sampled again"
        );
        let missed: Vec<String> = due
            .iter()
            .filter(|a| !first2.contains_key(*a))
            .map(|a| format!("{a} due {:?} before the end", end2 - earliest[a]))
            .collect();
        if split {
            // A spilled node left the table: once its cooldown ends it's
            // sampled when a lookup offers it again, as within a run, so a
            // few due near the end may not be offered in time.
            assert!(
                delays.len() * 100 >= due.len() * 99,
                "too few obligations expiring in leg 2 were followed by a sample: {missed:?}"
            );
        } else {
            assert_eq!(
                delays.len(),
                due.len(),
                "every obligation expiring in leg 2 was followed by a sample: {missed:?}"
            );
        }
    } else {
        assert_eq!(d2.sample_suppression_reason, Reason::Off, "{text}");
        if end2 < start2 + suppression {
            assert!(asks2.is_empty(), "sampled inside the full suppression");
        }
    }
    drop(dirs);
    Report {
        asks: net.asks,
        ..run::finish(text, &log, d2, net.stats, stored_at_end, &minutes, pool_end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> SocketAddr {
        SocketAddr::from(([24, 0, 0, n], 6881))
    }

    fn ask(n: u8, sent: Instant, reply: Option<(Instant, Option<u32>)>) -> SampleAsk {
        SampleAsk::new(addr(n), sent, reply)
    }

    const S: fn(u64) -> Duration = Duration::from_secs;

    /// Every case of the bound, as the sampler sets the real deadline.
    #[test]
    fn earliest_resample_follows_the_sampler() {
        let t = Instant::now();
        let stop = t + S(1_000);
        let at = t + S(1);
        // An interval below the floor, in range, and above the ceiling.
        assert_eq!(
            earliest_resample(&ask(1, t, Some((at, Some(10)))), None, stop, None),
            at + S(60)
        );
        assert_eq!(
            earliest_resample(&ask(1, t, Some((at, Some(900)))), None, stop, None),
            at + S(900)
        );
        assert_eq!(
            earliest_resample(&ask(1, t, Some((at, Some(30_000)))), None, stop, None),
            at + S(6 * 3600)
        );
        // An error reply cools for the ceiling from its arrival.
        assert_eq!(
            earliest_resample(&ask(1, t, Some((at, None))), None, stop, None),
            at + S(6 * 3600)
        );
        // No reply: the ceiling from the send.
        assert_eq!(
            earliest_resample(&ask(1, t, None), None, stop, None),
            t + S(6 * 3600)
        );
        // A reply due at the stop counts; one due after it doesn't arrive.
        assert_eq!(
            earliest_resample(&ask(1, t, Some((stop, Some(900)))), None, stop, None),
            stop + S(900)
        );
        assert_eq!(
            earliest_resample(&ask(1, t, Some((stop + S(1), Some(900)))), None, stop, None),
            t + S(6 * 3600)
        );
    }

    /// With escalation on, a failure cools for the first failure's backoff
    /// unless the node's previous sample failed too, within a ceiling; a
    /// 204 and an answer are as with it off.
    #[test]
    fn earliest_resample_follows_the_escalation() {
        let t = Instant::now();
        let stop = t + S(100_000);
        let at = t + S(1);
        let first = |a: &SampleAsk, prev| earliest_resample(a, prev, stop, Some(S(4)));
        let silent = ask(1, t, None);
        let busy = ask(1, t, Some((at, None)));
        let mut unknown = busy;
        unknown.unknown = true;
        let answered = ask(1, t, Some((at, Some(900))));
        // Alone: a first failure in a row.
        assert_eq!(first(&silent, None), t + FIRST_FAILURE);
        assert_eq!(first(&busy, None), at + FIRST_FAILURE);
        // A 204 cools for the ceiling at once; an answer for its interval.
        assert_eq!(first(&unknown, None), at + S(6 * 3600));
        assert_eq!(first(&answered, None), at + S(900));
        let later = |d: u64, reply: Option<(Instant, Option<u32>)>| ask(1, t + S(d), reply);
        // After a failure within a ceiling: escalated.
        for prev in [&silent, &busy] {
            assert_eq!(first(&later(400, None), Some(prev)), t + S(400 + 6 * 3600));
            let reply = Some((t + S(401), None));
            assert_eq!(first(&later(400, reply), Some(prev)), t + S(401 + 6 * 3600));
        }
        // After an answer or a 204, or a ceiling or more after a failure:
        // a first failure again.
        for prev in [&answered, &unknown] {
            assert_eq!(
                first(&later(400, None), Some(prev)),
                t + S(400) + FIRST_FAILURE
            );
        }
        assert_eq!(
            first(&later(6 * 3600, None), Some(&silent)),
            t + S(6 * 3600) + FIRST_FAILURE
        );
        // Unanswered and sent within the query timeout of the stop: sealed.
        assert_eq!(
            first(&ask(1, stop - S(3), None), Some(&silent)),
            stop + S(6 * 3600)
        );
        assert_eq!(
            first(&ask(1, stop - S(4), None), None),
            stop - S(4) + FIRST_FAILURE
        );
        // A reply after the stop didn't arrive: the earlier sample failed.
        let lost = ask(1, t, Some((stop + S(1), Some(900))));
        assert_eq!(first(&later(400, None), Some(&lost)), t + S(400 + 6 * 3600));
        // Off, the previous sample doesn't matter.
        assert_eq!(
            earliest_resample(&later(400, None), Some(&silent), stop, None),
            t + S(400 + 6 * 3600)
        );
        // By node: each node's last sample, after the one before it.
        let asks = [silent, ask(2, t, None), later(400, None)];
        let by = earliest_by_node(&asks, stop, Some(S(4)));
        assert_eq!(by[&addr(1)], t + S(400 + 6 * 3600));
        assert_eq!(by[&addr(2)], t + FIRST_FAILURE);
    }

    /// The check catches a sample before the bound and passes one at it,
    /// and ignores nodes leg 1 never sampled.
    #[test]
    fn a_sample_before_the_bound_is_caught() {
        let t = Instant::now();
        let stop = t + S(100);
        let leg1 = [ask(1, t, Some((t + S(1), Some(900)))), ask(2, t, None)];
        let earliest: HashMap<SocketAddr, Instant> = leg1
            .iter()
            .map(|a| (a.to, earliest_resample(a, None, stop, None)))
            .collect();
        let leg2 = [
            ask(1, t + S(900), None),
            ask(1, t + S(901), None),
            ask(2, t + S(3600), None),
            ask(3, t + S(200), None),
        ];
        let early: Vec<(SocketAddr, Instant)> = sampled_early(&earliest, &leg2)
            .iter()
            .map(|a| (a.to, a.sent))
            .collect();
        assert_eq!(early, [(addr(1), t + S(900)), (addr(2), t + S(3600))]);
    }

    /// Unanswered samples are failures, except those in flight at the stop.
    #[test]
    fn failures_balance_against_the_record() {
        let t = Instant::now();
        let stop = t + S(100);
        let w = S(5);
        let asks = [
            ask(1, t, Some((t + S(1), Some(900)))),
            ask(2, t, Some((t + S(1), None))),
            ask(3, t, None),
            ask(4, stop - S(1), None),
            ask(5, stop - S(1), Some((stop + S(1), Some(900)))),
        ];
        let c = count(&asks, stop);
        assert_eq!(
            c,
            Asks {
                sent: 5,
                answered: 1,
                errors: 1,
                unanswered: 3
            }
        );
        assert_eq!(sealed_at_stop(&c, 2, &asks, stop, w), 2);
        assert_eq!(sealed_at_stop(&c, 4, &asks, stop, w), 0);
    }

    #[test]
    #[should_panic(expected = "neither failed nor in flight")]
    fn a_missing_failure_is_caught() {
        let t = Instant::now();
        let stop = t + S(100);
        let asks = [ask(1, t, None), ask(2, t, None)];
        let c = count(&asks, stop);
        sealed_at_stop(&c, 1, &asks, stop, S(5));
    }
}
