//! `/workers` (architecture §19.6, §19.7): the remote workers Settings →
//! Workers shows, the one-time join token and the admin's actions.
//!
//! **What never appears here:** a join token outside
//! [`WorkerTokenCreated`] (the one response that carries it), a peer
//! address (§18.7), certificate material, or a credential. A worker's
//! external IP is its own egress as it reported it (§19.7 shows it), not a
//! peer's.
//!
//! Fields a later slice fills (version, uptime, egress, assigned shards,
//! per-minute throughput, quarantine) are left out rather than faked.

use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::REDACTED;

/// A worker's state as Settings → Workers shows it (icon + text).
/// `quarantined` arrives with WK-13.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WorkerStatus {
    /// Connected and taking work.
    Online,
    /// Finishing the leases it holds, then it stops (§19.6).
    Draining,
    /// Connected, taking no new work until resumed.
    Paused,
    /// Joined, not connected now.
    Offline,
}

/// Whether a worker may run the DHT (§19.5 placement pins). `off` keeps
/// fetch and prepare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DhtPin {
    Auto,
    Off,
}

/// A worker's crawl intensity (§19.5: each worker has its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WorkerIntensity {
    Low,
    Balanced,
    Aggressive,
}

/// A worker's resource budget. 0 means no cap beyond the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkerBudget {
    /// CPU share in ‰, 1–1000.
    pub cpu_permille: u16,
    /// Download cap in kB/s.
    pub down_kbps: u32,
    /// Memory cap in MB.
    pub ram_mb: u32,
    /// Upload cap in kB/s.
    pub up_kbps: u32,
}

/// One worker. Live values are null while it has not connected since the
/// coordinator started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Worker {
    pub budget: WorkerBudget,
    /// Keys claimed, as the worker last reported (its counter).
    #[schema(required = true)]
    pub claimed: Option<u64>,
    /// The worker's clock minus the coordinator's, in ms.
    #[schema(required = true)]
    pub clock_offset_ms: Option<i64>,
    /// More than 60 s off (§19.3): fix NTP on the worker.
    pub clock_skewed: bool,
    /// CPU use in ‰, as last reported.
    #[schema(required = true)]
    pub cpu_permille: Option<u16>,
    pub dht: DhtPin,
    /// The public addresses the worker reported for itself (its egress),
    /// at most 8. Never a peer's.
    pub external_ips: Vec<String>,
    /// Metadata fetched, as the worker last reported.
    #[schema(required = true)]
    pub fetched: Option<u64>,
    /// The worker's id in this cluster.
    pub id: u32,
    pub intensity: WorkerIntensity,
    /// Unix seconds of its last message; null if not seen since start.
    #[schema(required = true)]
    pub last_seen_at: Option<i64>,
    /// Leases it holds now.
    pub leases: u64,
    /// The name it joined with (its certificate's); null when the
    /// coordinator has no certificate on record for it.
    #[schema(required = true)]
    pub name: Option<String>,
    /// Paused by the admin: it gets no new work until resumed, and stays
    /// paused across reconnects and restarts. Set while offline too, when
    /// `status` can only say `offline`.
    pub paused: bool,
    /// Fetch-pool cap in keys, 1–1,000,000.
    pub pool_cap: u32,
    /// Torrents processed, as the worker last reported.
    #[schema(required = true)]
    pub processed: Option<u64>,
    /// Resident memory in MB, as last reported.
    #[schema(required = true)]
    pub rss_mb: Option<u32>,
    /// Bytes waiting in its on-disk spool (§19.6).
    #[schema(required = true)]
    pub spool_bytes: Option<u64>,
    pub status: WorkerStatus,
}

/// `GET /workers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkerList {
    /// "Allow workers" is on and the cluster port is serving.
    pub allowed: bool,
    /// Every joined worker, by id. Empty on a single machine.
    pub workers: Vec<Worker>,
}

/// `POST /workers/tokens`. `Debug` redacts the password.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkerToken {
    /// The new worker's name: 1–63 lowercase letters, digits and hyphens,
    /// not starting or ending with a hyphen. It goes in its certificate.
    pub name: String,
    /// The admin password: a join token admits a machine to the cluster,
    /// so it's confirmed like an API key (428 `confirm_required` without
    /// it, 401 `bad_password` when wrong).
    #[serde(default)]
    #[schema(nullable = false)]
    pub password: Option<String>,
}

impl fmt::Debug for CreateWorkerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateWorkerToken")
            .field("name", &self.name)
            .field("password", &REDACTED)
            .finish()
    }
}

/// Ready-to-copy ways to join with the token (§19.6). Most hold the
/// token, so they're shown once with it. `Debug` redacts every one.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JoinCommands {
    /// `lodestar worker`, given the token on standard input. The token is
    /// in the line, so it lands in the shell's history; it's single use
    /// and expires in 15 minutes. `cli_read` keeps it out.
    pub cli: String,
    /// The same, reading the token from a hidden prompt (`read -rs`):
    /// paste the token when asked. Nothing secret is in the line.
    pub cli_read: String,
    /// A compose service.
    pub compose: String,
    /// A `docker run` line. Remove `LODESTAR_TOKEN` once the worker has
    /// joined (it's spent; the worker warns about it at each start).
    pub docker_run: String,
    /// What to enter in the Unraid "Lodestar Worker" template.
    pub unraid: String,
    /// What to enter in the Windows installer's "Join an existing
    /// Lodestar".
    pub windows: String,
}

impl fmt::Debug for JoinCommands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinCommands").finish_non_exhaustive()
    }
}

/// The one response that carries a join token. It can't be read back:
/// only its hash is kept. `Debug` redacts the token and the commands.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkerTokenCreated {
    /// The cluster address workers dial, `https://host:port`.
    pub address: String,
    pub commands: JoinCommands,
    /// Unix seconds; 15 minutes from now.
    pub expires_at: i64,
    pub name: String,
    /// The `lsj1…` token: single use, for this name only.
    pub token: String,
}

impl fmt::Debug for WorkerTokenCreated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkerTokenCreated")
            .field("address", &self.address)
            .field("commands", &self.commands)
            .field("expires_at", &self.expires_at)
            .field("name", &self.name)
            .field("token", &REDACTED)
            .finish()
    }
}

/// `POST /workers/{id}/revoke`: what revoking did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkerRevoked {
    /// Certificates invalidated now.
    pub certificates: u64,
    /// Seconds until its connection is closed, at most: one heartbeat.
    pub gone_within_secs: u32,
    pub id: u32,
    /// Leases taken back and offered to other workers.
    pub leases_returned: u64,
    #[schema(required = true)]
    pub name: Option<String>,
}

/// `PATCH /workers/{id}`: any of these; absent fields are kept. `budget`
/// is sent whole, all four fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub budget: Option<WorkerBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub dht: Option<DhtPin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub intensity: Option<WorkerIntensity>,
    /// 1–1,000,000 keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pool_cap: Option<u32>,
}
