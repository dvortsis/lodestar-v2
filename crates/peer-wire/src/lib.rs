//! The BitTorrent peer wire, as far as metadata fetching needs it
//! (architecture §6): the BEP 3 handshake, the BEP 10 extension protocol,
//! BEP 9 `ut_metadata` with SHA-1 (v1) or SHA-256 (v2) verification, MSE/PE
//! obfuscation with plaintext fallback, and reading BEP 11 PEX peers along
//! the way. It runs over TCP or, behind [`FetchOptions::transport`], uTP
//! (BEP 29, `lodestar-utp`), with outcomes counted per transport.
//!
//! Every byte from a peer is untrusted: message sizes, the advertised
//! metadata size and piece data are all bounded and checked, and the whole
//! exchange runs under one deadline.

mod fetch;
pub mod message;
pub mod mse;
mod rc4;
mod stats;
mod wire;

#[cfg(feature = "testing")]
pub mod testing;

use std::fmt;
use std::io;

pub use fetch::{Encryption, FetchOptions, Fetched, fetch, fetch_on_stream};
/// The uTP crate, for building the socket `FetchOptions::utp` takes.
pub use lodestar_utp as utp;
pub use stats::{FetchStats, Transport};

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Couldn't connect.
    Connect(io::Error),
    /// The connection failed mid-exchange.
    Io(io::Error),
    Timeout,
    /// The MSE handshake failed (or the peer hung up during it).
    Mse(&'static str),
    Handshake(&'static str),
    /// The peer doesn't speak the extension protocol (BEP 10).
    NoExtensions,
    /// The peer doesn't offer `ut_metadata` (BEP 9).
    NoMetadata,
    /// The peer refused our metadata requests.
    Rejected,
    /// The advertised metadata size is zero or over the limit.
    MetadataSize(u64),
    Protocol(&'static str),
    /// The assembled metadata doesn't hash to the info hash.
    HashMismatch,
}

/// Coarse outcome classes for metrics and the Insights loss reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureKind {
    Unreachable,
    TimedOut,
    Refused,
    EncryptionFailed,
    Invalid,
    Oversize,
}

impl Error {
    pub fn kind(&self) -> FailureKind {
        match self {
            Error::Connect(_) => FailureKind::Unreachable,
            Error::Timeout => FailureKind::TimedOut,
            Error::Io(_) | Error::NoExtensions | Error::NoMetadata | Error::Rejected => {
                FailureKind::Refused
            }
            Error::Mse(_) => FailureKind::EncryptionFailed,
            Error::MetadataSize(_) => FailureKind::Oversize,
            Error::Handshake(_) | Error::Protocol(_) | Error::HashMismatch => FailureKind::Invalid,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Connect(e) => write!(f, "connect failed: {e}"),
            Error::Io(e) => write!(f, "connection failed: {e}"),
            Error::Timeout => f.write_str("timed out"),
            Error::Mse(why) => write!(f, "encryption handshake failed: {why}"),
            Error::Handshake(why) => write!(f, "bad handshake: {why}"),
            Error::NoExtensions => f.write_str("peer lacks the extension protocol"),
            Error::NoMetadata => f.write_str("peer doesn't offer metadata"),
            Error::Rejected => f.write_str("peer rejected metadata requests"),
            Error::MetadataSize(n) => write!(f, "metadata size {n} out of range"),
            Error::Protocol(why) => write!(f, "protocol violation: {why}"),
            Error::HashMismatch => f.write_str("metadata doesn't match the info hash"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}
