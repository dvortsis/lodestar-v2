//! X-escalate-full-table: with `dht.sampler_cooldown_split` off and the
//! sampler's table full, `dht.sample_backoff_escalate` still gives a struck
//! node its second sample about `FIRST_FAILURE_BACKOFF` after its first
//! failed one. With escalation off, a failed node waits the full backoff,
//! as before.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use lodestar_dht::node::testing::{
    SAMPLE_FIRST_FAILURE_BACKOFF as FIRST, SAMPLE_MAX_INTERVAL as FULL, SamplerCapacity,
};
use tokio::time::Instant;

use crate::net::SampleAsk;
use crate::{scenario, simulate};

/// The sampler's table: the base network's 2,000 nodes fill it within the
/// first minute, and it stays full (`sample_full_seconds`).
const CAPACITY: usize = 200;
/// How late a second sample may come after its first backoff ends: the
/// node waits for the next maintenance prune and for a reply that offers
/// it again.
const SLACK: Duration = Duration::from_secs(120);

fn full_table(escalate: bool) -> crate::run::Report {
    let mut s = scenario::base("escalate-full-table");
    s.minutes = 20;
    s.switches.sample_backoff_escalate = escalate;
    let _capacity = SamplerCapacity::set(CAPACITY);
    simulate(&s)
}

/// Each node whose first sample failed (not a 204, which cools in full at
/// once), with when it failed and when, if ever, it was sampled next.
/// Only nodes whose first backoff and its slack ended within the run.
fn struck(asks: &[SampleAsk]) -> Vec<(SocketAddr, Instant, Option<Instant>)> {
    let timeout = lodestar_dht::node::Config::default().query_timeout;
    let end = asks.iter().map(|a| a.sent).max().expect("samples sent");
    let mut by: HashMap<SocketAddr, Vec<&SampleAsk>> = HashMap::new();
    for a in asks {
        by.entry(a.to).or_default().push(a);
    }
    let mut out = vec![];
    for (to, asks) in by {
        let a = asks[0];
        if matches!(a.reply, Some((_, Some(_)))) || a.unknown {
            continue;
        }
        let failed = a.reply.map_or(a.sent + timeout, |(at, _)| at);
        if failed + FIRST + SLACK <= end {
            out.push((to, failed, asks.get(1).map(|b| b.sent)));
        }
    }
    out
}

/// Split off, a full table, escalation on: every struck node gets its
/// second sample within `SLACK` of its first backoff's end, never sooner.
/// The same run with escalation off samples none of them again (the full
/// backoff outlasts the run), and is the run `main` gave before this slice
/// (measured at 32396391): the held queue never fills with the switch off.
#[test]
fn a_struck_node_gets_its_second_sample_on_a_full_table() {
    let on = full_table(true);
    assert!(
        on.dht.sample_full_seconds >= 600,
        "the table is full most of the run: {} s",
        on.dht.sample_full_seconds
    );
    let nodes = struck(&on.asks);
    assert!(nodes.len() >= 30, "{} struck nodes", nodes.len());
    let mut slowest = Duration::ZERO;
    for (to, failed, next) in &nodes {
        let next = next.unwrap_or_else(|| panic!("{to} never sampled again"));
        slowest = slowest.max(next - *failed);
        assert!(next >= *failed + FIRST, "{to} sampled before its backoff");
        assert!(
            next <= *failed + FIRST + SLACK,
            "{to} sampled {:?} after its failure",
            next - *failed
        );
    }
    eprintln!(
        "escalation on: {} struck nodes, the slowest resampled {} s after its failure",
        nodes.len(),
        slowest.as_secs()
    );

    let off = full_table(false);
    // Before this slice, `main` gave exactly these.
    assert_eq!(
        (off.dht.samples_sent, off.dht.sample_full_seconds),
        (380, 1_045),
        "{}",
        off.text
    );
    let nodes = struck(&off.asks);
    assert!(nodes.len() >= 30, "{} struck nodes", nodes.len());
    assert!(FULL > Duration::from_secs(20 * 60));
    for (to, _, next) in &nodes {
        assert_eq!(*next, None, "{to} sampled again within the full backoff");
    }
}
