//! Tracker scrapes (architecture §3, §7, §15): seeders, leechers and
//! completed counts for batches of info hashes, over UDP (BEP 15) or HTTP
//! (BEP 48).
//!
//! This crate is the protocol only. Choosing which hashes to scrape, rate
//! limits, backoff and ban handling belong to the scrape scheduler. One
//! call is one request to one tracker: [`Tracker::scrape`] returns the
//! counts in request order, `None` for a hash the tracker left out.
//!
//! Every reply is untrusted. The parsers ([`udp::parse_response`],
//! [`http::parse_scrape`]) are pure, bounded and fuzzed, and a reply that
//! doesn't match its request (wrong transaction, more entries than asked
//! for, an oversized body) is rejected rather than half-used.
//!
//! A scrape announces nothing: our address never enters a swarm's peer
//! list. The tracker does learn which hashes we asked about, so private
//! torrents must never be scraped on public trackers (the scheduler's
//! rule, §18.7).

mod address;
pub mod http;
pub mod udp;

#[cfg(feature = "testing")]
pub mod testing;

use std::fmt;
use std::io;
use std::time::Duration;

use lodestar_metainfo::InfoHash;

pub use crate::address::{Scheme, TrackerUrl, UrlError};
pub use crate::http::{HttpConfig, HttpTracker};
pub use crate::udp::{UdpConfig, UdpTracker};

/// One hash's counts as a tracker reported them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub seeders: u32,
    /// Completed downloads (`downloaded` in BEP 48).
    pub completed: u32,
    pub leechers: u32,
}

impl Counts {
    /// Whether the tracker knows the swarm. Trackers such as opentracker
    /// answer zeros for hashes they've never seen, so only a non-zero
    /// answer counts as "reporting" (§15).
    pub fn is_reporting(&self) -> bool {
        self.seeders > 0 || self.completed > 0 || self.leechers > 0
    }
}

/// A tracker's answer to one scrape request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reply {
    /// In request order; `None` where the tracker gave no entry.
    pub counts: Vec<Option<Counts>>,
    /// The tracker asked us to wait this long before scraping it again
    /// (BEP 48 `min_request_interval`).
    pub retry_after: Option<Duration>,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// No answer within the deadline, after retries.
    Timeout,
    /// The host name didn't resolve.
    Resolve,
    Io(io::Error),
    /// The tracker refused with a message (UDP error action, HTTP
    /// `failure reason`). Rate limits and bans usually arrive this way.
    Refused(String),
    /// A non-200 HTTP status.
    Status(u16),
    /// A reply that breaks the protocol or doesn't match the request.
    Protocol(&'static str),
    /// The reply body passed the size cap.
    TooLarge,
    /// The request had no hashes, or more than one request may carry.
    BadRequest(&'static str),
    Http(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Timeout => f.write_str("tracker timed out"),
            Error::Resolve => f.write_str("tracker host did not resolve"),
            Error::Io(e) => write!(f, "tracker i/o: {e}"),
            Error::Refused(m) => write!(f, "tracker refused: {m}"),
            Error::Status(s) => write!(f, "tracker returned HTTP {s}"),
            Error::Protocol(what) => write!(f, "tracker protocol error: {what}"),
            Error::TooLarge => f.write_str("tracker reply too large"),
            Error::BadRequest(what) => write!(f, "bad scrape request: {what}"),
            Error::Http(e) => write!(f, "tracker http: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Longest tracker message kept, in characters: enough to read a rate
/// limit notice, not enough to fill logs with whatever a tracker sends.
pub(crate) const MAX_MESSAGE: usize = 200;

pub(crate) fn message(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_MESSAGE)
        .collect()
}

/// A tracker of either kind.
#[derive(Debug)]
pub enum Tracker {
    Udp(UdpTracker),
    Http(HttpTracker),
}

impl Tracker {
    /// A client for `url`. HTTP trackers share `client` (see
    /// [`http::client`]); `None` when the URL has no scrape convention.
    pub fn new(
        url: &TrackerUrl,
        udp: UdpConfig,
        http: HttpConfig,
        client: &reqwest::Client,
    ) -> Option<Tracker> {
        Some(match url.scheme() {
            Scheme::Udp => Tracker::Udp(UdpTracker::new(url.host(), url.port(), udp)),
            Scheme::Http | Scheme::Https => {
                Tracker::Http(HttpTracker::new(url.scrape_url()?, http, client.clone()))
            }
        })
    }

    /// Most hashes one request may carry.
    pub fn max_hashes(&self) -> usize {
        match self {
            Tracker::Udp(_) => udp::MAX_HASHES,
            Tracker::Http(t) => t.max_hashes(),
        }
    }

    pub async fn scrape(&mut self, hashes: &[InfoHash]) -> Result<Reply> {
        match self {
            Tracker::Udp(t) => t.scrape(hashes).await,
            Tracker::Http(t) => t.scrape(hashes).await,
        }
    }
}

pub(crate) fn check_batch(hashes: &[InfoHash], max: usize) -> Result<()> {
    if hashes.is_empty() {
        return Err(Error::BadRequest("no hashes"));
    }
    if hashes.len() > max {
        return Err(Error::BadRequest("too many hashes for one request"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporting_needs_a_non_zero_count() {
        assert!(!Counts::default().is_reporting());
        for c in [
            Counts {
                seeders: 1,
                ..Counts::default()
            },
            Counts {
                completed: 1,
                ..Counts::default()
            },
            Counts {
                leechers: 1,
                ..Counts::default()
            },
        ] {
            assert!(c.is_reporting());
        }
    }

    #[test]
    fn messages_are_bounded_and_printable() {
        let m = message(b"rate \x1b[31mlimited\n");
        assert_eq!(m, "rate [31mlimited");
        assert_eq!(message(&[b'x'; 1000]).len(), MAX_MESSAGE);
        assert_eq!(message(&[0xff, b'a']), "\u{fffd}a");
    }
}
