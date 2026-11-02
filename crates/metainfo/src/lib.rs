//! BitTorrent metainfo: the v1/v2/hybrid info dictionary (what BEP 9 delivers
//! from the DHT) and full `.torrent` files (imports only).
//!
//! Every input is treated as hostile: sizes, file counts, path depth and
//! lengths are capped by [`Limits`], arithmetic is checked, and parsing never
//! panics. Anomalies that don't make a torrent unusable (a piece count that
//! disagrees with the file sizes, suspicious path segments, an inconsistent
//! hybrid) are reported as [`Warning`]s rather than rejected, because the goal
//! is to capture everything that's out there; classification decides later
//! what to do with odd torrents.
//!
//! Names and paths keep their **raw bytes** plus the [`TextEncoding`] they
//! were judged to be in, so decoding can be redone when detection improves.

mod display;
mod error;
mod hash;
mod info;
mod text;
mod torrent;

pub use display::hidden_char;
pub use error::{Error, Warning};
pub use hash::{InfoHash, InfoHashV2, ParseHashError};
pub use info::{
    File, FileAttrs, FileHashes, Info, Limits, MetaVersion, ParseOptions, info_without_pieces,
};
pub use text::{Text, TextEncoding};
pub use torrent::TorrentFile;
