//! The cluster protocol's messages (architecture §19.2), as they travel in
//! [`frame`](crate::frame)s.
//!
//! | From → to | Message | Here |
//! |---|---|---|
//! | W → C | [`ToCoordinator::Hello`] | protocol version, worker id, capabilities |
//! | C → W | [`ToWorker::Welcome`] | the accepted version and the session's timings |
//! | C → W | [`ToWorker::Assign`] | shape only: a revision and an opaque payload |
//! | C ↔ W | [`ToCoordinator::Heartbeat`] / [`ToWorker::HeartbeatAck`] | every 5 s: stats, clock offset, lease renewal |
//! | W → C | [`ToCoordinator::Claim`] | up to [`MAX_CLAIM_BATCH`] new keys |
//! | C → W | [`ToWorker::Grants`] | per key a grant (fencing token, TTL) or a denial; batch 0 carries re-grants |
//! | W → C | [`ToCoordinator::Outcome`] | terminal outcomes; stale tokens are ignored |
//! | W → C | [`ToCoordinator::Renew`] / C → W [`ToWorker::Renewed`] | certificate renewal, rate-limited |
//! | C → W | [`ToWorker::Drain`] / [`ToWorker::Stop`] | graceful drain; a stop with its reason |
//! | W → C | [`ToCoordinator::Drained`] | the drain finished |
//!
//! `Results`, `Counters`, `HaveSnapshot`, `HaveDelta` and `Update` exist
//! with opaque payloads so the enum layout is fixed; nothing sends or acts
//! on them yet (V6-cluster3 and cluster5).
//!
//! Every limit a message can break is checked by `validate`, on both
//! transports, so the in-process worker goes through the same checks.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::JoinGrant;

/// A worker refuses a coordinator, and the coordinator a worker, with
/// another major version.
pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 0;

/// Keys per claim (§19.2: "≤ 512 or every 250 ms").
pub const MAX_CLAIM_BATCH: usize = 512;
/// Outcomes per report, and grants per reply.
pub const MAX_OUTCOME_BATCH: usize = 1024;
pub const MAX_GRANT_BATCH: usize = 1024;
pub const MAX_EXTERNAL_IPS: usize = 8;
/// Opaque payloads of the messages later slices fill in.
pub const MAX_OPAQUE: usize = 512 * 1024;
/// CA certificates in a renewal grant (current and previous).
pub const MAX_CAS: usize = 4;
pub const MAX_CERT: usize = 4096;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimItem {
    pub key: [u8; 20],
    pub source: u8,
    pub strategy: u8,
    /// The worker's clock when it found the key; the coordinator corrects
    /// it by the worker's measured offset.
    pub discovered_ms: i64,
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

/// A leased key's terminal funnel outcome (§19.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Fetched,
    /// Peers were found, but the metadata wasn't fetched.
    PeerFound {
        reason: u8,
    },
    Lost {
        reason: u8,
    },
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
    /// V6-cluster5: processed torrents (§19.4).
    Results(Vec<u8>),
    /// V6-cluster5: rollup deltas.
    Counters(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToWorker {
    Welcome(Welcome),
    Assign(Assign),
    HeartbeatAck(HeartbeatAck),
    Grants(GrantBatch),
    Renewed(Box<WireGrant>),
    RenewRefused(RenewRefusal),
    Drain,
    Stop(StopReason),
    /// V6-cluster3: the replicated have-filter (§19.3).
    HaveSnapshot(Vec<u8>),
    HaveDelta(Vec<u8>),
    /// A signed update offer (§19.6), later.
    Update(Vec<u8>),
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
                c.batch != 0 && !c.items.is_empty() && c.items.len() <= MAX_CLAIM_BATCH
            }
            ToCoordinator::Outcome(o) => !o.items.is_empty() && o.items.len() <= MAX_OUTCOME_BATCH,
            ToCoordinator::Results(p) | ToCoordinator::Counters(p) => p.len() <= MAX_OPAQUE,
        }
    }
}

impl ToWorker {
    pub fn validate(&self) -> bool {
        match self {
            ToWorker::Grants(g) => g.items.len() <= MAX_GRANT_BATCH,
            ToWorker::Renewed(g) => g.valid(),
            ToWorker::Assign(a) => a.payload.len() <= MAX_OPAQUE,
            ToWorker::HaveSnapshot(p) | ToWorker::HaveDelta(p) | ToWorker::Update(p) => {
                p.len() <= MAX_OPAQUE
            }
            ToWorker::Welcome(_)
            | ToWorker::HeartbeatAck(_)
            | ToWorker::RenewRefused(_)
            | ToWorker::Drain
            | ToWorker::Stop(_) => true,
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
