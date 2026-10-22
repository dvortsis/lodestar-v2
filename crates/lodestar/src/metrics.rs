//! Prometheus text exposition of the pipeline's counters (architecture
//! §12: metrics for every pipeline stage).

use std::fmt::Write;
use std::sync::atomic::Ordering;

use lodestar_dht::node::StatsSnapshot;
use lodestar_peer_wire::FailureKind;
use lodestar_pipeline::Monitor;
use lodestar_sources::Loss;

pub const FAILURE_KINDS: [(FailureKind, &str); 6] = [
    (FailureKind::Unreachable, "unreachable"),
    (FailureKind::TimedOut, "timed_out"),
    (FailureKind::Refused, "refused"),
    (FailureKind::EncryptionFailed, "encryption_failed"),
    (FailureKind::Invalid, "invalid"),
    (FailureKind::Oversize, "oversize"),
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

pub fn render(m: &Monitor, dht: Option<&StatsSnapshot>) -> String {
    let mut o = Out(String::with_capacity(8192));
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);

    o.one(
        "start_time_seconds",
        "gauge",
        "Unix time the pipeline started.",
        m.started_at(),
    );
    o.one("torrents", "gauge", "Torrents stored.", m.torrents());

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
    o.one(
        "fetch_success_total",
        "counter",
        "Info dictionaries fetched and verified.",
        get(&p.fetched),
    );
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
            "dht_lookups_completed_total",
            "counter",
            "get_peers lookups finished.",
            d.lookups_completed,
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
        "Torrents indexed since start.",
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
        "Indexer read errors.",
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
