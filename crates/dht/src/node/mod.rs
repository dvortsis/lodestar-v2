//! The DHT node runtime (architecture §5).
//!
//! One task owns everything: the sockets (one per address family), the
//! routing tables, the transaction table, lookups and rate limiters. It's
//! an actor fed by socket reads, a maintenance tick and a command channel,
//! so there are no locks; the outside world talks to it through [`Node`]
//! and reads counters from shared atomics ([`Stats`]).
//!
//! Defaults are conservative on purpose: DHT fan-out can exhaust a home
//! router's NAT table, so outbound queries are rate-limited and the number
//! outstanding is capped by the [`Intensity`] preset.

mod actor;
mod hints;
mod lookup;
mod peers;
mod proof;
mod sampler;
mod seen;
mod stream;
#[cfg(feature = "testing")]
pub mod testing;
mod tokens;
mod transport;

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use lodestar_metainfo::InfoHash;

use crate::compact::NodeId;
use crate::state::StateError;

pub use stream::{
    LookupEnd, LookupSession, STREAM_BATCH, STREAM_QUEUE, STREAM_SEEN, SessionAdmission,
    SessionAdmit, SessionEnd,
};

/// Crawl intensity presets (architecture §5: Low / Balanced / Aggressive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Intensity {
    Low,
    #[default]
    Balanced,
    Aggressive,
}

impl Intensity {
    /// Outbound queries per second.
    pub fn queries_per_sec(self) -> u32 {
        match self {
            Intensity::Low => 50,
            Intensity::Balanced => 200,
            Intensity::Aggressive => 1000,
        }
    }

    /// Cap on queries awaiting an answer: each is a NAT/conntrack entry on
    /// a home router.
    pub fn max_outstanding(self) -> usize {
        match self {
            Intensity::Low => 256,
            Intensity::Balanced => 1024,
            Intensity::Aggressive => 4096,
        }
    }
}

/// The public routers used when there's no saved state.
pub const DEFAULT_BOOTSTRAP: &[&str] = &[
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "router.utorrent.com:6881",
    "dht.libtorrent.org:25401",
];

/// Harvest strategies (architecture §5), each toggleable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarvestConfig {
    /// BEP 51 traversal: walk the keyspace sampling info hashes.
    pub sample: bool,
    /// Report hashes from `get_peers`/`announce_peer` queries we receive.
    pub passive: bool,
}

impl Default for HarvestConfig {
    fn default() -> Self {
        HarvestConfig {
            sample: true,
            passive: true,
        }
    }
}

/// Where a harvested info hash came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// A BEP 51 `sample_infohashes` reply.
    Sample,
    /// Someone asked us for peers (passive).
    GetPeers,
    /// Someone announced a peer to us (passive); the fastest path to
    /// metadata, since it comes with a live peer.
    Announce,
    /// The result of a get_peers lookup requested with [`Node::get_peers`].
    Lookup,
}

/// An info hash for the pipeline. `Sample` and `GetPeers` harvests are
/// deduplicated by the seen filter; `Announce` and `Lookup` always pass,
/// because they carry peers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harvest {
    pub info_hash: InfoHash,
    pub source: Source,
    pub peers: Vec<SocketAddr>,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// One UDP socket per address; at most one IPv4 and one IPv6.
    pub bind: Vec<SocketAddr>,
    /// How many BEP 42 ids to present (architecture §5: 1–16, default 4).
    pub virtual_ids: usize,
    pub intensity: Intensity,
    /// `host:port` entries, resolved at start.
    pub bootstrap: Vec<String>,
    /// Where to keep `state.bin`; `None` disables persistence.
    pub state_path: Option<PathBuf>,
    pub query_timeout: Duration,
    /// `None` suppresses samples for six hours plus `query_timeout` after
    /// every start, protecting cooldowns lost on restart. Override only
    /// for isolated fixtures or a caller guaranteeing no previous obligations.
    pub sample_start_delay: Option<Duration>,
    /// Maintenance cadence (timeouts, refresh, pings).
    pub tick: Duration,
    pub save_every: Duration,
    /// BEP 43: don't answer queries and ask others not to route to us.
    pub read_only: bool,
    pub harvest: HarvestConfig,
    /// Addresses never contacted or answered: subscribed IP blocklists
    /// (§20.13). Checked on every packet, so it must be cheap.
    pub deny: Option<Deny>,
    /// Explicit local/fixture opt-in; production public discovery leaves false.
    pub allow_private: bool,
    /// Start paused (the crawler's pause, which persists): see
    /// [`NodeHandle::set_paused`].
    pub paused: bool,
    /// CR-0d's crash-safe restart proof (`dht.safe_restart`): `None` keeps
    /// the full suppression after every start. It needs `state_path` and
    /// no `sample_start_delay` override; without them it acts as off.
    pub restart: Option<RestartConfig>,
    /// CR-1a-H's responder hints (`dht.responder_hints`): a lookup first
    /// asks the BEP 51 responder that listed its hash. Off allocates no
    /// hint state and sends nothing differently.
    pub responder_hints: bool,
}

/// The restart proof's settings (docs/decisions/2026-10-30-CR-0d.md).
#[derive(Debug, Clone)]
pub struct RestartConfig {
    /// Where the generation witness lives: outside the data directory.
    /// `None`, or a directory that resolves inside it, means no witness,
    /// so every start suppresses (`no_witness`).
    pub witness_dir: Option<PathBuf>,
    /// Test seam: the wall clock. Its `mono` must be tokio's
    /// `Instant::now()`, the clock the sampler keeps; production uses
    /// [`crate::restart::SystemClock`] ([`RestartConfig::new`]).
    #[doc(hidden)]
    pub clock: Arc<dyn crate::restart::Clock>,
    /// Test seam: do the file work on the actor's thread instead of
    /// tokio's blocking pool, for tests on a paused clock, where a pool
    /// thread's progress isn't ordered with simulated time.
    #[doc(hidden)]
    pub inline_io: bool,
}

impl RestartConfig {
    pub fn new(witness_dir: Option<PathBuf>) -> RestartConfig {
        RestartConfig {
            witness_dir,
            clock: Arc::new(crate::restart::SystemClock),
            inline_io: false,
        }
    }
}

/// A live address check, shared with whoever keeps the lists current.
#[derive(Clone)]
pub struct Deny(pub Arc<dyn Fn(IpAddr) -> bool + Send + Sync>);

impl Deny {
    pub fn denies(&self, ip: IpAddr) -> bool {
        (self.0)(ip)
    }
}

impl std::fmt::Debug for Deny {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Deny")
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: vec![SocketAddr::from(([0, 0, 0, 0], 6881))],
            virtual_ids: 4,
            intensity: Intensity::default(),
            bootstrap: DEFAULT_BOOTSTRAP.iter().map(|s| s.to_string()).collect(),
            state_path: None,
            query_timeout: Duration::from_secs(4),
            sample_start_delay: None,
            tick: Duration::from_secs(1),
            save_every: Duration::from_secs(600),
            read_only: false,
            harvest: HarvestConfig::default(),
            deny: None,
            allow_private: false,
            paused: false,
            restart: None,
            responder_hints: false,
        }
    }
}

/// Counters shared with the rest of the process; all monotonic except the
/// gauges noted.
#[derive(Debug, Default)]
pub struct Stats {
    pub packets_in: AtomicU64,
    pub packets_out: AtomicU64,
    pub malformed_in: AtomicU64,
    pub queries_in: AtomicU64,
    /// Queries from an IP we hadn't sent anything to recently: proof that
    /// unsolicited inbound packets reach the DHT port (the network check).
    pub queries_in_unsolicited: AtomicU64,
    pub responses_in: AtomicU64,
    pub errors_in: AtomicU64,
    pub queries_out: AtomicU64,
    pub timeouts: AtomicU64,
    /// Queries not sent because of the rate limit or outstanding cap.
    pub throttled: AtomicU64,
    /// Replies not sent because of the reply rate limit.
    pub replies_dropped: AtomicU64,
    pub send_errors: AtomicU64,
    /// Responses from an address other than the one queried.
    pub spoofed: AtomicU64,
    /// Gauge: nodes across routing tables.
    pub routing_nodes: AtomicU64,
    /// Gauge: queries awaiting an answer.
    pub outstanding: AtomicU64,
    pub harvested_sample: AtomicU64,
    pub harvested_get_peers: AtomicU64,
    pub harvested_announce: AtomicU64,
    pub lookups_completed: AtomicU64,
    /// Harvests not delivered because the pipeline's channel was full.
    pub harvest_dropped: AtomicU64,
    pub samples_sent: AtomicU64,
    /// Nodes that answered sample_infohashes with an error or timed out.
    pub sample_failures: AtomicU64,
    /// Gauge: unexpired sample obligations.
    pub sample_cooling: AtomicU64,
    /// Gauge: reserved sample requests.
    pub sample_inflight: AtomicU64,
    /// Ignored stale sampler completions.
    pub sample_stale: AtomicU64,
    /// Owned peer requests admitted.
    pub peer_admitted: AtomicU64,
    pub peer_rejected: AtomicU64,
    pub peer_rejected_capacity: AtomicU64,
    pub peer_rejected_paused: AtomicU64,
    pub peer_rejected_caller_closed: AtomicU64,
    pub peer_ack_failed: AtomicU64,
    pub peer_command_full: AtomicU64,
    pub peer_command_closed: AtomicU64,
    /// Admission permits (CR-ADM) returned, by [`PermitReturn`].
    pub peer_permit_returns: [AtomicU64; PermitReturn::ALL.len()],

    pub peer_cancelled: AtomicU64,
    /// Admitted requests dropped while still queued, by [`QueuedDrop`].
    pub peer_queued_dropped: [AtomicU64; QueuedDrop::ALL.len()],
    pub peer_active: AtomicU64,
    pub peer_queued: AtomicU64,
    pub lookup_scheduled: AtomicU64,
    pub lookup_deadlines: AtomicU64,
    pub sample_frontier: AtomicU64,
    pub sample_suppression_seconds: AtomicU64,
    /// Gauge: why this run is or isn't sampling yet, as an index into
    /// [`crate::restart::Reason::ALL`].
    pub sample_suppression_reason: AtomicU64,
    /// Gauges: restrictions restored at start, and those still cooling.
    pub restart_obligations_restored: AtomicU64,
    pub restart_obligations_pending: AtomicU64,
    /// Candidates a restored restriction turned away, and restored
    /// restrictions that expired unsampled. Monitoring only.
    pub restart_obligations_refused: AtomicU64,
    pub restart_obligations_honoured: AtomicU64,
    /// Restart-proof I/O failures, by [`PersistStep`].
    pub restart_persist_errors: [AtomicU64; PersistStep::ALL.len()],
    /// Sampling ticks/admissions deferred by capacity.
    pub sample_deferred: AtomicU64,
    /// BEP 33 scrapes asked for, answered, and refused because the queue
    /// was full.
    pub scrapes_requested: AtomicU64,
    pub scrapes_completed: AtomicU64,
    pub scrapes_dropped: AtomicU64,
    /// Scrapes that found at least one node storing the hash.
    pub scrapes_with_responders: AtomicU64,
    /// Packets to or from a blocklisted address, not sent or ignored.
    pub denied: AtomicU64,
    /// Sum of owned requests' run times, admission to answer, in ms.
    pub peer_lookup_millis: AtomicU64,
    /// Owned requests answered to a waiting caller: the time sum's count.
    pub peer_lookups_answered: AtomicU64,
    /// CR-1a-H: hints kept, and refused at capture by [`HintReject`].
    pub hints_captured: AtomicU64,
    pub hint_capture_rejected: [AtomicU64; HintReject::ALL.len()],
    /// Hints that reached the TTL before any lookup asked for them.
    pub hints_expired: AtomicU64,
    /// Hinted `get_peers` sent, and hints not used by [`HintFallback`].
    pub hint_queries: AtomicU64,
    pub hint_fallbacks: [AtomicU64; HintFallback::ALL.len()],
    /// Hinted replies that carried peers.
    pub hint_useful: AtomicU64,
    /// Ordinary lookup queries skipped by a retained per-IP restriction.
    pub hint_guard_skips: AtomicU64,
    /// Gauges: hashes holding hints, and IPs the guard restricts.
    pub hints_held: AtomicU64,
    pub hint_guard_entries: AtomicU64,
    /// CR-1a-S: peer batches sent to streaming sessions, peers dropped to
    /// backpressure (including the final flush's discards), peers ignored
    /// once a session's seen set was full, and terminals by [`LookupEnd`].
    pub peer_stream_batches: AtomicU64,
    pub peer_stream_backpressure: AtomicU64,
    pub peer_stream_seen_full: AtomicU64,
    pub peer_stream_terminals: [AtomicU64; LookupEnd::ALL.len()],
}

impl Stats {
    pub(crate) fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        StatsSnapshot {
            packets_in: g(&self.packets_in),
            packets_out: g(&self.packets_out),
            malformed_in: g(&self.malformed_in),
            queries_in: g(&self.queries_in),
            queries_in_unsolicited: g(&self.queries_in_unsolicited),
            responses_in: g(&self.responses_in),
            errors_in: g(&self.errors_in),
            queries_out: g(&self.queries_out),
            timeouts: g(&self.timeouts),
            throttled: g(&self.throttled),
            replies_dropped: g(&self.replies_dropped),
            send_errors: g(&self.send_errors),
            spoofed: g(&self.spoofed),
            routing_nodes: g(&self.routing_nodes),
            outstanding: g(&self.outstanding),
            harvested_sample: g(&self.harvested_sample),
            harvested_get_peers: g(&self.harvested_get_peers),
            harvested_announce: g(&self.harvested_announce),
            lookups_completed: g(&self.lookups_completed),
            harvest_dropped: g(&self.harvest_dropped),
            samples_sent: g(&self.samples_sent),
            sample_failures: g(&self.sample_failures),
            sample_cooling: g(&self.sample_cooling),
            sample_inflight: g(&self.sample_inflight),
            sample_stale: g(&self.sample_stale),
            peer_admitted: g(&self.peer_admitted),
            peer_rejected: g(&self.peer_rejected),
            peer_rejected_capacity: g(&self.peer_rejected_capacity),
            peer_rejected_paused: g(&self.peer_rejected_paused),
            peer_rejected_caller_closed: g(&self.peer_rejected_caller_closed),
            peer_ack_failed: g(&self.peer_ack_failed),
            peer_command_full: g(&self.peer_command_full),
            peer_command_closed: g(&self.peer_command_closed),
            peer_permit_returns: std::array::from_fn(|i| g(&self.peer_permit_returns[i])),

            peer_cancelled: g(&self.peer_cancelled),
            peer_queued_dropped: std::array::from_fn(|i| g(&self.peer_queued_dropped[i])),
            peer_active: g(&self.peer_active),
            peer_queued: g(&self.peer_queued),
            lookup_scheduled: g(&self.lookup_scheduled),
            lookup_deadlines: g(&self.lookup_deadlines),
            sample_frontier: g(&self.sample_frontier),
            sample_suppression_seconds: g(&self.sample_suppression_seconds),
            sample_suppression_reason: crate::restart::Reason::ALL
                .get(g(&self.sample_suppression_reason) as usize)
                .copied()
                .unwrap_or(crate::restart::Reason::Off),
            restart_obligations_restored: g(&self.restart_obligations_restored),
            restart_obligations_pending: g(&self.restart_obligations_pending),
            restart_obligations_refused: g(&self.restart_obligations_refused),
            restart_obligations_honoured: g(&self.restart_obligations_honoured),
            restart_persist_errors: std::array::from_fn(|i| g(&self.restart_persist_errors[i])),
            sample_deferred: g(&self.sample_deferred),
            scrapes_requested: g(&self.scrapes_requested),
            scrapes_completed: g(&self.scrapes_completed),
            scrapes_dropped: g(&self.scrapes_dropped),
            scrapes_with_responders: g(&self.scrapes_with_responders),
            denied: g(&self.denied),
            peer_lookup_millis: g(&self.peer_lookup_millis),
            peer_lookups_answered: g(&self.peer_lookups_answered),
            hints_captured: g(&self.hints_captured),
            hint_capture_rejected: std::array::from_fn(|i| g(&self.hint_capture_rejected[i])),
            hints_expired: g(&self.hints_expired),
            hint_queries: g(&self.hint_queries),
            hint_fallbacks: std::array::from_fn(|i| g(&self.hint_fallbacks[i])),
            hint_useful: g(&self.hint_useful),
            hint_guard_skips: g(&self.hint_guard_skips),
            hints_held: g(&self.hints_held),
            hint_guard_entries: g(&self.hint_guard_entries),
            peer_stream_batches: g(&self.peer_stream_batches),
            peer_stream_backpressure: g(&self.peer_stream_backpressure),
            peer_stream_seen_full: g(&self.peer_stream_seen_full),
            peer_stream_terminals: std::array::from_fn(|i| g(&self.peer_stream_terminals[i])),
        }
    }
}

/// Why a captured hint wasn't used and the lookup went straight to
/// ordinary routing (`dht_hint_fallbacks_total{reason}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintFallback {
    /// Its age reached the TTL.
    Expired,
    /// Port 0 or an unsupported address encoding, at send.
    Invalid,
    /// A private, reserved or blocklisted address, at send.
    Private,
    /// No socket of its address family.
    Family,
    /// The per-IP guard, the query rate or the outstanding cap.
    Throttled,
    /// The per-IP guard is full of unexpired restrictions.
    GuardFull,
    /// The socket refused the send.
    SendFailed,
}

impl HintFallback {
    pub const ALL: [HintFallback; 7] = [
        HintFallback::Expired,
        HintFallback::Invalid,
        HintFallback::Private,
        HintFallback::Family,
        HintFallback::Throttled,
        HintFallback::GuardFull,
        HintFallback::SendFailed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HintFallback::Expired => "expired",
            HintFallback::Invalid => "invalid",
            HintFallback::Private => "private",
            HintFallback::Family => "family",
            HintFallback::Throttled => "throttled",
            HintFallback::GuardFull => "guard_full",
            HintFallback::SendFailed => "send_failed",
        }
    }

    pub(crate) fn count(self, stats: &Stats) {
        Stats::inc(&stats.hint_fallbacks[self as usize]);
    }
}

/// Why a responder wasn't kept as a hint
/// (`dht_hint_capture_rejected_total{reason}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintReject {
    /// Port 0 or an unsupported address encoding.
    Invalid,
    /// A private, reserved or blocklisted address.
    Private,
    /// No socket of its address family.
    Family,
    /// The hint table is full of fresh hints.
    Full,
}

impl HintReject {
    pub const ALL: [HintReject; 4] = [
        HintReject::Invalid,
        HintReject::Private,
        HintReject::Family,
        HintReject::Full,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HintReject::Invalid => "invalid",
            HintReject::Private => "private",
            HintReject::Family => "family",
            HintReject::Full => "full",
        }
    }

    pub(crate) fn count(self, stats: &Stats) {
        Stats::inc(&stats.hint_capture_rejected[self as usize]);
    }

    /// The same check failing at send.
    pub(crate) fn at_send(self) -> HintFallback {
        match self {
            HintReject::Invalid => HintFallback::Invalid,
            HintReject::Private => HintFallback::Private,
            HintReject::Family => HintFallback::Family,
            HintReject::Full => HintFallback::GuardFull,
        }
    }
}

/// Why an admitted request left the queue without starting
/// (`dht_peer_queued_dropped_total{reason}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedDrop {
    /// The caller dropped its receiver before the request started.
    CallerClosed,
    /// The pause dropped it.
    Paused,
}

impl QueuedDrop {
    pub const ALL: [QueuedDrop; 2] = [QueuedDrop::CallerClosed, QueuedDrop::Paused];

    pub fn as_str(self) -> &'static str {
        match self {
            QueuedDrop::CallerClosed => "caller_closed",
            QueuedDrop::Paused => "paused",
        }
    }

    pub(crate) fn count(self, stats: &Stats) {
        Stats::inc(&stats.peer_queued_dropped[self as usize]);
    }
}

/// Where an admission permit (CR-ADM) went back to its finder's pool
/// (`dht_peer_permit_returns_total{boundary}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermitReturn {
    /// The last family leg finished and the caller had its answer.
    Completed,
    /// The caller dropped its receiver while the request was queued.
    CancelledQueued,
    /// The caller dropped its receiver while the request ran.
    CancelledActive,
    /// The pause dropped the queued request.
    Paused,
    /// Refused at admission: caller closed, paused or capacity.
    Refused,
    /// The admission acknowledgment couldn't be delivered.
    AckFailed,
    /// The command channel to the actor was full or closed.
    HandoffFailed,
    /// Still held when the actor stopped.
    Shutdown,
}

impl PermitReturn {
    pub const ALL: [PermitReturn; 8] = [
        PermitReturn::Completed,
        PermitReturn::CancelledQueued,
        PermitReturn::CancelledActive,
        PermitReturn::Paused,
        PermitReturn::Refused,
        PermitReturn::AckFailed,
        PermitReturn::HandoffFailed,
        PermitReturn::Shutdown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PermitReturn::Completed => "completed",
            PermitReturn::CancelledQueued => "cancelled_queued",
            PermitReturn::CancelledActive => "cancelled_active",
            PermitReturn::Paused => "paused",
            PermitReturn::Refused => "refused",
            PermitReturn::AckFailed => "ack_failed",
            PermitReturn::HandoffFailed => "handoff_failed",
            PermitReturn::Shutdown => "shutdown",
        }
    }

    /// Counts the return and drops the permit, if there is one.
    pub(crate) fn release(self, stats: &Stats, permit: Option<PeerPermit>) {
        if let Some(permit) = permit {
            Stats::inc(&stats.peer_permit_returns[self as usize]);
            drop(permit);
        }
    }
}

/// Where a restart-proof write failed (`dht_restart_persist_errors_total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistStep {
    Lock,
    Witness,
    Invalidate,
    StateBin,
    Snapshot,
    Marker,
}

impl PersistStep {
    pub const ALL: [PersistStep; 6] = [
        PersistStep::Lock,
        PersistStep::Witness,
        PersistStep::Invalidate,
        PersistStep::StateBin,
        PersistStep::Snapshot,
        PersistStep::Marker,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PersistStep::Lock => "lock",
            PersistStep::Witness => "witness",
            PersistStep::Invalidate => "invalidate",
            PersistStep::StateBin => "state_bin",
            PersistStep::Snapshot => "snapshot",
            PersistStep::Marker => "marker",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatsSnapshot {
    pub packets_in: u64,
    pub packets_out: u64,
    pub malformed_in: u64,
    pub queries_in: u64,
    pub queries_in_unsolicited: u64,
    pub responses_in: u64,
    pub errors_in: u64,
    pub queries_out: u64,
    pub timeouts: u64,
    pub throttled: u64,
    pub replies_dropped: u64,
    pub send_errors: u64,
    pub spoofed: u64,
    pub routing_nodes: u64,
    pub outstanding: u64,
    pub harvested_sample: u64,
    pub harvested_get_peers: u64,
    pub harvested_announce: u64,
    pub lookups_completed: u64,
    pub harvest_dropped: u64,
    pub samples_sent: u64,
    pub sample_failures: u64,
    pub sample_cooling: u64,
    pub sample_inflight: u64,
    pub sample_stale: u64,
    pub peer_admitted: u64,
    pub peer_rejected: u64,
    pub peer_rejected_capacity: u64,
    pub peer_rejected_paused: u64,
    pub peer_rejected_caller_closed: u64,
    pub peer_ack_failed: u64,
    pub peer_command_full: u64,
    pub peer_command_closed: u64,
    /// By [`PermitReturn::ALL`].
    pub peer_permit_returns: [u64; PermitReturn::ALL.len()],

    pub peer_cancelled: u64,
    /// By [`QueuedDrop::ALL`].
    pub peer_queued_dropped: [u64; QueuedDrop::ALL.len()],
    pub peer_active: u64,
    pub peer_queued: u64,
    pub lookup_scheduled: u64,
    pub lookup_deadlines: u64,
    pub sample_frontier: u64,
    pub sample_suppression_seconds: u64,
    pub sample_suppression_reason: crate::restart::Reason,
    pub restart_obligations_restored: u64,
    pub restart_obligations_pending: u64,
    pub restart_obligations_refused: u64,
    pub restart_obligations_honoured: u64,
    /// By [`PersistStep::ALL`].
    pub restart_persist_errors: [u64; 6],
    pub sample_deferred: u64,
    pub scrapes_requested: u64,
    pub scrapes_completed: u64,
    pub scrapes_dropped: u64,
    pub scrapes_with_responders: u64,
    pub denied: u64,
    pub peer_lookup_millis: u64,
    pub peer_lookups_answered: u64,
    pub hints_captured: u64,
    /// By [`HintReject::ALL`].
    pub hint_capture_rejected: [u64; HintReject::ALL.len()],
    pub hints_expired: u64,
    pub hint_queries: u64,
    /// By [`HintFallback::ALL`].
    pub hint_fallbacks: [u64; HintFallback::ALL.len()],
    pub hint_useful: u64,
    pub hint_guard_skips: u64,
    pub hints_held: u64,
    pub hint_guard_entries: u64,
    pub peer_stream_batches: u64,
    pub peer_stream_backpressure: u64,
    pub peer_stream_seen_full: u64,
    /// By [`LookupEnd::ALL`].
    pub peer_stream_terminals: [u64; LookupEnd::ALL.len()],
}

impl StatsSnapshot {
    /// Responses per query sent. A collapsing ratio is the symptom of NAT
    /// saturation the dashboard warns about (§5).
    pub fn reply_ratio(&self) -> f64 {
        if self.queries_out == 0 {
            0.0
        } else {
            (self.responses_in + self.errors_in) as f64 / self.queries_out as f64
        }
    }
}

/// Point-in-time view of the node's identity and tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub ids: Vec<NodeId>,
    pub external_ip: Option<IpAddr>,
    /// Routing-table size per bound address, in `Config::bind` order.
    pub table_sizes: Vec<usize>,
    /// Every node in the routing tables.
    pub nodes: Vec<crate::compact::NodeInfo>,
    pub lookups: usize,
    /// Nodes queued for BEP 51 sampling.
    pub sample_frontier: usize,
    /// Nodes with a known "sample again at" time.
    pub sample_known: usize,
    /// Info hashes with announced peers in our store.
    pub stored_peer_hashes: usize,
    /// Memory held by the seen filter (fixed at start).
    pub seen_filter_bytes: usize,
    /// The intensity the node runs at now.
    pub intensity: Intensity,
    pub paused: bool,
    /// What other nodes report as our address.
    pub external: ExternalVotes,
    /// How long ago each of the most recent distinct networks (/24 or
    /// /64, at most 64) last sent a query we didn't solicit.
    pub unsolicited_sources: Vec<std::time::Duration>,
}

/// The external address other nodes say they see us at (the BEP 42 `ip`
/// key of their replies), tallied over the last 32 distinct reporting
/// networks (one vote per /24 or /64), IPv4 and IPv6 apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExternalVotes {
    /// The majority address: set once at least 5 reporters agree and
    /// they're more than half of them.
    pub ip: Option<IpAddr>,
    /// The port most of those reporters saw (a NAT may rewrite it);
    /// `None` without a majority address.
    pub port: Option<u16>,
    /// Distinct reporting networks in the window.
    pub voters: usize,
    /// Reporting networks naming the leading address.
    pub agreeing: usize,
}

pub(crate) enum Command {
    Snapshot(oneshot::Sender<Snapshot>),
    GetPeers(InfoHash),
    RequestPeers(InfoHash, PeerAdmission, Option<PeerPermit>),
    /// A streaming request (CR-1a-S).
    RequestSession(InfoHash, Box<dyn SessionAdmit>, Option<PeerPermit>),
    Scrape(InfoHash, oneshot::Sender<Option<DhtEstimate>>),
    SetIntensity(Intensity),
    SetPaused(bool),
}

/// An owned terminal result. Dropping the receiver cancels the request;
/// the actor reclaims all RPCs no later than its next maintenance tick.
pub type PeerResult = oneshot::Receiver<Vec<SocketAddr>>;
/// `None` means backpressure/paused; it must not consume a network attempt.
pub type PeerAdmission = oneshot::Sender<Option<PeerResult>>;
/// One owned request's slot, under coordinated admission (CR-ADM). The
/// caller's semaphore holds [`PEER_REQUEST_CAPACITY`] of them; the actor
/// keeps the permit for as long as the request holds a slot, and dropping
/// it gives the slot back.
pub type PeerPermit = tokio::sync::OwnedSemaphorePermit;
/// Owned peer requests the actor runs or queues at once, across families:
/// one slot per logical request, however many family legs it runs.
pub const PEER_REQUEST_CAPACITY: usize = 32;

/// A DHT swarm estimate (BEP 33) from the nodes closest to a hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DhtEstimate {
    pub seeds: u32,
    /// Peers that aren't seeds.
    pub peers: u32,
    /// Nodes whose filters went into the estimate: 0 means no node near
    /// the hash stores it, which is "unknown" rather than "dead".
    pub responders: u8,
}

/// A cloneable handle for peer finding, scraping and node controls.
#[derive(Debug, Clone)]
pub struct NodeHandle {
    stats: Arc<Stats>,
    commands: mpsc::Sender<Command>,
}

impl NodeHandle {
    /// Nonblocking, acknowledged peer lookup admission. There is no blind
    /// internal backlog; a full command channel rejects by closing `admit`.
    /// `true` means enqueued only; `Some` on the admission channel owns work.
    /// Dropping the returned terminal receiver cancels all family legs.
    pub fn request_peers(&self, hash: InfoHash, admit: PeerAdmission) -> bool {
        self.request_peers_with(hash, admit, None)
    }

    /// [`request_peers`](Self::request_peers) carrying the request's
    /// admission permit. The actor holds it while the request holds a slot;
    /// a failed handoff returns it here.
    pub fn request_peers_with(
        &self,
        hash: InfoHash,
        admit: PeerAdmission,
        permit: Option<PeerPermit>,
    ) -> bool {
        let (counter, cmd) = match self
            .commands
            .try_send(Command::RequestPeers(hash, admit, permit))
        {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Full(cmd)) => (&self.stats.peer_command_full, cmd),
            Err(mpsc::error::TrySendError::Closed(cmd)) => (&self.stats.peer_command_closed, cmd),
        };
        Stats::inc(counter);
        if let Command::RequestPeers(_, admit, permit) = cmd {
            // The caller sees the closed admission before the permit is
            // back, as with a refusal.
            drop(admit);
            PermitReturn::HandoffFailed.release(&self.stats, permit);
        }
        false
    }

    /// A streaming peer request (CR-1a-S): an admitted request hands the
    /// caller a session of bounded peer batches and a separate terminal,
    /// allocated only once the actor admits it. Otherwise as
    /// [`request_peers_with`](Self::request_peers_with).
    pub fn request_session(
        &self,
        hash: InfoHash,
        admit: Box<dyn SessionAdmit>,
        permit: Option<PeerPermit>,
    ) -> bool {
        let (counter, cmd) = match self
            .commands
            .try_send(Command::RequestSession(hash, admit, permit))
        {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Full(cmd)) => (&self.stats.peer_command_full, cmd),
            Err(mpsc::error::TrySendError::Closed(cmd)) => (&self.stats.peer_command_closed, cmd),
        };
        Stats::inc(counter);
        if let Command::RequestSession(_, admit, permit) = cmd {
            drop(admit);
            PermitReturn::HandoffFailed.release(&self.stats, permit);
        }
        false
    }

    /// Estimates a swarm's seeds and peers from the BEP 33 filters of the
    /// nodes closest to `hash`. `None` when the scrape queue is full or
    /// the node has stopped. Only counts come back: no filters and no
    /// addresses leave the node (§18.7).
    pub async fn scrape(&self, hash: InfoHash) -> Option<DhtEstimate> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(Command::Scrape(hash, tx)).await.ok()?;
        rx.await.ok().flatten()
    }

    /// Runs the node at `intensity` from now on: its query and reply
    /// rates and its cap on outstanding queries (the settings registry's
    /// live change, §11). `false` if the node has stopped.
    pub async fn set_intensity(&self, intensity: Intensity) -> bool {
        self.commands
            .send(Command::SetIntensity(intensity))
            .await
            .is_ok()
    }

    /// Pauses or resumes harvesting (the crawler's pause). Paused, the
    /// node sends no BEP 51 samples and starts no `get_peers` lookups or
    /// scrapes (legacy queued lookups wait, owned queued requests finish
    /// empty, and queued/new scrapes answer
    /// `None`), and reports no harvests. It keeps its routing table up
    /// and answers other nodes' queries. Lookups already running finish.
    /// `false` if the node has stopped.
    pub async fn set_paused(&self, paused: bool) -> bool {
        self.commands.send(Command::SetPaused(paused)).await.is_ok()
    }

    /// `None` if the node has stopped.
    pub async fn snapshot(&self) -> Option<Snapshot> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(Command::Snapshot(tx)).await.ok()?;
        rx.await.ok()
    }
}

/// Binds a non-blocking UDP socket and returns it twice: as a tokio socket
/// for receiving, and as a std clone for sending. Sends never wait: a
/// `send_to` on a non-blocking UDP socket only fails when the kernel buffer
/// is full, whereas tokio's `try_send_to` also fails until the reactor has
/// seen the socket become writable, which drops a fresh node's first
/// (bootstrap) packets. IPv6 sockets are IPv6-only, so an IPv4 and an IPv6
/// socket can share a port.
fn bind(addr: SocketAddr) -> io::Result<(UdpSocket, std::net::UdpSocket)> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    // Room for bursts of replies; best effort, the OS may cap it.
    let requested = 4 << 20;
    let set_errors = usize::from(socket.set_recv_buffer_size(requested).is_err());
    let receive = socket.recv_buffer_size();
    let send = socket.send_buffer_size();
    let read_errors = usize::from(receive.is_err()) + usize::from(send.is_err());
    lodestar_log::log_event!(
        crate::log_events::SOCKET_BUFFERS,
        receive_requested = requested,
        send_requested = 0usize,
        receive = receive.unwrap_or(0),
        send = send.unwrap_or(0),
        set_errors = set_errors,
        read_errors = read_errors
    );
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    let std_socket: std::net::UdpSocket = socket.into();
    let send = std_socket.try_clone()?;
    Ok((UdpSocket::from_std(std_socket)?, send))
}

/// Handle to a running node.
#[derive(Debug)]
pub struct Node {
    commands: mpsc::Sender<Command>,
    shutdown: watch::Sender<bool>,
    stats: Arc<Stats>,
    local_addrs: Vec<SocketAddr>,
    task: JoinHandle<Result<(), StateError>>,
}

impl Node {
    /// Binds the sockets (so address errors surface here) and starts the
    /// node in the background. Harvested info hashes go to `harvest`
    /// (dropped and counted if it's full, so the DHT never blocks).
    pub async fn start(config: Config, harvest: Option<mpsc::Sender<Harvest>>) -> io::Result<Node> {
        if config.bind.is_empty() || config.bind.len() > 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bind one IPv4 and/or one IPv6 address",
            ));
        }
        if config.bind.len() == 2 && config.bind[0].is_ipv6() == config.bind[1].is_ipv6() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "two sockets of the same address family",
            ));
        }
        let mut sockets = Vec::new();
        for addr in &config.bind {
            let (recv, send) = bind(*addr)?;
            sockets.push((
                transport::RecvSocket::Udp(recv),
                transport::SendSocket::Udp(send),
            ));
        }
        let local_addrs = sockets
            .iter()
            .map(|(recv, _)| recv.local_addr())
            .collect::<io::Result<Vec<_>>>()?;
        let rng = {
            use rand::SeedableRng;
            rand::rngs::StdRng::from_rng(&mut rand::rng())
        };
        Ok(Node::spawn(config, sockets, local_addrs, rng, harvest))
    }

    /// Starts the actor on bound sockets. Shared by [`Node::start`] and the
    /// simulator's `testing::start`, so both get the same wiring.
    fn spawn(
        config: Config,
        sockets: Vec<(transport::RecvSocket, transport::SendSocket)>,
        local_addrs: Vec<SocketAddr>,
        rng: rand::rngs::StdRng,
        harvest: Option<mpsc::Sender<Harvest>>,
    ) -> Node {
        let stats = Arc::new(Stats::default());
        let (commands, command_rx) = mpsc::channel(64);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(actor::run(
            config,
            sockets,
            rng,
            stats.clone(),
            command_rx,
            shutdown_rx,
            harvest,
        ));
        Node {
            commands,
            shutdown,
            stats,
            local_addrs,
            task,
        }
    }

    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.local_addrs
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    /// A handle that can outlive borrows of the node, for scrapes.
    pub fn handle(&self) -> NodeHandle {
        NodeHandle {
            stats: self.stats.clone(),
            commands: self.commands.clone(),
        }
    }

    /// Asks the node to look up peers for a hash (strategy 3: hashes with
    /// no peer yet). The result arrives as a [`Source::Lookup`] harvest.
    /// Returns `false` if the request queue is full.
    pub fn get_peers(&self, info_hash: InfoHash) -> bool {
        self.commands.try_send(Command::GetPeers(info_hash)).is_ok()
    }

    /// `None` if the node has stopped.
    pub async fn snapshot(&self) -> Option<Snapshot> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(Command::Snapshot(tx)).await.ok()?;
        rx.await.ok()
    }

    /// Stops the node, saving its state when persistence is on.
    pub async fn shutdown(self) -> Result<(), StateError> {
        let _ = self.shutdown.send(true);
        match self.task.await {
            Ok(result) => result,
            Err(e) => Err(StateError::Io(io::Error::other(e))),
        }
    }
}
