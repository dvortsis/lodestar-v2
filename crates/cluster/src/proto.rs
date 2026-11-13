//! The cluster protocol's messages (architecture §19.2), as they travel in
//! [`frame`](crate::frame)s.
//!
//! | From → to | Message | Here |
//! |---|---|---|
//! | W → C | [`ToCoordinator::Hello`] | protocol version, worker id, capabilities |
//! | C → W | [`ToWorker::Welcome`] | the accepted version and the session's timings |
//! | C → W | [`ToWorker::Assign`] | shape only: a revision and an opaque payload |
//! | C ↔ W | [`ToCoordinator::Heartbeat`] / [`ToWorker::HeartbeatAck`] | every 5 s: stats, clock offset, lease renewal |
//! | W → C | [`ToCoordinator::Claim`] | up to [`MAX_CLAIM_BATCH`] new keys, each with up to [`MAX_HINTS`] peer hints |
//! | C → W | [`ToWorker::Grants`] | per key a grant (fencing token, TTL) or a denial; batch 0 carries re-grants |
//! | C → W | [`ToWorker::Hints`] | peer hints another worker found for a key this worker holds (in memory only, §18.7) |
//! | W → C | [`ToCoordinator::Outcome`] | terminal outcomes; stale tokens are ignored |
//! | W → C | [`ToCoordinator::Results`] / [`ToCoordinator::Counters`] | an opaque, batch-numbered payload (WK-6's codecs) |
//! | C → W | [`ToWorker::Accepted`] | a `Results` or `Counters` batch was taken, or refused for now |
//! | C → W | [`ToWorker::Snapshot`] / W → C [`ToCoordinator::SnapshotAck`] | a versioned, chunked snapshot (the filter, the dictionary), paced by acknowledgements |
//! | C → W | [`ToWorker::Reverify`] / W → C [`ToCoordinator::Reverified`] | re-verification of sampled keys (§19.4) |
//! | W → C | [`ToCoordinator::Renew`] / C → W [`ToWorker::Renewed`] | certificate renewal, rate-limited |
//! | C → W | [`ToWorker::Drain`] / [`ToWorker::Stop`] | graceful drain; a stop with its reason |
//! | W → C | [`ToCoordinator::Drained`] | the drain finished |
//! | W → C | [`ToCoordinator::Holding`] / C → W [`ToWorker::Reconciled`] | the leases a worker still holds, in parts, after a replay with drift (minor 1, WK-11b) |
//!
//! Every v1 message exists (WK-0). The payloads other slices define stay
//! opaque bytes here: `Assign`'s (WK-5), `Results` and `Counters` (WK-6),
//! `HaveSnapshot` and `HaveDelta` (WK-3), the snapshot chunks' content
//! (WK-3, WK-8), the re-verification reply (WK-13) and `Update` (later).
//! Until WK-G freezes protocol 1.0 they may still change (plan Q11).
//!
//! Every limit a message can break is checked by `validate`, on both
//! transports, so the in-process worker goes through the same checks.

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::JoinGrant;

/// A worker refuses a coordinator, and the coordinator a worker, with
/// another major version.
pub const PROTOCOL_MAJOR: u16 = 1;
/// Minor 1 (WK-11b): the coordinator takes [`ToCoordinator::Holding`].
/// A minor version only adds messages a peer sends when the other side's
/// minor says it knows them, so peers of any minor work together.
pub const PROTOCOL_MINOR: u16 = 1;
/// The first minor whose coordinator takes [`ToCoordinator::Holding`]: a
/// worker sends it only when `Welcome.minor` is at least this.
pub const MINOR_HOLDING: u16 = 1;

/// Keys per claim (§19.2: "≤ 512 or every 250 ms").
pub const MAX_CLAIM_BATCH: usize = 512;
/// Outcomes per report, and grants per reply.
pub const MAX_OUTCOME_BATCH: usize = 1024;
pub const MAX_GRANT_BATCH: usize = 1024;
pub const MAX_EXTERNAL_IPS: usize = 8;
/// Opaque payloads of the messages later slices fill in.
pub const MAX_OPAQUE: usize = 512 * 1024;
/// Peer hints per claimed key, and keys per hint relay.
pub const MAX_HINTS: usize = 8;
pub const MAX_HINT_BATCH: usize = 1024;
/// Bytes in one snapshot chunk (the rest of a frame is headroom).
pub const MAX_CHUNK: usize = 512 * 1024;
/// Chunks in one snapshot: 64 GiB at most, far above any real filter.
pub const MAX_CHUNKS: u32 = 1 << 17;
/// Keys in one re-verification request.
pub const MAX_REVERIFY: usize = 256;
/// CA certificates in a renewal grant (current and previous).
pub const MAX_CAS: usize = 4;
pub const MAX_CERT: usize = 4096;
/// Leases in one [`Holding`] part: about 240 KiB, well under a frame.
pub const MAX_HOLDING_BATCH: usize = 8192;
/// Parts in one [`Holding`] list: 2,097,152 leases, twice the largest pool
/// an assignment allows (`assign::MAX_POOL_CAP`, keys held plus results
/// out). A worker holding more doesn't send a list.
pub const MAX_HOLDING_PARTS: u32 = 256;
pub const MAX_HOLDING: usize = MAX_HOLDING_BATCH * MAX_HOLDING_PARTS as usize;

/// A worker's first message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub major: u16,
    pub minor: u16,
    /// Must be the id in the worker's certificate.
    pub worker: u32,
    pub capabilities: Capabilities,
}

impl Hello {
    pub fn new(worker: u32, capabilities: Capabilities) -> Hello {
        Hello {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            worker,
            capabilities,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub cores: u16,
    /// Memory the worker may use, in MiB.
    pub ram_mb: u32,
    pub external_ips: Vec<IpAddr>,
    /// Companions present, a bitset (§18).
    pub companions: u64,
    /// The egress the worker reports (direct, VPN, …), a code.
    pub egress: u8,
}

/// The coordinator's answer to an accepted `Hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub major: u16,
    pub minor: u16,
    pub worker: u32,
    pub heartbeat_ms: u32,
    pub lease_ttl_secs: u32,
    /// The worker's certificate is from a CA being rotated out: renew now.
    pub renew_now: bool,
}

/// What to run (§19.5). This slice fixes only its shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assign {
    pub revision: u64,
    pub payload: Vec<u8>,
}

/// Sent every heartbeat interval. It renews all the worker's leases.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub seq: u64,
    /// The worker's clock when it sent this.
    pub sent_ms: i64,
    /// When the last acknowledgement arrived, for the coordinator's
    /// clock-offset estimate.
    pub last_ack: Option<AckEcho>,
    /// Leases the worker believes it holds.
    pub leases: u32,
    pub stats: Stats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckEcho {
    pub seq: u64,
    /// The worker's clock when the acknowledgement arrived.
    pub received_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub claimed: u64,
    pub fetched: u64,
    pub processed: u64,
    pub cpu_permille: u16,
    /// Resident memory, in MiB.
    pub rss_mb: u32,
    pub spool_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatAck {
    pub seq: u64,
    /// The coordinator's clock when it sent this.
    pub coordinator_ms: i64,
    /// The coordinator's estimate of the worker's clock minus its own.
    pub offset_ms: i64,
    /// More than 60 s off (§19.3): fix NTP.
    pub skewed: bool,
    /// Leases the coordinator has on record for the worker.
    pub leases: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimBatch {
    /// Chosen by the worker, never 0; the reply carries it back.
    pub batch: u64,
    pub items: Vec<ClaimItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimItem {
    pub key: [u8; 20],
    pub source: u8,
    pub strategy: u8,
    /// The worker's clock when it found the key; the coordinator corrects
    /// it by the worker's measured offset.
    pub discovered_ms: i64,
    /// Peers that announced the key, at most [`MAX_HINTS`]. If another
    /// worker holds the key they are relayed to it in memory and never
    /// stored (§19.3, §18.7).
    pub hints: Vec<SocketAddr>,
}

/// Peer hints for a key the receiving worker holds, found by another worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HintItem {
    pub key: [u8; 20],
    /// The holder's fencing token, so a hint for a lease that has since
    /// moved is dropped.
    pub token: u64,
    pub hints: Vec<SocketAddr>,
}

/// An opaque payload with the worker's batch id (never 0), acknowledged by
/// [`ToWorker::Accepted`]; replaying a batch is idempotent (§19.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    pub batch: u64,
    pub bytes: Vec<u8>,
}

/// What a snapshot carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SnapshotKind {
    /// What the commit judges with (WK-8).
    Filter,
    /// The file-tree zstd dictionary (WK-8).
    Dictionary,
    /// The have-filter (WK-3), if it travels in chunks rather than in
    /// `HaveSnapshot`.
    HaveFilter,
}

/// One chunk of a versioned snapshot. Chunk `seq` of `total`, in order;
/// the worker acknowledges each with [`ToCoordinator::SnapshotAck`] and the
/// coordinator sends the next only after it, so a snapshot never fills a
/// queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotChunk {
    pub kind: SnapshotKind,
    pub version: u64,
    pub seq: u32,
    pub total: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotAck {
    pub kind: SnapshotKind,
    pub version: u64,
    pub seq: u32,
}

/// Fetch these keys again and report their derived fields (§19.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverifyRequest {
    /// Chosen by the coordinator, never 0; the reply carries it back.
    pub id: u64,
    pub keys: Vec<[u8; 20]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverifyReply {
    pub id: u64,
    /// The derived fields per key, in WK-13's encoding.
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantBatch {
    /// The claim this answers, or 0 for keys whose lease expired elsewhere
    /// and are granted to this worker now.
    pub batch: u64,
    pub items: Vec<GrantItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantItem {
    Granted {
        key: [u8; 20],
        token: u64,
        ttl_secs: u32,
    },
    Denied {
        key: [u8; 20],
        reason: DenyReason,
    },
}

impl GrantItem {
    pub fn key(&self) -> [u8; 20] {
        match self {
            GrantItem::Granted { key, .. } | GrantItem::Denied { key, .. } => *key,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DenyReason {
    /// Already in the store.
    Stored,
    /// Another worker holds it; the claim's source was merged into its
    /// lease.
    HeldElsewhere,
    /// The worker is draining and gets no new leases.
    Draining,
    /// Not now: claim again later. The coordinator is behind (its
    /// writer's queue is full, §19.5), or its store check couldn't answer:
    /// for the whole batch when the check died or came back short (#607),
    /// and for one key when that key's read failed (#628).
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeBatch {
    pub items: Vec<OutcomeItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeItem {
    pub key: [u8; 20],
    /// The lease's fencing token. A stale one is ignored.
    pub token: u64,
    pub outcome: Outcome,
}

/// A leased key's terminal funnel outcome (§19.3). Each ends the lease;
/// `PeerFound` and `Lost` are losses, and their `reason` is the funnel's
/// loss code (`lodestar_sources::Loss as u8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// The metadata arrived; the torrent travels in `Results`.
    Fetched,
    /// Peers were found, but the metadata wasn't fetched.
    PeerFound {
        reason: u8,
    },
    Lost {
        reason: u8,
    },
}

/// One lease a worker still holds: its key and fencing token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HeldLease {
    pub key: [u8; 20],
    pub token: u64,
}

/// One part of the list of leases a worker still holds (WK-11b), sent
/// after a spool replay when its lease count and the coordinator's differ.
/// Parts `0..` of one `round` go in order on one session; the last has
/// `last` set. On the last, the coordinator returns for re-grant the
/// leases it has on record for the worker that no part listed, delivered
/// on an earlier session, and answers with [`ToWorker::Reconciled`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    /// Chosen by the worker, never 0; the reply carries it back.
    pub round: u64,
    pub part: u32,
    pub last: bool,
    pub leases: HeldList,
}

/// At most [`MAX_HOLDING_BATCH`] leases. The count is checked against the
/// cap when decoding starts, before anything is allocated.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct HeldList(pub Vec<HeldLease>);

impl<'de> Deserialize<'de> for HeldList {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<HeldList, D::Error> {
        struct Bounded;
        impl<'de> serde::de::Visitor<'de> for Bounded {
            type Value = HeldList;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "at most {MAX_HOLDING_BATCH} leases")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<HeldList, A::Error> {
                // postcard states the length up front: refused before any
                // allocation if it's over the cap.
                let hint = seq.size_hint().unwrap_or(0);
                if hint > MAX_HOLDING_BATCH {
                    return Err(serde::de::Error::invalid_length(hint, &self));
                }
                let mut out = Vec::with_capacity(hint);
                while let Some(l) = seq.next_element::<HeldLease>()? {
                    if out.len() == MAX_HOLDING_BATCH {
                        return Err(serde::de::Error::invalid_length(out.len() + 1, &self));
                    }
                    out.push(l);
                }
                Ok(HeldList(out))
            }
        }
        d.deserialize_seq(Bounded)
    }
}

/// The coordinator's answer to the last part of a [`Holding`] round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconciled {
    pub round: u64,
    pub answer: ReconcileAnswer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReconcileAnswer {
    /// `returned`: leases on record for the worker, not listed, given back
    /// for re-grant. `unknown`: listed leases the coordinator doesn't have
    /// on record for the worker (counted, never created).
    /// `leases`: what it has on record for the worker now.
    Done {
        returned: u32,
        unknown: u32,
        leases: u32,
    },
    /// Nothing changed: the worker is paused (quarantined) or no longer
    /// current, or the parts came out of order.
    Refused,
}

/// A renewed certificate, as [`JoinGrant`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGrant {
    pub worker: u32,
    pub name: String,
    pub cert: Vec<u8>,
    pub not_after: i64,
    pub cas: Vec<Vec<u8>>,
}

impl From<JoinGrant> for WireGrant {
    fn from(g: JoinGrant) -> WireGrant {
        WireGrant {
            worker: g.worker.0,
            name: g.name,
            cert: g.cert.to_vec(),
            not_after: g.not_after,
            cas: g.cas.into_iter().map(|c| c.to_vec()).collect(),
        }
    }
}

impl From<WireGrant> for JoinGrant {
    fn from(g: WireGrant) -> JoinGrant {
        JoinGrant {
            worker: crate::WorkerId(g.worker),
            name: g.name,
            cert: g.cert.into(),
            not_after: g.not_after,
            cas: g.cas.into_iter().map(Into::into).collect(),
        }
    }
}

impl WireGrant {
    fn valid(&self) -> bool {
        crate::valid_worker_name(&self.name)
            && self.cert.len() <= MAX_CERT
            && !self.cas.is_empty()
            && self.cas.len() <= MAX_CAS
            && self.cas.iter().all(|c| c.len() <= MAX_CERT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenewRefusal {
    /// The last certificate is less than an hour old.
    TooSoon,
    Revoked,
    Expired,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// The certificate was revoked; join again.
    Revoked,
    /// The major versions differ; update.
    VersionMismatch {
        coordinator_major: u16,
    },
    /// `Hello` named another worker than the certificate.
    IdentityMismatch,
    /// A message out of order or over a limit.
    Protocol,
    HeartbeatTimeout,
    Drained,
    /// The same worker connected again; this connection is the older one.
    Replaced,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToCoordinator {
    Hello(Hello),
    Heartbeat(Heartbeat),
    Claim(ClaimBatch),
    Outcome(OutcomeBatch),
    Renew {
        public_key: [u8; 32],
    },
    Drained,
    /// Processed torrents (§19.4), WK-6's encoding.
    Results(Payload),
    /// Rollup deltas, WK-6's encoding.
    Counters(Payload),
    SnapshotAck(SnapshotAck),
    Reverified(ReverifyReply),
    /// Minor 1: sent only to a coordinator whose `Welcome.minor` is at
    /// least [`MINOR_HOLDING`]. Last, so older variants keep their tags.
    Holding(Holding),
}

/// `Debug` is written out below: the opaque payloads show only their
/// length, so the have-filter key in a `HaveSnapshot` header can't reach
/// a log (WK-3).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToWorker {
    Welcome(Welcome),
    Assign(Assign),
    HeartbeatAck(HeartbeatAck),
    Grants(GrantBatch),
    Renewed(Box<WireGrant>),
    RenewRefused(RenewRefusal),
    Drain,
    Stop(StopReason),
    /// The replicated have-filter (§19.3), WK-3's encoding.
    HaveSnapshot(Vec<u8>),
    HaveDelta(Vec<u8>),
    /// A signed update offer (§19.6), later.
    Update(Vec<u8>),
    Hints(Vec<HintItem>),
    /// A `Results` or `Counters` batch: taken (`true`), or refused for now
    /// because the coordinator is behind (send it again later).
    Accepted {
        batch: u64,
        accepted: bool,
    },
    Snapshot(SnapshotChunk),
    Reverify(ReverifyRequest),
    /// Minor 1: only ever an answer to [`ToCoordinator::Holding`], so an
    /// older worker never gets one.
    Reconciled(Reconciled),
}

impl std::fmt::Debug for ToWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes = |name: &str, b: &[u8], f: &mut std::fmt::Formatter<'_>| {
            write!(f, "{name}({} bytes)", b.len())
        };
        match self {
            ToWorker::Welcome(w) => f.debug_tuple("Welcome").field(w).finish(),
            ToWorker::Assign(a) => f
                .debug_struct("Assign")
                .field("revision", &a.revision)
                .field("payload_bytes", &a.payload.len())
                .finish(),
            ToWorker::HeartbeatAck(h) => f.debug_tuple("HeartbeatAck").field(h).finish(),
            ToWorker::Grants(g) => f.debug_tuple("Grants").field(g).finish(),
            ToWorker::Renewed(g) => f.debug_tuple("Renewed").field(g).finish(),
            ToWorker::RenewRefused(r) => f.debug_tuple("RenewRefused").field(r).finish(),
            ToWorker::Drain => f.write_str("Drain"),
            ToWorker::Stop(r) => f.debug_tuple("Stop").field(r).finish(),
            ToWorker::HaveSnapshot(b) => bytes("HaveSnapshot", b, f),
            ToWorker::HaveDelta(b) => bytes("HaveDelta", b, f),
            ToWorker::Update(b) => bytes("Update", b, f),
            ToWorker::Hints(h) => f.debug_tuple("Hints").field(h).finish(),
            ToWorker::Accepted { batch, accepted } => f
                .debug_struct("Accepted")
                .field("batch", batch)
                .field("accepted", accepted)
                .finish(),
            ToWorker::Snapshot(c) => f
                .debug_struct("Snapshot")
                .field("kind", &c.kind)
                .field("version", &c.version)
                .field("seq", &c.seq)
                .field("total", &c.total)
                .field("bytes", &c.bytes.len())
                .finish(),
            ToWorker::Reverify(r) => f.debug_tuple("Reverify").field(r).finish(),
            ToWorker::Reconciled(r) => f.debug_tuple("Reconciled").field(r).finish(),
        }
    }
}

fn hints_valid(hints: &[SocketAddr]) -> bool {
    hints.len() <= MAX_HINTS
}

impl Payload {
    fn valid(&self) -> bool {
        self.batch != 0 && self.bytes.len() <= MAX_OPAQUE
    }
}

impl SnapshotChunk {
    fn valid(&self) -> bool {
        self.total != 0
            && self.total <= MAX_CHUNKS
            && self.seq < self.total
            && self.bytes.len() <= MAX_CHUNK
    }
}

impl ToCoordinator {
    /// Whether every list and payload is within its limit.
    pub fn validate(&self) -> bool {
        match self {
            ToCoordinator::Hello(h) => h.capabilities.external_ips.len() <= MAX_EXTERNAL_IPS,
            ToCoordinator::Heartbeat(_) | ToCoordinator::Renew { .. } | ToCoordinator::Drained => {
                true
            }
            ToCoordinator::Claim(c) => {
                c.batch != 0
                    && !c.items.is_empty()
                    && c.items.len() <= MAX_CLAIM_BATCH
                    && c.items.iter().all(|i| hints_valid(&i.hints))
            }
            ToCoordinator::Outcome(o) => !o.items.is_empty() && o.items.len() <= MAX_OUTCOME_BATCH,
            ToCoordinator::Results(p) | ToCoordinator::Counters(p) => p.valid(),
            ToCoordinator::SnapshotAck(a) => a.seq < MAX_CHUNKS,
            ToCoordinator::Reverified(r) => r.id != 0 && r.payload.len() <= MAX_OPAQUE,
            ToCoordinator::Holding(h) => {
                h.round != 0 && h.part < MAX_HOLDING_PARTS && h.leases.0.len() <= MAX_HOLDING_BATCH
            }
        }
    }

    /// An upper bound on the message's encoded size (tested against
    /// postcard), for byte-capped queues: a budget, not exact memory.
    pub fn approx_size(&self) -> usize {
        const BASE: usize = 64;
        BASE + match self {
            ToCoordinator::Hello(h) => h.capabilities.external_ips.len() * 17,
            ToCoordinator::Heartbeat(_) | ToCoordinator::Renew { .. } | ToCoordinator::Drained => 0,
            ToCoordinator::Claim(c) => c
                .items
                .iter()
                .map(|i| 48 + i.hints.len() * 32)
                .sum::<usize>(),
            ToCoordinator::Outcome(o) => o.items.len() * 40,
            ToCoordinator::Results(p) | ToCoordinator::Counters(p) => p.bytes.len(),
            ToCoordinator::SnapshotAck(_) => 0,
            ToCoordinator::Reverified(r) => r.payload.len(),
            ToCoordinator::Holding(h) => h.leases.0.len() * 32,
        }
    }
}

impl ToWorker {
    pub fn validate(&self) -> bool {
        match self {
            ToWorker::Grants(g) => g.items.len() <= MAX_GRANT_BATCH,
            ToWorker::Hints(h) => {
                !h.is_empty()
                    && h.len() <= MAX_HINT_BATCH
                    && h.iter()
                        .all(|i| !i.hints.is_empty() && hints_valid(&i.hints))
            }
            ToWorker::Accepted { batch, .. } => *batch != 0,
            ToWorker::Snapshot(c) => c.valid(),
            ToWorker::Reverify(r) => {
                r.id != 0 && !r.keys.is_empty() && r.keys.len() <= MAX_REVERIFY
            }
            ToWorker::Renewed(g) => g.valid(),
            ToWorker::Assign(a) => a.payload.len() <= MAX_OPAQUE,
            ToWorker::HaveSnapshot(p) | ToWorker::HaveDelta(p) | ToWorker::Update(p) => {
                p.len() <= MAX_OPAQUE
            }
            ToWorker::Welcome(_)
            | ToWorker::HeartbeatAck(_)
            | ToWorker::RenewRefused(_)
            | ToWorker::Drain
            | ToWorker::Stop(_)
            | ToWorker::Reconciled(_) => true,
        }
    }

    /// An upper bound on the message's encoded size (tested against
    /// postcard), for byte-capped queues: a budget, not exact memory.
    pub fn approx_size(&self) -> usize {
        const BASE: usize = 64;
        BASE + match self {
            ToWorker::Grants(g) => g.items.len() * 40,
            ToWorker::Renewed(g) => {
                g.name.len() + g.cert.len() + g.cas.iter().map(|c| c.len() + 8).sum::<usize>()
            }
            ToWorker::Assign(a) => a.payload.len(),
            ToWorker::HaveSnapshot(p) | ToWorker::HaveDelta(p) | ToWorker::Update(p) => p.len(),
            ToWorker::Hints(h) => h.iter().map(|i| 40 + i.hints.len() * 32).sum::<usize>(),
            ToWorker::Snapshot(c) => c.bytes.len(),
            ToWorker::Reverify(r) => r.keys.len() * 20,
            ToWorker::Welcome(_)
            | ToWorker::HeartbeatAck(_)
            | ToWorker::RenewRefused(_)
            | ToWorker::Drain
            | ToWorker::Stop(_)
            | ToWorker::Accepted { .. }
            | ToWorker::Reconciled(_) => 0,
        }
    }
}

/// The one message on a join connection (ALPN `lodestar-join/1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub major: u16,
    /// The token as typed, `lsj1…`.
    pub token: String,
    pub public_key: [u8; 32],
}

/// The answer. Every refusal looks the same (cluster1: a guess learns
/// nothing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinReply {
    Granted(Box<WireGrant>),
    Refused,
}

impl JoinReply {
    pub fn validate(&self) -> bool {
        match self {
            JoinReply::Granted(g) => g.valid(),
            JoinReply::Refused => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{self, FrameError, MAX_FRAME, decode_to_coordinator};

    fn holding(n: usize, last: bool) -> ToCoordinator {
        ToCoordinator::Holding(Holding {
            round: 3,
            part: 0,
            last,
            leases: HeldList(
                (0..n)
                    .map(|i| HeldLease {
                        key: [(i % 251) as u8; 20],
                        token: u64::MAX - i as u64,
                    })
                    .collect(),
            ),
        })
    }

    /// A full part fits a frame however it compresses, and its size
    /// budget holds against postcard.
    #[test]
    fn a_full_holding_part_fits_a_frame() {
        let msg = holding(MAX_HOLDING_BATCH, true);
        let plain = postcard::to_allocvec(&msg).unwrap();
        assert!(plain.len() < MAX_FRAME, "{}", plain.len());
        assert!(plain.len() <= msg.approx_size(), "{}", plain.len());
        let frame = frame::encode(&msg).unwrap();
        assert_eq!(decode_to_coordinator(&frame).unwrap(), msg);
    }

    /// Over the cap is refused when decoding starts, from the stated
    /// length, before anything is allocated; round 0 and a part number
    /// past the cap fail validation.
    #[test]
    fn a_holding_part_over_its_caps_is_refused() {
        let over = postcard::to_allocvec(&holding(MAX_HOLDING_BATCH + 1, true)).unwrap();
        assert!(postcard::from_bytes::<ToCoordinator>(&over).is_err());
        // Only the length prefix of a huge list: refused without its body.
        // Only a length prefix near u64::MAX, no body: postcard itself
        // gives no size hint for a length longer than the bytes left, so
        // this is refused at the end of the input.
        let mut lie = postcard::to_allocvec(&holding(0, true)).unwrap();
        lie.pop();
        lie.extend_from_slice(&[0xff; 9]);
        lie.push(0x01);
        assert!(postcard::from_bytes::<ToCoordinator>(&lie).is_err());
        let encode = |m: &ToCoordinator| frame::encode(m).unwrap();
        let mut msg = holding(1, false);
        if let ToCoordinator::Holding(h) = &mut msg {
            h.round = 0;
        }
        assert_eq!(
            decode_to_coordinator(&encode(&msg)),
            Err(FrameError::Invalid)
        );
        if let ToCoordinator::Holding(h) = &mut msg {
            h.round = 1;
            h.part = MAX_HOLDING_PARTS;
        }
        assert_eq!(
            decode_to_coordinator(&encode(&msg)),
            Err(FrameError::Invalid)
        );
        // An empty list is a real answer: the worker holds nothing.
        assert!(decode_to_coordinator(&encode(&holding(0, true))).is_ok());
    }

    /// The cap is checked against the stated length before allocating,
    /// whatever the decoder: a sequence that claims `usize::MAX` elements
    /// is refused at once (without the check, `with_capacity` would be
    /// asked for that and abort).
    #[test]
    fn a_huge_stated_length_is_refused_before_allocating() {
        use serde::de::value::{Error, SeqAccessDeserializer};
        struct Claims(usize);
        impl<'de> serde::de::SeqAccess<'de> for Claims {
            type Error = Error;
            fn next_element_seed<T: serde::de::DeserializeSeed<'de>>(
                &mut self,
                _: T,
            ) -> Result<Option<T::Value>, Error> {
                Ok(None)
            }
            fn size_hint(&self) -> Option<usize> {
                Some(self.0)
            }
        }
        let huge = HeldList::deserialize(SeqAccessDeserializer::new(Claims(usize::MAX)));
        assert!(huge.is_err());
        let over = HeldList::deserialize(SeqAccessDeserializer::new(Claims(MAX_HOLDING_BATCH + 1)));
        assert!(over.is_err());
        let fine = HeldList::deserialize(SeqAccessDeserializer::new(Claims(MAX_HOLDING_BATCH)));
        assert_eq!(fine.unwrap(), HeldList::default());
    }

    /// The new messages are last, so every older message keeps its tag
    /// and an older peer decodes what it knows unchanged.
    #[test]
    fn older_messages_keep_their_tags() {
        let tag = |m: &ToCoordinator| postcard::to_allocvec(m).unwrap()[0];
        assert_eq!(
            tag(&ToCoordinator::Hello(Hello::new(
                1,
                Capabilities::default()
            ))),
            0
        );
        assert_eq!(
            tag(&ToCoordinator::Reverified(ReverifyReply {
                id: 1,
                payload: vec![]
            })),
            9
        );
        assert_eq!(tag(&holding(0, true)), 10);
        let reply = ToWorker::Reconciled(Reconciled {
            round: 1,
            answer: ReconcileAnswer::Refused,
        });
        assert_eq!(postcard::to_allocvec(&reply).unwrap()[0], 15);
        assert_eq!(
            postcard::to_allocvec(&ToWorker::Reverify(ReverifyRequest {
                id: 1,
                keys: vec![[0; 20]]
            }))
            .unwrap()[0],
            14
        );
    }
}
