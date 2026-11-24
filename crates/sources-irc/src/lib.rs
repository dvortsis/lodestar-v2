//! A small IRC client for Lodestar's IRC sources (§18.1, §18.4, §18.7):
//! private-tracker announce channels and the live PreDB feed build on it.
//!
//! - [`line`]: framing, parsing and encoding, bounded and fuzzed (untrusted
//!   server bytes).
//! - [`client`]: the connection task. It connects only through the
//!   plugin's egress ([`client::Egress`], over NOS-2a's guarded, routed
//!   connect), speaks TLS by default, logs in with SASL PLAIN or NickServ
//!   over TLS only, reconnects with jittered back-off, rate-limits what it
//!   sends, and hands every message to a bounded queue ([`client::Events`]).
//! - [`announce`]: tracker announce definitions (Lodestar's own format)
//!   and the matcher that turns an announcer's line into a name, size,
//!   category and torrent id; passkeyed URLs are never stored.
//!
//! No IRC library is used: the protocol surface Lodestar needs is small,
//! and owning the parser keeps every bound in view.

pub mod announce;
pub mod client;
pub mod line;

pub use announce::{Announce, Definition, DefinitionError, Matcher, Outcome, SecretUrl};
pub use client::{
    Auth, Channel, Config, ConfigError, Disconnect, Egress, Events, Flood, Handle, Reconnect,
    Roots, SendError, State, Stats, StatsSnapshot, Stop, Timeouts, Transport, spawn,
};
pub use line::{Message, ParseError, Tag};
