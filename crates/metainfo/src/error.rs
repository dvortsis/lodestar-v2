use std::fmt;

use lodestar_bencode::DecodeError;

/// Why an info dictionary or `.torrent` was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Input larger than the configured cap (recorded as "oversize" upstream).
    TooLarge {
        size: usize,
        limit: usize,
    },
    Bencode(DecodeError),
    MissingField(&'static str),
    WrongType {
        field: &'static str,
        expected: &'static str,
    },
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
    LimitExceeded {
        what: &'static str,
        limit: usize,
    },
    UnsupportedMetaVersion(i64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooLarge { size, limit } => {
                write!(f, "metainfo is {size} bytes, over the {limit}-byte limit")
            }
            Error::Bencode(e) => e.fmt(f),
            Error::MissingField(field) => write!(f, "missing field {field:?}"),
            Error::WrongType { field, expected } => {
                write!(f, "field {field:?} should be a {expected}")
            }
            Error::Invalid { field, reason } => write!(f, "invalid {field:?}: {reason}"),
            Error::LimitExceeded { what, limit } => {
                write!(f, "{what} exceeds the limit of {limit}")
            }
            Error::UnsupportedMetaVersion(v) => write!(f, "unsupported meta version {v}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Bencode(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DecodeError> for Error {
    fn from(e: DecodeError) -> Self {
        Error::Bencode(e)
    }
}

/// A non-fatal anomaly found while parsing. Each kind is reported once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Warning {
    /// `pieces` doesn't match `ceil(total size / piece length)`.
    PieceCountMismatch { expected: u64, actual: u64 },
    /// A hybrid torrent's v1 file list and v2 file tree disagree.
    HybridMismatch,
    /// A path segment is empty, `.`, `..`, or contains `/`, `\` or NUL.
    SuspiciousPath,
    /// A per-file `md5sum`/`sha1`/`ed2k` value was malformed and ignored.
    BadFileHash,
    /// A v2 file entry lacks a well-formed `pieces root`, or the tree has
    /// empty directories.
    MalformedV2,
    /// Unknown info keys were larger than the cap and were not kept.
    ExtrasDropped { size: usize },
}
