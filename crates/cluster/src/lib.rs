//! The coordinator–worker boundary (architecture §19).
//!
//! One coordinator owns the store, the index and the UI; workers find,
//! fetch and prepare torrents. M3 builds the boundary with one built-in
//! worker in-process ([`WorkerId::LOCAL`]), so all processing already runs
//! on the worker side and single-machine behaviour is unchanged. Remote
//! workers (v0.6) add the network protocol, joining and the replicated
//! have-filter on top of the same pieces.
//!
//! This crate holds the parts that don't depend on how the two sides talk:
//! - the fleet-wide [`LeaseTable`] with its fencing tokens;
//! - the cluster PKI (§19.6): the [`Coordinator`]'s CA with rotation, its
//!   issued and revoked certificates and one-time [`JoinToken`]s, and the
//!   worker's [`Joining`] and [`Credentials`];
//! - mutual TLS 1.3 for the cluster port ([`CoordinatorTls`] and the
//!   worker's client configurations), pinned to the cluster CA;
//! - the protocol (§19.2): [`proto`] messages in bounded [`frame`]s over a
//!   [`Link`] (a WebSocket, or in-process channels for the local worker),
//!   the coordinator's [`ClusterServer`] (pre-auth limits, sessions,
//!   heartbeats, clock offsets, leases and re-grants, revocation) and the
//!   worker's [`WorkerSession`].

mod client;
mod clock;
mod coordinator;
pub mod frame;
mod hex;
mod join;
mod leases;
mod link;
pub mod proto;
mod server;
mod tls;
mod worker;
mod x509;

#[cfg(test)]
mod tests;

use std::fmt;

pub use client::{ClientError, WorkerConfig, WorkerEvent, WorkerSession, connect, join};
pub use clock::{Clock, ClockTime, ManualClock, OffsetClock, SystemClock};
pub use coordinator::{
    CA_LIFETIME_SECS, Coordinator, IssuedCert, JOIN_TOKEN_TTL_SECS, JoinError, JoinGrant,
    LEAF_LIFETIME_SECS, MAX_OPEN_TOKENS, RENEW_MIN_INTERVAL_SECS, ROTATION_OVERLAP_SECS,
};
pub use join::{
    JoinToken, TokenFormatError, TokenInputError, read_join_token, read_join_token_file,
    valid_worker_name,
};
pub use leases::{Claim, Claimed, Lease, LeaseTable, RestoredLease, Token, WorkerId};
pub use link::{CoordinatorLink, Link, LinkError, WorkerLink, local_pair};
pub use server::{
    CLUSTER_PATH, ClusterServer, FleetSnapshot, Pki, ServerConfig, StoredKeys, WorkerStatus,
};
pub use tls::{ALPN_CLUSTER, ALPN_JOIN, CoordinatorTls, PeerIdentity, coordinator_server_name};
pub use worker::{Credentials, Joining, Renewal};
pub use x509::{CLOCK_SKEW_SECS, COORDINATOR_NAME, Fingerprint, KeyPair};

/// Why a cluster PKI operation failed. Its text never carries key
/// material or token secrets.
#[derive(Debug)]
pub enum ClusterError {
    Store(lodestar_store::Error),
    Secret(lodestar_integrations::SecretError),
    Tls(rustls::Error),
    /// A TLS verifier couldn't be built from the CA bundle.
    Verifier(String),
    Io(&'static str, std::io::ErrorKind),
    /// A stored record or file isn't what this version wrote.
    Corrupt(&'static str),
    /// The system's random source failed.
    Random,
    /// Worker names are one DNS label: `a-z`, `0-9` and `-`.
    InvalidName,
    /// A live worker already has the name.
    NameInUse,
    TooManyTokens,
    /// The certificate was revoked.
    Revoked,
    /// The certificate has expired: the worker must join again.
    Expired,
    /// A rotation is already under way.
    RotationInProgress,
    /// A join or renewal answer that doesn't match what was asked.
    BadGrant,
    /// The worker renewed less than an hour ago.
    TooSoon,
}

impl fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClusterError::Store(e) => write!(f, "cluster storage: {e}"),
            ClusterError::Secret(e) => write!(f, "cluster keys: {e}"),
            ClusterError::Tls(e) => write!(f, "cluster TLS: {e}"),
            ClusterError::Verifier(e) => write!(f, "cluster TLS setup: {e}"),
            ClusterError::Io(op, kind) => write!(f, "couldn't {op}: {kind}"),
            ClusterError::Corrupt(what) => write!(f, "corrupt cluster data ({what})"),
            ClusterError::Random => f.write_str("the system random source failed"),
            ClusterError::InvalidName => f.write_str(
                "a worker name is 1-63 lowercase letters, digits or hyphens, \
                 not starting or ending with a hyphen",
            ),
            ClusterError::NameInUse => f.write_str("a worker with this name is already joined"),
            ClusterError::TooManyTokens => write!(
                f,
                "{MAX_OPEN_TOKENS} join tokens are already open; wait for them to expire"
            ),
            ClusterError::Revoked => f.write_str("this worker's certificate was revoked"),
            ClusterError::Expired => f.write_str("this worker's certificate has expired"),
            ClusterError::RotationInProgress => f.write_str("a CA rotation is already in progress"),
            ClusterError::BadGrant => f.write_str("the coordinator's answer doesn't match"),
            ClusterError::TooSoon => f.write_str("this worker renewed less than an hour ago"),
        }
    }
}

impl std::error::Error for ClusterError {}

impl From<lodestar_store::Error> for ClusterError {
    fn from(e: lodestar_store::Error) -> Self {
        ClusterError::Store(e)
    }
}

impl From<lodestar_store::rusqlite::Error> for ClusterError {
    fn from(e: lodestar_store::rusqlite::Error) -> Self {
        ClusterError::Store(e.into())
    }
}

impl From<lodestar_integrations::SecretError> for ClusterError {
    fn from(e: lodestar_integrations::SecretError) -> Self {
        ClusterError::Secret(e)
    }
}

impl From<rustls::Error> for ClusterError {
    fn from(e: rustls::Error) -> Self {
        ClusterError::Tls(e)
    }
}
