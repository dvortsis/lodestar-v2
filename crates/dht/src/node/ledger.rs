//! CR-LEDGER: the lookup ledger, counters only (`docs/metrics.md`, "Lookup
//! ledger"). It measures what CR-0 derived: how long a peer request holds
//! its slot and why it let go (`W`), the queries spent by purpose (`q`),
//! when values arrive, and what a responder hint came to. Nothing here
//! changes a packet, timer or decision.
//!
//! Every label is a fixed enum: never a hash, address, port or node id.

use std::sync::atomic::{AtomicU64, Ordering};

/// Why a peer request (one slot, all its family legs) ended
/// (`dht_lookup_ended_total{reason}`). Exactly one per started request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerEnd {
    /// Every leg converged on its closest set (or ran out of candidates),
    /// and values were found.
    Values,
    /// The same, without values.
    NoValues,
    /// The 25 s request deadline ran out first.
    Deadline,
    /// The request's 64-query budget ran out before convergence.
    QueryCap,
    /// The caller dropped its receiver while the request ran.
    Cancelled,
    /// Still running when the actor stopped.
    Shutdown,
}

impl LedgerEnd {
    pub const ALL: [LedgerEnd; 6] = [
        LedgerEnd::Values,
        LedgerEnd::NoValues,
        LedgerEnd::Deadline,
        LedgerEnd::QueryCap,
        LedgerEnd::Cancelled,
        LedgerEnd::Shutdown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LedgerEnd::Values => "values",
            LedgerEnd::NoValues => "no_values",
            LedgerEnd::Deadline => "deadline",
            LedgerEnd::QueryCap => "query_cap",
            LedgerEnd::Cancelled => "cancelled",
            LedgerEnd::Shutdown => "shutdown",
        }
    }
}

/// What a DHT query we sent was for (`dht_queries_sent_total{purpose}`).
/// The actor never sends `announce_peer`, so there's no such purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryPurpose {
    /// `get_peers` of a peer request's ordinary routing.
    PeerLookup,
    /// A responder hint's one direct `get_peers` (CR-1a-H).
    Hint,
    /// A BEP 33 scrape lookup's `get_peers`.
    Scrape,
    /// The self-lookup of a bootstrap round or a BEP 42 rejoin, routers
    /// included.
    Bootstrap,
    /// Stale-bucket and join refresh `find_node`.
    Refresh,
    /// The sampler's exploration `find_node` (CR-1b).
    Explore,
    /// BEP 51 `sample_infohashes`.
    Sample,
    /// Pings: questionable table entries, replacement checks, and queriers
    /// being verified.
    Ping,
}

impl QueryPurpose {
    pub const ALL: [QueryPurpose; 8] = [
        QueryPurpose::PeerLookup,
        QueryPurpose::Hint,
        QueryPurpose::Scrape,
        QueryPurpose::Bootstrap,
        QueryPurpose::Refresh,
        QueryPurpose::Explore,
        QueryPurpose::Sample,
        QueryPurpose::Ping,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            QueryPurpose::PeerLookup => "peer_lookup",
            QueryPurpose::Hint => "hint",
            QueryPurpose::Scrape => "scrape",
            QueryPurpose::Bootstrap => "bootstrap",
            QueryPurpose::Refresh => "refresh",
            QueryPurpose::Explore => "explore",
            QueryPurpose::Sample => "sample",
            QueryPurpose::Ping => "ping",
        }
    }
}

/// What a responder hint came to (`dht_hint_outcomes_total{outcome}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintOutcome {
    /// The hinted `get_peers` was answered with values.
    Values,
    /// Answered without values (an error reply included).
    NoValues,
    /// Timed out, or its request ended before the reply.
    Unanswered,
    /// Not sent: the per-IP guard refused it.
    GuardSkipped,
}

impl HintOutcome {
    pub const ALL: [HintOutcome; 4] = [
        HintOutcome::Values,
        HintOutcome::NoValues,
        HintOutcome::Unanswered,
        HintOutcome::GuardSkipped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HintOutcome::Values => "values",
            HintOutcome::NoValues => "no_values",
            HintOutcome::Unanswered => "unanswered",
            HintOutcome::GuardSkipped => "guard_skipped",
        }
    }
}

/// Upper bounds (`le`, in ms) of `dht_lookup_first_values_seconds`. The
/// last is the 25 s deadline; later values land in `+Inf`.
pub const FIRST_VALUES_MS: [u64; 9] = [
    500, 1_000, 2_000, 4_000, 8_000, 12_000, 16_000, 20_000, 25_000,
];
/// Upper bounds (`le`) of `dht_lookup_values`. A request keeps at most 64
/// distinct values.
pub const VALUES_BUCKETS: [u64; 8] = [0, 1, 2, 4, 8, 16, 32, 64];

/// A fixed-bucket histogram of atomics: one increment per bucket and one
/// add to the sum per observation, no allocation and no lock.
#[derive(Debug)]
pub struct Histogram<const N: usize> {
    bounds: &'static [u64; N],
    /// Per bucket, not cumulative; the last slot is `+Inf`'s own.
    counts: [AtomicU64; N],
    over: AtomicU64,
    sum: AtomicU64,
}

impl<const N: usize> Histogram<N> {
    pub const fn new(bounds: &'static [u64; N]) -> Self {
        Histogram {
            bounds,
            counts: [const { AtomicU64::new(0) }; N],
            over: AtomicU64::new(0),
            sum: AtomicU64::new(0),
        }
    }

    pub fn observe(&self, v: u64) {
        match self.bounds.iter().position(|b| v <= *b) {
            Some(i) => self.counts[i].fetch_add(1, Ordering::Relaxed),
            None => self.over.fetch_add(1, Ordering::Relaxed),
        };
        self.sum.fetch_add(v, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> HistogramSnapshot<N> {
        let mut cumulative = [0; N];
        let mut total = 0;
        for (i, c) in self.counts.iter().enumerate() {
            total += c.load(Ordering::Relaxed);
            cumulative[i] = total;
        }
        HistogramSnapshot {
            bounds: *self.bounds,
            cumulative,
            count: total + self.over.load(Ordering::Relaxed),
            sum: self.sum.load(Ordering::Relaxed),
        }
    }
}

/// A histogram as read: cumulative counts per bound (Prometheus `le`),
/// the total (`+Inf` and `_count`), and the sum of observations in their
/// unit (ms for times, values for counts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistogramSnapshot<const N: usize> {
    pub bounds: [u64; N],
    pub cumulative: [u64; N],
    pub count: u64,
    pub sum: u64,
}

impl<const N: usize> Default for HistogramSnapshot<N> {
    fn default() -> Self {
        HistogramSnapshot {
            bounds: [0; N],
            cumulative: [0; N],
            count: 0,
            sum: 0,
        }
    }
}

/// The ledger's counters, inside [`super::Stats`].
#[derive(Debug)]
pub struct Ledger {
    /// Requests ended, and their slot residence (start to end) in ms, by
    /// [`LedgerEnd`].
    pub ended: [AtomicU64; LedgerEnd::ALL.len()],
    pub residence_millis: [AtomicU64; LedgerEnd::ALL.len()],
    /// Queries sent, by [`QueryPurpose`]; they sum to `queries_out`.
    pub queries: [AtomicU64; QueryPurpose::ALL.len()],
    /// Ended requests that got values: ms from start to the first.
    pub first_values: Histogram<{ FIRST_VALUES_MS.len() }>,
    /// Ended requests that never got values.
    pub without_values: AtomicU64,
    /// Values each ended request found, across its legs.
    pub values: Histogram<{ VALUES_BUCKETS.len() }>,
    /// By [`HintOutcome`].
    pub hint_outcomes: [AtomicU64; HintOutcome::ALL.len()],
    /// Gauges: requests holding a slot (running or queued), and the cap.
    pub slots_occupied: AtomicU64,
    pub slots_capacity: AtomicU64,
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger {
            ended: Default::default(),
            residence_millis: Default::default(),
            queries: Default::default(),
            first_values: Histogram::new(&FIRST_VALUES_MS),
            without_values: AtomicU64::new(0),
            values: Histogram::new(&VALUES_BUCKETS),
            hint_outcomes: Default::default(),
            slots_occupied: AtomicU64::new(0),
            slots_capacity: AtomicU64::new(0),
        }
    }
}

/// One request's ledger entry, kept on the request while it runs.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Entry {
    /// ms from start to the first reply with values.
    pub first_values_ms: Option<u64>,
    /// Distinct values the request found across its legs (at most 64, as
    /// a final reply holds), filled in at its end.
    pub values: u64,
    /// A leg ended on the query budget, not on convergence.
    pub capped: bool,
}

impl Ledger {
    pub(crate) fn query(&self, purpose: QueryPurpose) {
        self.queries[purpose as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn hint(&self, outcome: HintOutcome) {
        self.hint_outcomes[outcome as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// Records a request's end: called exactly once per started request.
    pub(crate) fn end(&self, why: LedgerEnd, entry: &Entry, residence_ms: u64) {
        self.ended[why as usize].fetch_add(1, Ordering::Relaxed);
        self.residence_millis[why as usize].fetch_add(residence_ms, Ordering::Relaxed);
        match entry.first_values_ms {
            Some(ms) => self.first_values.observe(ms),
            None => {
                self.without_values.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.values.observe(entry.values);
    }

    pub fn snapshot(&self) -> LedgerSnapshot {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        LedgerSnapshot {
            ended: std::array::from_fn(|i| g(&self.ended[i])),
            residence_millis: std::array::from_fn(|i| g(&self.residence_millis[i])),
            queries: std::array::from_fn(|i| g(&self.queries[i])),
            first_values: self.first_values.snapshot(),
            without_values: g(&self.without_values),
            values: self.values.snapshot(),
            hint_outcomes: std::array::from_fn(|i| g(&self.hint_outcomes[i])),
            slots_occupied: g(&self.slots_occupied),
            slots_capacity: g(&self.slots_capacity),
        }
    }
}

/// [`Ledger`] as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LedgerSnapshot {
    /// By [`LedgerEnd::ALL`].
    pub ended: [u64; LedgerEnd::ALL.len()],
    pub residence_millis: [u64; LedgerEnd::ALL.len()],
    /// By [`QueryPurpose::ALL`].
    pub queries: [u64; QueryPurpose::ALL.len()],
    pub first_values: HistogramSnapshot<{ FIRST_VALUES_MS.len() }>,
    pub without_values: u64,
    pub values: HistogramSnapshot<{ VALUES_BUCKETS.len() }>,
    /// By [`HintOutcome::ALL`].
    pub hint_outcomes: [u64; HintOutcome::ALL.len()],
    pub slots_occupied: u64,
    pub slots_capacity: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets_are_le_and_cumulative() {
        let h = Histogram::new(&VALUES_BUCKETS);
        for v in [0, 1, 3, 64, 65, 128] {
            h.observe(v);
        }
        let s = h.snapshot();
        assert_eq!(s.cumulative, [1, 2, 2, 3, 3, 3, 3, 4]);
        assert_eq!(s.count, 6);
        assert_eq!(s.sum, 261);
    }

    #[test]
    fn an_end_counts_once_in_every_partition() {
        let l = Ledger::default();
        let got = Entry {
            first_values_ms: Some(1_500),
            values: 3,
            capped: false,
        };
        l.end(LedgerEnd::Values, &got, 2_000);
        l.end(LedgerEnd::Deadline, &Entry::default(), 25_000);
        let s = l.snapshot();
        assert_eq!(s.ended.iter().sum::<u64>(), 2);
        assert_eq!(s.residence_millis[LedgerEnd::Deadline as usize], 25_000);
        assert_eq!(s.first_values.count + s.without_values, 2);
        assert_eq!(s.values.count, 2);
        assert_eq!(s.first_values.cumulative[1], 0);
        assert_eq!(s.first_values.cumulative[2], 1);
    }
}
