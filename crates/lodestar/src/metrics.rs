//! Prometheus text exposition of the pipeline's counters (architecture
//! §12: metrics for every pipeline stage).

use std::fmt::Write;
use std::sync::atomic::Ordering;

use lodestar_dht::node::StatsSnapshot;
use lodestar_peer_wire::{FailureKind, FetchStats};
use lodestar_pipeline::Monitor;
use lodestar_pipeline::gate::{EXCLUDED, GateStats, LATENCY_BUCKETS, ORIGINS};
use lodestar_pipeline::health::{HealthHandle, trackers::Health};
use lodestar_sources::Loss;
use lodestar_store::HotColumns;

pub const FAILURE_KINDS: [(FailureKind, &str); 7] = [
    (FailureKind::Unreachable, "unreachable"),
    (FailureKind::TimedOut, "timed_out"),
    (FailureKind::Refused, "refused"),
    (FailureKind::EncryptionFailed, "encryption_failed"),
    (FailureKind::Invalid, "invalid"),
    (FailureKind::Oversize, "oversize"),
    (FailureKind::HashMismatch, "hash_mismatch"),
];

pub const LOSSES: [(Loss, &str); 10] = [
    (Loss::NoPeers, "no_peers"),
    (Loss::TimedOut, "timed_out"),
    (Loss::Refused, "refused"),
    (Loss::EncryptionFailed, "encryption_failed"),
    (Loss::Oversize, "oversize"),
    (Loss::Invalid, "invalid"),
    (Loss::Blocked, "blocked"),
    (Loss::Evicted, "evicted"),
    (Loss::GaveUp, "gave_up"),
    (Loss::Unreachable, "unreachable"),
];

struct Out(String);

impl Out {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.0, "# HELP lodestar_{name} {help}");
        let _ = writeln!(self.0, "# TYPE lodestar_{name} {kind}");
    }

    fn value(&mut self, name: &str, labels: &str, v: impl std::fmt::Display) {
        if labels.is_empty() {
            let _ = writeln!(self.0, "lodestar_{name} {v}");
        } else {
            let _ = writeln!(self.0, "lodestar_{name}{{{labels}}} {v}");
        }
    }

    fn one(&mut self, name: &str, kind: &str, help: &str, v: impl std::fmt::Display) {
        self.family(name, kind, help);
        self.value(name, "", v);
    }
}

pub fn render(m: &Monitor, dht: Option<&StatsSnapshot>, fetch: &FetchStats) -> String {
    let mut o = Out(String::with_capacity(8192));
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);

    o.one(
        "start_time_seconds",
        "gauge",
        "Unix time the pipeline started.",
        m.started_at(),
    );
    o.one("torrents", "gauge", "Torrents stored.", m.torrents());
    o.one(
        "floor_removals_kept_total",
        "counter",
        "Locked-floor entries a config file or a floor-marked list dropped, kept until \
         removed in the app (counted at each start and list refresh).",
        crate::floor::KEPT.load(Ordering::Relaxed),
    );

    o.family(
        "source_up",
        "gauge",
        "Whether each source plugin is running.",
    );
    for s in m.sources() {
        o.value(
            "source_up",
            &format!("source=\"{}\"", s.plugin),
            u8::from(s.running),
        );
    }
    o.family(
        "source_dropped_total",
        "counter",
        "Keys a source dropped because the pipeline was full.",
    );
    for s in m.sources() {
        o.value(
            "source_dropped_total",
            &format!("source=\"{}\"", s.plugin),
            s.dropped,
        );
    }

    let i = m.intake_stats();
    o.one(
        "intake_received_total",
        "counter",
        "Keys reported by sources.",
        get(&i.received),
    );
    o.one(
        "intake_known_total",
        "counter",
        "Keys the have-filter says are stored.",
        get(&i.known),
    );
    o.one(
        "intake_new_total",
        "counter",
        "Keys sent to the fetch pool.",
        get(&i.to_pool),
    );
    o.one(
        "intake_dropped_total",
        "counter",
        "Rediscoveries dropped while the writer was behind.",
        get(&i.dropped),
    );
    o.one(
        "intake_refused_total",
        "counter",
        "Keys refused before fetching: CS-1 hashes, tombstones and quarantine.",
        get(&i.refused),
    );

    let p = m.pool_stats();
    o.one(
        "fetch_pending",
        "gauge",
        "Keys queued or being fetched.",
        get(&p.pending),
    );
    o.one(
        "fetch_active",
        "gauge",
        "Metadata fetches in progress.",
        get(&p.active),
    );
    o.one(
        "fetch_lookups",
        "gauge",
        "Peer lookups in flight.",
        get(&p.lookups),
    );
    o.one(
        "fetch_lookup_backlog",
        "gauge",
        "Keys waiting for a lookup slot.",
        get(&p.lookup_backlog),
    );
    for (name, value) in [
        ("fetch_finder_full_total", &p.finder_full),
        ("fetch_finder_closed_total", &p.finder_closed),
        ("fetch_lookup_unaccepted_total", &p.lookup_unaccepted),
        ("fetch_retry_unaccepted_total", &p.retry_unaccepted),
    ] {
        o.one(
            name,
            "counter",
            "Pool admission diagnostics; not network attempts.",
            get(value),
        );
    }
    o.family(
        "fetch_evicted_service_total",
        "counter",
        "Service reached before eviction; transient process observations.",
    );
    for (i, stage) in ["unaccepted", "lookup", "peers", "fetch"]
        .iter()
        .enumerate()
    {
        o.value(
            "fetch_evicted_service_total",
            &format!("stage=\"{stage}\""),
            get(&p.evicted_service[i]),
        );
    }
    render_admission(&mut o, p);
    // Attempts and successes per transport; the success family replaces
    // the pool's unlabelled count (see `write_prometheus` for how the two
    // can differ). The uTP socket's own
    // counters join once the DHT shares its port with uTP.
    fetch.write_prometheus(None, &mut o.0);
    o.one(
        "fetch_duplicates_total",
        "counter",
        "Keys already queued or just fetched.",
        get(&p.duplicates),
    );
    o.family(
        "fetch_failures_total",
        "counter",
        "Failed fetch attempts by kind.",
    );
    for (kind, label) in FAILURE_KINDS {
        o.value(
            "fetch_failures_total",
            &format!("kind=\"{label}\""),
            p.failure_count(kind),
        );
    }
    o.family(
        "fetch_lost_total",
        "counter",
        "Keys that left the pool without metadata, by reason.",
    );
    for (loss, label) in LOSSES {
        o.value(
            "fetch_lost_total",
            &format!("reason=\"{label}\""),
            p.lost_count(loss),
        );
    }

    let w = m.writer_stats();
    o.one(
        "writer_stored_total",
        "counter",
        "Torrents newly stored.",
        get(&w.stored),
    );
    o.one(
        "writer_existing_total",
        "counter",
        "Fetched torrents that were already stored.",
        get(&w.existing),
    );
    o.one(
        "writer_invalid_total",
        "counter",
        "Verified dictionaries that aren't usable metainfo.",
        get(&w.invalid),
    );
    o.one(
        "writer_blocked_total",
        "counter",
        "Torrents dropped by a content filter rule.",
        get(&w.blocked),
    );
    o.one(
        "writer_quarantined_total",
        "counter",
        "Torrents held in quarantine by a content filter rule.",
        get(&w.quarantined),
    );
    o.one(
        "writer_rediscovered_total",
        "counter",
        "Stored torrents seen again.",
        get(&w.rediscovered),
    );
    o.one(
        "writer_filter_false_positives_total",
        "counter",
        "Have-filter answers the store contradicted.",
        get(&w.false_positives),
    );
    o.one(
        "writer_transactions_total",
        "counter",
        "Store transactions committed.",
        get(&w.transactions),
    );
    o.one(
        "writer_errors_total",
        "counter",
        "Failed store writes.",
        get(&w.errors),
    );
    o.one(
        "writer_enriched_total",
        "counter",
        "Stored torrents enriched in the background (§6).",
        get(&w.enriched),
    );
    o.one(
        "writer_last_commit_seconds",
        "gauge",
        "Unix time of the last commit.",
        get(&w.last_commit),
    );

    render_gate(&mut o, m.gate());

    let l = m.lease_stats();
    o.one(
        "leases_granted_total",
        "counter",
        "New keys leased to a worker (each counted as harvested once).",
        get(&l.granted),
    );
    o.one(
        "leases_held_total",
        "counter",
        "Claims for keys already leased; their source joined the lease.",
        get(&l.held),
    );
    o.one(
        "leases_stale_total",
        "counter",
        "Reports and results under a token that was no longer current.",
        get(&l.stale),
    );
    o.one(
        "leases_expired_total",
        "counter",
        "Leases taken back from silent workers.",
        get(&l.expired),
    );
    o.one(
        "leases_outstanding",
        "gauge",
        "Keys leased and in flight.",
        get(&l.outstanding),
    );

    let h = m.have();
    o.one(
        "have_entries",
        "gauge",
        "Hashes in the have-filter.",
        h.len(),
    );
    o.one(
        "have_capacity",
        "gauge",
        "Have-filter capacity before a rebuild.",
        h.capacity(),
    );
    o.one(
        "have_bytes",
        "gauge",
        "Have-filter memory.",
        h.memory_bytes(),
    );

    if let Some(d) = dht {
        o.one(
            "dht_routing_nodes",
            "gauge",
            "Nodes in the routing tables.",
            d.routing_nodes,
        );
        o.one(
            "dht_outstanding",
            "gauge",
            "Queries awaiting a reply.",
            d.outstanding,
        );
        o.one(
            "dht_reply_ratio",
            "gauge",
            "Responses per query sent (collapses under NAT saturation).",
            d.reply_ratio(),
        );
        o.one(
            "dht_packets_in_total",
            "counter",
            "UDP packets received.",
            d.packets_in,
        );
        o.one(
            "dht_packets_out_total",
            "counter",
            "UDP packets sent.",
            d.packets_out,
        );
        o.one(
            "dht_malformed_total",
            "counter",
            "Undecodable packets received.",
            d.malformed_in,
        );
        o.one(
            "dht_denied_total",
            "counter",
            "Packets to or from addresses on a subscribed IP blocklist.",
            d.denied,
        );
        o.one(
            "dht_queries_in_total",
            "counter",
            "Queries received.",
            d.queries_in,
        );
        o.one(
            "dht_queries_out_total",
            "counter",
            "Queries sent.",
            d.queries_out,
        );
        o.one(
            "dht_responses_in_total",
            "counter",
            "Responses received.",
            d.responses_in,
        );
        o.one(
            "dht_timeouts_total",
            "counter",
            "Queries that timed out.",
            d.timeouts,
        );
        o.one(
            "dht_throttled_total",
            "counter",
            "Queries held back by the rate limit.",
            d.throttled,
        );
        o.one(
            "dht_peer_admitted_total",
            "counter",
            "Owned peer requests admitted.",
            d.peer_admitted,
        );
        o.one(
            "dht_peer_rejected_total",
            "counter",
            "Peer requests rejected without ownership.",
            d.peer_rejected,
        );
        o.family(
            "dht_peer_rejection_reasons_total",
            "counter",
            "Exclusive actor rejection reasons; caller closure precedes pause and capacity.",
        );
        for (reason, value) in [
            ("capacity", d.peer_rejected_capacity),
            ("paused", d.peer_rejected_paused),
            ("caller_closed", d.peer_rejected_caller_closed),
        ] {
            o.value(
                "dht_peer_rejection_reasons_total",
                &format!("reason=\"{reason}\""),
                value,
            );
        }
        for (name, value) in [
            ("dht_peer_ack_failed_total", d.peer_ack_failed),
            ("dht_peer_command_full_total", d.peer_command_full),
            ("dht_peer_command_closed_total", d.peer_command_closed),
        ] {
            o.one(
                name,
                "counter",
                "Pre-ownership delivery failure; not an accepted attempt.",
                value,
            );
        }
        render_permit_returns(&mut o, d);
        o.one(
            "dht_peer_cancelled_total",
            "counter",
            "Active peer requests cancelled by the caller.",
            d.peer_cancelled,
        );
        o.family(
            "dht_peer_queued_dropped_total",
            "counter",
            "Admitted peer requests dropped while queued, never started.",
        );
        for (i, why) in lodestar_dht::node::QueuedDrop::ALL.iter().enumerate() {
            o.value(
                "dht_peer_queued_dropped_total",
                &format!("reason=\"{}\"", why.as_str()),
                d.peer_queued_dropped[i],
            );
        }
        o.one(
            "dht_peer_active",
            "gauge",
            "Active logical peer requests across families.",
            d.peer_active,
        );
        o.one(
            "dht_peer_queued",
            "gauge",
            "Accepted peer requests awaiting dispatch.",
            d.peer_queued,
        );
        o.one(
            "dht_lookup_scheduled",
            "gauge",
            "Lookups scheduled for continuation.",
            d.lookup_scheduled,
        );
        o.one(
            "dht_lookup_deadlines_total",
            "counter",
            "Lookup legs terminated by deadline.",
            d.lookup_deadlines,
        );
        o.one(
            "dht_sample_frontier",
            "gauge",
            "Eligible sample frontier candidates.",
            d.sample_frontier,
        );
        o.one(
            "dht_sample_suppression_seconds",
            "gauge",
            "Seconds of startup sampling suppression remaining.",
            d.sample_suppression_seconds,
        );
        render_restart(&mut o, d);
        o.one(
            "dht_sample_cooling",
            "gauge",
            "Sampler cooldown obligations; also the indexed expiry count.",
            d.sample_cooling,
        );
        o.one(
            "dht_sample_inflight",
            "gauge",
            "Reserved sample transactions.",
            d.sample_inflight,
        );
        o.one(
            "dht_sample_stale_total",
            "counter",
            "Stale sampler completions ignored.",
            d.sample_stale,
        );
        o.one(
            "dht_sample_deferred_total",
            "counter",
            "Sample work deferred by state capacity.",
            d.sample_deferred,
        );
        o.one(
            "dht_lookups_completed_total",
            "counter",
            "get_peers lookups finished.",
            d.lookups_completed,
        );
        o.one(
            "dht_scrapes_requested_total",
            "counter",
            "BEP 33 scrapes asked of the DHT node.",
            d.scrapes_requested,
        );
        o.one(
            "dht_scrapes_completed_total",
            "counter",
            "BEP 33 scrape lookups finished.",
            d.scrapes_completed,
        );
        o.one(
            "dht_scrapes_with_responders_total",
            "counter",
            "BEP 33 scrapes where at least one node sent filters.",
            d.scrapes_with_responders,
        );
        o.one(
            "dht_scrapes_dropped_total",
            "counter",
            "BEP 33 scrapes refused because the queue was full.",
            d.scrapes_dropped,
        );
        o.one(
            "dht_harvest_dropped_total",
            "counter",
            "Harvests dropped because the pipeline was full.",
            d.harvest_dropped,
        );
        o.family(
            "dht_harvested_total",
            "counter",
            "Info hashes harvested by strategy.",
        );
        o.value(
            "dht_harvested_total",
            "strategy=\"sample\"",
            d.harvested_sample,
        );
        o.value(
            "dht_harvested_total",
            "strategy=\"get_peers\"",
            d.harvested_get_peers,
        );
        o.value(
            "dht_harvested_total",
            "strategy=\"announce\"",
            d.harvested_announce,
        );
    }
    o.0
}

/// The crawler gate (owner-decisions §2.25): new verified records and the
/// same records searchable, by origin; every excluded outcome by reason;
/// and the time from first sighting to searchable.
fn render_gate(o: &mut Out, g: &GateStats) {
    o.family(
        "metadata_new_verified_total",
        "counter",
        "Unique new infohashes fetched and verified in this run and committed as stored.",
    );
    for (origin, label) in ORIGINS {
        o.value(
            "metadata_new_verified_total",
            &format!("origin=\"{label}\""),
            g.new_verified(origin),
        );
    }
    o.family(
        "searchable_new_total",
        "counter",
        "Of those, records the search index has committed.",
    );
    for (origin, label) in ORIGINS {
        o.value(
            "searchable_new_total",
            &format!("origin=\"{label}\""),
            g.searchable_new(origin),
        );
    }
    o.family(
        "metadata_excluded_total",
        "counter",
        "Outcomes that don't count towards the gate, by reason.",
    );
    for (reason, label) in EXCLUDED {
        o.value(
            "metadata_excluded_total",
            &format!("reason=\"{label}\""),
            g.excluded(reason),
        );
    }
    let name = "discovery_to_searchable_seconds";
    o.family(
        name,
        "histogram",
        "Seconds from a new record's first sighting in this run to searchable.",
    );
    let counts = g.latency.cumulative();
    for (le, n) in LATENCY_BUCKETS.iter().zip(counts) {
        o.value(&format!("{name}_bucket"), &format!("le=\"{le}\""), n);
    }
    o.value(
        &format!("{name}_bucket"),
        "le=\"+Inf\"",
        counts[LATENCY_BUCKETS.len()],
    );
    o.value(&format!("{name}_sum"), "", g.latency.sum());
    // The `+Inf` bucket, so the two always agree within a scrape.
    o.value(&format!("{name}_count"), "", counts[LATENCY_BUCKETS.len()]);
    o.one(
        "discovery_to_searchable_dropped_total",
        "counter",
        "Latency measurements dropped because their bounded queue was full.",
        g.dropped.load(Ordering::Relaxed),
    );
}

/// The search index's counters (§8).
pub fn render_search(out: &mut String, docs: u64, stats: &lodestar_search::IndexStats) {
    let mut o = Out(std::mem::take(out));
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    o.one(
        "search_docs",
        "gauge",
        "Torrents in the search index.",
        docs,
    );
    o.one(
        "search_watermark",
        "gauge",
        "Highest torrent id searchable.",
        stats.watermark.load(Ordering::Relaxed),
    );
    o.one(
        "search_indexed_total",
        "counter",
        "Torrents indexed or re-indexed since start, counted at commit.",
        get(&stats.indexed),
    );
    o.one(
        "search_commits_total",
        "counter",
        "Index commits.",
        get(&stats.commits),
    );
    o.one(
        "search_errors_total",
        "counter",
        "Indexer errors (store reads and commits); each is retried.",
        get(&stats.errors),
    );
    o.one(
        "search_last_commit_seconds",
        "gauge",
        "Unix time of the last index commit.",
        get(&stats.last_commit),
    );
    *out = o.0;
}

/// Scrape outcome labels, by [`lodestar_pipeline::health::Outcome`] code.
pub const SCRAPE_OUTCOMES: [&str; 6] = ["ok", "timeout", "banned", "error", "skipped", "late"];

pub fn health_name(h: Health) -> &'static str {
    match h {
        Health::Ok => "ok",
        Health::Backoff => "backoff",
        Health::Down => "down",
        Health::Banned => "banned",
    }
}

/// The scrape scheduler's counters and the hot columns (§15, §7).
/// Trackers are labelled by id: an HTTP tracker URL can carry a passkey,
/// and `/metrics` is open.
pub fn render_scrape(out: &mut String, health: &HealthHandle, swarm_updates: u64) {
    let mut o = Out(std::mem::take(out));
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    let st = health.stats();
    o.one(
        "scrape_rounds_total",
        "counter",
        "Scrape rounds started.",
        get(&st.rounds),
    );
    o.family(
        "scrape_torrents_total",
        "counter",
        "Torrents scraped, by what the round found.",
    );
    for (label, v) in [
        ("alive", &st.alive),
        ("dead", &st.dead),
        ("unreached", &st.unreached),
    ] {
        o.value(
            "scrape_torrents_total",
            &format!("result=\"{label}\""),
            get(v),
        );
    }
    o.one(
        "scrape_suspect_total",
        "counter",
        "Scrapes the swarm-consistency check flagged as fake seeding.",
        get(&st.suspect),
    );
    o.one(
        "scrape_private_skipped_total",
        "counter",
        "Private torrents taken from a lane and never scraped.",
        get(&st.private_skipped),
    );
    o.one(
        "scrape_fresh_skipped_total",
        "counter",
        "Torrents dropped from a lane as already fresh: hints for recent scrapes, and new or due torrents scraped since they were queued.",
        get(&st.fresh_skipped),
    );
    o.one(
        "scrape_dht_busy_total",
        "counter",
        "DHT scrapes not asked because the node was busy with others.",
        get(&st.dht_busy),
    );
    o.one(
        "scrape_hints_dropped_total",
        "counter",
        "Interest and rediscovery hints dropped because a lane was full.",
        get(&st.hints_dropped),
    );
    o.one(
        "scrape_dht_requested_total",
        "counter",
        "DHT (BEP 33) scrapes the scheduler asked for.",
        get(&st.dht_requested),
    );
    o.one(
        "scrape_dht_estimates_total",
        "counter",
        "DHT scrapes that returned an estimate.",
        get(&st.dht_estimates),
    );
    o.one(
        "scrape_stored_total",
        "counter",
        "Scrape results committed to the store.",
        swarm_updates,
    );
    let trackers = health.trackers();
    o.family(
        "tracker_rounds_total",
        "counter",
        "Scrape rounds per tracker, by outcome.",
    );
    for t in &trackers {
        for (code, label) in SCRAPE_OUTCOMES.iter().enumerate() {
            o.value(
                "tracker_rounds_total",
                &format!("tracker=\"{}\",outcome=\"{label}\"", t.id),
                t.rounds[code],
            );
        }
    }
    o.family(
        "tracker_health",
        "gauge",
        "Tracker health: 0 ok, 1 backoff, 2 down, 3 banned.",
    );
    for t in &trackers {
        o.value(
            "tracker_health",
            &format!("tracker=\"{}\"", t.id),
            t.health as u8,
        );
    }
    *out = o.0;
}

/// The hot columns' size: they're always held (last seen, search), with
/// scraping on or off.
pub fn render_hot(out: &mut String, hot: &HotColumns) {
    let mut o = Out(std::mem::take(out));
    o.one(
        "hot_columns_ids",
        "gauge",
        "One past the highest torrent id in the hot columns.",
        hot.len(),
    );
    o.one(
        "hot_columns_bytes",
        "gauge",
        "Memory held by the hot columns.",
        hot.memory_bytes(),
    );
    *out = o.0;
}

/// Coordinated admission (CR-ADM): all zero with the switch off.
fn render_admission(o: &mut Out, p: &lodestar_sources::PoolStats) {
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    let labelled =
        |o: &mut Out, name: &str, help: &str, rows: &[(&str, &std::sync::atomic::AtomicU64)]| {
            o.family(name, "counter", help);
            for (labels, value) in rows {
                o.value(name, labels, get(value));
            }
        };
    let kinds = ["arrival", "retry", "requeue"];
    labelled(
        o,
        "fetch_admission_waits_total",
        "Keys due a lookup that queued for a permit.",
        &[
            ("kind=\"arrival\"", &p.admission_waits[0]),
            ("kind=\"retry\"", &p.admission_waits[1]),
            ("kind=\"requeue\"", &p.admission_waits[2]),
        ],
    );
    labelled(
        o,
        "fetch_admission_wait_cancelled_total",
        "Waiters that left without a request; no network attempt.",
        &[
            ("reason=\"evicted\"", &p.admission_wait_cancelled[0]),
            ("reason=\"peers\"", &p.admission_wait_cancelled[1]),
        ],
    );
    o.one(
        "fetch_admission_wakeups_total",
        "counter",
        "Times a returned permit woke the pool.",
        get(&p.admission_wakeups),
    );
    o.family(
        "fetch_admission_grants_total",
        "counter",
        "Requests granted finder permits.",
    );
    for (k, kind) in kinds.iter().enumerate() {
        for (c, class) in ["interactive", "background"].iter().enumerate() {
            o.value(
                "fetch_admission_grants_total",
                &format!("kind=\"{kind}\",class=\"{class}\""),
                get(&p.admission_grants[k][c]),
            );
        }
    }
    labelled(
        o,
        "fetch_admission_refusals_total",
        "Permitted requests not accepted; no attempt or back-off used.",
        &[
            ("reason=\"refused\"", &p.admission_refusals[0]),
            ("reason=\"unacknowledged\"", &p.admission_refusals[1]),
        ],
    );
    o.one(
        "fetch_admission_holds_total",
        "counter",
        "One-second finder holds after a refusal.",
        get(&p.admission_holds),
    );
    labelled(
        o,
        "fetch_admission_permit_returns_total",
        "Permits returned in the pool.",
        &[
            ("boundary=\"finder_full\"", &p.admission_permit_returns[0]),
            ("boundary=\"finder_closed\"", &p.admission_permit_returns[1]),
            ("boundary=\"unused\"", &p.admission_permit_returns[2]),
        ],
    );
    o.one(
        "fetch_admission_waiters",
        "gauge",
        "Keys waiting for a lookup.",
        get(&p.admission_waiters),
    );
    o.one(
        "fetch_admission_permits_in_use",
        "gauge",
        "Finder permits held by requests.",
        get(&p.admission_permits_in_use),
    );
}

/// Admission permits the DHT actor returned, by boundary (CR-ADM).
fn render_permit_returns(o: &mut Out, d: &StatsSnapshot) {
    o.family(
        "dht_peer_permit_returns_total",
        "counter",
        "Admission permits returned in the DHT actor, by boundary.",
    );
    for (i, why) in lodestar_dht::node::PermitReturn::ALL.iter().enumerate() {
        o.value(
            "dht_peer_permit_returns_total",
            &format!("boundary=\"{}\"", why.as_str()),
            d.peer_permit_returns[i],
        );
    }
}

/// CR-0d's restart-proof families (docs/metrics.md).
fn render_restart(o: &mut Out, d: &StatsSnapshot) {
    // CR-0d: why sampling is or isn't running, one label set to 1.
    o.family(
        "dht_sample_suppression_reason",
        "gauge",
        "Why this run is or isn't sampling yet (1 for the current reason).",
    );
    for r in lodestar_dht::restart::Reason::ALL {
        o.value(
            "dht_sample_suppression_reason",
            &format!("reason=\"{}\"", r.as_str()),
            u64::from(r == d.sample_suppression_reason),
        );
    }
    o.one(
        "dht_restart_obligations_restored",
        "gauge",
        "Restrictions restored from the last clean stop.",
        d.restart_obligations_restored,
    );
    o.one(
        "dht_restart_obligations_pending",
        "gauge",
        "Restored restrictions still cooling.",
        d.restart_obligations_pending,
    );
    o.one(
        "dht_restart_obligations_refused_total",
        "counter",
        "Sample candidates a restored restriction turned away.",
        d.restart_obligations_refused,
    );
    o.one(
        "dht_restart_obligations_honoured_total",
        "counter",
        "Restored restrictions that expired unsampled.",
        d.restart_obligations_honoured,
    );
    o.family(
        "dht_restart_persist_errors_total",
        "counter",
        "Restart-proof write failures, by step.",
    );
    for (step, v) in lodestar_dht::node::PersistStep::ALL
        .iter()
        .zip(d.restart_persist_errors)
    {
        o.value(
            "dht_restart_persist_errors_total",
            &format!("step=\"{}\"", step.as_str()),
            v,
        );
    }
}

/// The storage sampler's last values (R-gauges); the scrape reads them
/// and never the filesystem.
pub fn render_storage(out: &mut String, g: &crate::storage::Gauges) {
    let mut o = Out(std::mem::take(out));
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    o.one(
        "store_db_bytes",
        "gauge",
        "Size of the main SQLite file, sampled.",
        get(&g.db_bytes),
    );
    o.one(
        "store_wal_bytes",
        "gauge",
        "Size of the SQLite write-ahead log, sampled; 0 when absent.",
        get(&g.wal_bytes),
    );
    o.one(
        "search_index_bytes",
        "gauge",
        "Size of the search index directory, any rebuild included, sampled.",
        get(&g.index_bytes),
    );
    o.one(
        "storage_sampled_seconds",
        "gauge",
        "Unix time of the last storage sample that read every size; 0 before one.",
        get(&g.sampled_at),
    );
    o.one(
        "storage_sample_errors_total",
        "counter",
        "Storage sizes that couldn't be read; each keeps its last value.",
        get(&g.errors),
    );
    *out = o.0;
}

/// The search side's in-process caches (R-gauges): the serving index's
/// `file:(…)` id sets and browse tops, and the result cache.
pub fn render_caches(
    out: &mut String,
    index: &lodestar_search::CacheUsage,
    results: &lodestar_search::CacheStats,
) {
    let mut o = Out(std::mem::take(out));
    o.one(
        "search_file_cache_entries",
        "gauge",
        "File-clause id sets cached by the serving index.",
        index.file_set_entries,
    );
    o.one(
        "search_file_cache_bytes",
        "gauge",
        "Bytes the file-clause id sets retain (8 MiB budget).",
        index.file_set_bytes,
    );
    o.one(
        "search_browse_cache_entries",
        "gauge",
        "Per-segment sorted tops cached by the serving index.",
        index.browse_entries,
    );
    o.one(
        "search_browse_cache_bytes",
        "gauge",
        "Estimated bytes of the per-segment sorted tops.",
        index.browse_bytes,
    );
    let kinds = [("search", &results.search), ("facets", &results.facets)];
    o.family(
        "search_result_cache_entries",
        "gauge",
        "Result cache entries, by kind.",
    );
    for (kind, k) in kinds {
        o.value(
            "search_result_cache_entries",
            &format!("kind=\"{kind}\""),
            k.entries,
        );
    }
    o.family(
        "search_result_cache_bytes",
        "gauge",
        "Result cache estimated bytes, by kind.",
    );
    for (kind, k) in kinds {
        o.value(
            "search_result_cache_bytes",
            &format!("kind=\"{kind}\""),
            k.bytes,
        );
    }
    *out = o.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_pipeline::gate::{Excluded, Origin};

    #[test]
    fn gate_families_render_each_field_on_its_own_line() {
        // Distinct values everywhere, so a swapped field shows.
        let g = GateStats::new(100);
        g.follow();
        for id in 1..=3 {
            g.committed_new(Origin::Network, id, 100, 100);
        }
        g.searchable(2, 103);
        // Two more observations, so `_count` differs from the searchable
        // count and from every bucket below `+Inf`.
        g.latency.observe(9_000);
        g.latency.observe(9_001);
        for (i, (reason, _)) in EXCLUDED.iter().enumerate() {
            g.exclude(*reason, 10 + i as u64);
        }
        g.dropped.store(41, Ordering::Relaxed);
        let mut o = Out(String::new());
        render_gate(&mut o, &g);
        let out = o.0;
        for line in [
            "# TYPE lodestar_metadata_new_verified_total counter",
            "lodestar_metadata_new_verified_total{origin=\"network\"} 3",
            "lodestar_searchable_new_total{origin=\"network\"} 2",
            "lodestar_metadata_excluded_total{reason=\"already_stored\"} 10",
            "lodestar_metadata_excluded_total{reason=\"rediscovered\"} 11",
            "lodestar_metadata_excluded_total{reason=\"filtered\"} 12",
            "lodestar_metadata_excluded_total{reason=\"quarantined\"} 13",
            "lodestar_metadata_excluded_total{reason=\"invalid_metainfo\"} 14",
            "lodestar_metadata_excluded_total{reason=\"failed_verification\"} 15",
            "lodestar_metadata_excluded_total{reason=\"duplicate\"} 16",
            "lodestar_metadata_excluded_total{reason=\"store_failed\"} 17",
            "# TYPE lodestar_discovery_to_searchable_seconds histogram",
            "lodestar_discovery_to_searchable_seconds_bucket{le=\"2\"} 0",
            "lodestar_discovery_to_searchable_seconds_bucket{le=\"5\"} 2",
            "lodestar_discovery_to_searchable_seconds_bucket{le=\"7200\"} 2",
            "lodestar_discovery_to_searchable_seconds_bucket{le=\"+Inf\"} 4",
            "lodestar_discovery_to_searchable_seconds_sum 18007",
            "lodestar_discovery_to_searchable_seconds_count 4",
            "lodestar_discovery_to_searchable_dropped_total 41",
        ] {
            assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
        }
        assert_eq!(g.excluded(Excluded::StoreFailed), 17);
    }

    #[test]
    fn hot_column_gauges_render_on_their_own() {
        // With scraping off there's no health handle, but the columns
        // (last seen, search) are still held, so their size still shows.
        let hot = HotColumns::new();
        hot.touch(3, 10);
        let mut out = String::new();
        render_hot(&mut out, &hot);
        assert!(
            out.contains(&format!("lodestar_hot_columns_ids {}\n", hot.len())),
            "{out}"
        );
        assert!(out.contains("lodestar_hot_columns_bytes "), "{out}");
    }

    #[test]
    fn admission_families_render_each_label_with_its_own_value() {
        let p = lodestar_sources::PoolStats::default();
        let set = |a: &std::sync::atomic::AtomicU64, v| a.store(v, Ordering::Relaxed);
        set(&p.admission_waits[2], 3);
        set(&p.admission_wait_cancelled[1], 4);
        set(&p.admission_wakeups, 5);
        set(&p.admission_grants[1][0], 6);
        set(&p.admission_grants[2][1], 7);
        set(&p.admission_refusals[1], 8);
        set(&p.admission_holds, 9);
        set(&p.admission_permit_returns[2], 10);
        set(&p.admission_waiters, 11);
        set(&p.admission_permits_in_use, 12);
        let mut d = StatsSnapshot::default();
        d.peer_permit_returns[2] = 13;
        d.peer_permit_returns[6] = 14;
        let mut o = Out(String::new());
        render_admission(&mut o, &p);
        render_permit_returns(&mut o, &d);
        let out = o.0;
        for line in [
            "# TYPE lodestar_fetch_admission_waits_total counter",
            "lodestar_fetch_admission_waits_total{kind=\"requeue\"} 3",
            "lodestar_fetch_admission_waits_total{kind=\"arrival\"} 0",
            "lodestar_fetch_admission_wait_cancelled_total{reason=\"peers\"} 4",
            "lodestar_fetch_admission_wakeups_total 5",
            "lodestar_fetch_admission_grants_total{kind=\"retry\",class=\"interactive\"} 6",
            "lodestar_fetch_admission_grants_total{kind=\"requeue\",class=\"background\"} 7",
            "lodestar_fetch_admission_refusals_total{reason=\"unacknowledged\"} 8",
            "lodestar_fetch_admission_holds_total 9",
            "lodestar_fetch_admission_permit_returns_total{boundary=\"unused\"} 10",
            "# TYPE lodestar_fetch_admission_waiters gauge",
            "lodestar_fetch_admission_waiters 11",
            "lodestar_fetch_admission_permits_in_use 12",
            "lodestar_dht_peer_permit_returns_total{boundary=\"cancelled_active\"} 13",
            "lodestar_dht_peer_permit_returns_total{boundary=\"handoff_failed\"} 14",
            "lodestar_dht_peer_permit_returns_total{boundary=\"completed\"} 0",
        ] {
            assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
        }
        assert_eq!(
            out.lines()
                .filter(|l| l.starts_with("lodestar_fetch_admission_grants_total{"))
                .count(),
            6
        );
    }

    #[test]
    fn restart_families_render_one_reason_and_each_step() {
        let mut d = StatsSnapshot {
            sample_suppression_reason: lodestar_dht::restart::Reason::Resumed,
            restart_obligations_restored: 7,
            restart_obligations_pending: 5,
            restart_obligations_refused: 3,
            restart_obligations_honoured: 2,
            ..StatsSnapshot::default()
        };
        d.restart_persist_errors[4] = 9;
        let mut o = Out(String::new());
        render_restart(&mut o, &d);
        let out = o.0;
        for line in [
            "# TYPE lodestar_dht_sample_suppression_reason gauge",
            "lodestar_dht_sample_suppression_reason{reason=\"resumed\"} 1",
            "lodestar_dht_sample_suppression_reason{reason=\"first_start\"} 0",
            "lodestar_dht_restart_obligations_restored 7",
            "lodestar_dht_restart_obligations_pending 5",
            "lodestar_dht_restart_obligations_refused_total 3",
            "lodestar_dht_restart_obligations_honoured_total 2",
            "lodestar_dht_restart_persist_errors_total{step=\"snapshot\"} 9",
            "lodestar_dht_restart_persist_errors_total{step=\"lock\"} 0",
        ] {
            assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
        }
        let ones = out
            .lines()
            .filter(|l| {
                l.starts_with("lodestar_dht_sample_suppression_reason{") && l.ends_with(" 1")
            })
            .count();
        assert_eq!(ones, 1, "exactly one reason is set");
    }

    #[test]
    fn storage_and_cache_families_render_once() {
        let g = crate::storage::Gauges::default();
        g.db_bytes.store(4096, Ordering::Relaxed);
        g.wal_bytes.store(0, Ordering::Relaxed);
        g.errors.store(2, Ordering::Relaxed);
        let usage = lodestar_search::CacheUsage {
            file_set_entries: 3,
            file_set_bytes: 1024,
            browse_entries: 5,
            browse_bytes: 640,
        };
        let mut results = lodestar_search::CacheStats::default();
        results.facets.bytes = 77;
        let mut out = String::new();
        render_storage(&mut out, &g);
        render_caches(&mut out, &usage, &results);
        for (family, kind) in [
            ("store_db_bytes", "gauge"),
            ("store_wal_bytes", "gauge"),
            ("search_index_bytes", "gauge"),
            ("storage_sampled_seconds", "gauge"),
            ("storage_sample_errors_total", "counter"),
            ("search_file_cache_entries", "gauge"),
            ("search_file_cache_bytes", "gauge"),
            ("search_browse_cache_entries", "gauge"),
            ("search_browse_cache_bytes", "gauge"),
            ("search_result_cache_entries", "gauge"),
            ("search_result_cache_bytes", "gauge"),
        ] {
            let ty = format!("# TYPE lodestar_{family} {kind}\n");
            assert_eq!(out.matches(&ty).count(), 1, "{family}: {out}");
        }
        for line in [
            "lodestar_store_db_bytes 4096\n",
            "lodestar_store_wal_bytes 0\n",
            "lodestar_storage_sample_errors_total 2\n",
            "lodestar_search_file_cache_bytes 1024\n",
            "lodestar_search_browse_cache_entries 5\n",
            "lodestar_search_browse_cache_bytes 640\n",
            "lodestar_search_result_cache_bytes{kind=\"facets\"} 77\n",
            "lodestar_search_result_cache_entries{kind=\"search\"} 0\n",
        ] {
            assert_eq!(out.matches(line).count(), 1, "{line}: {out}");
        }
        // Not duplicated: the have-filter and hot columns render elsewhere.
        assert!(!out.contains("have_bytes") && !out.contains("hot_columns"));
    }
}
