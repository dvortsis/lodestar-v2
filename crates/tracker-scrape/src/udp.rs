//! UDP tracker scrape (BEP 15).
//!
//! ```text
//! connect   →  protocol_id u64 | action 0 u32 | transaction u32
//!           ←  action 0 | transaction | connection_id u64
//! scrape    →  connection_id | action 2 | transaction | info_hash × n
//!           ←  action 2 | transaction | (seeders, completed, leechers) u32 × n
//! error     ←  action 3 | transaction | message
//! ```
//!
//! The socket is connected to the tracker, so the kernel drops datagrams
//! from anyone else, and a reply must also carry the transaction id we
//! sent. A connection id is reused for a minute (BEP 15), and dropped
//! early if the tracker rejects it.

use std::net::SocketAddr;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout_at};

use crate::{Counts, Error, Reply, Result, check_batch, message};

/// Most hashes one scrape packet may carry: 8 + 74 × 12 bytes of reply
/// fits a 900-byte datagram, under every path MTU.
pub const MAX_HASHES: usize = 74;
const PROTOCOL_ID: u64 = 0x0417_2710_1980;
const CONNECT: u32 = 0;
const SCRAPE: u32 = 2;
const ERROR: u32 = 3;
/// How long BEP 15 lets a client reuse a connection id.
const CONNECTION_TTL: Duration = Duration::from_secs(60);
/// Big enough for any legitimate reply, with room for long error text.
const RECV_BUFFER: usize = 2048;

#[derive(Debug, Clone)]
pub struct UdpConfig {
    /// How long to wait for each reply.
    pub timeout: Duration,
    /// Extra attempts after a timeout.
    pub retries: u32,
    /// Local address to send from: the crawler's bound interface, so
    /// scrapes leave through the same VPN as the DHT (§5). Port 0 picks
    /// an ephemeral port. `None` binds the unspecified address of the
    /// tracker's family.
    pub bind: Option<SocketAddr>,
    /// Re-resolve the tracker's host this often.
    pub resolve_every: Duration,
}

impl Default for UdpConfig {
    fn default() -> Self {
        UdpConfig {
            timeout: Duration::from_secs(5),
            retries: 1,
            bind: None,
            resolve_every: Duration::from_secs(3600),
        }
    }
}

/// A tracker's reply to one of our requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Connect {
        transaction: u32,
        connection_id: u64,
    },
    /// Entries in request order; the count can't be checked here, only
    /// against the request.
    Scrape {
        transaction: u32,
        counts: Vec<Counts>,
    },
    Error {
        transaction: u32,
        message: String,
    },
}

impl Packet {
    pub fn transaction(&self) -> u32 {
        match self {
            Packet::Connect { transaction, .. }
            | Packet::Scrape { transaction, .. }
            | Packet::Error { transaction, .. } => *transaction,
        }
    }
}

pub fn connect_request(transaction: u32) -> [u8; 16] {
    let mut p = [0u8; 16];
    p[..8].copy_from_slice(&PROTOCOL_ID.to_be_bytes());
    p[8..12].copy_from_slice(&CONNECT.to_be_bytes());
    p[12..].copy_from_slice(&transaction.to_be_bytes());
    p
}

pub fn scrape_request(connection_id: u64, transaction: u32, hashes: &[InfoHash]) -> Vec<u8> {
    let mut p = Vec::with_capacity(16 + 20 * hashes.len());
    p.extend_from_slice(&connection_id.to_be_bytes());
    p.extend_from_slice(&SCRAPE.to_be_bytes());
    p.extend_from_slice(&transaction.to_be_bytes());
    for h in hashes {
        p.extend_from_slice(&h.0);
    }
    p
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().expect("4 bytes"))
}

/// Parses one datagram from a tracker. Announce replies and anything
/// malformed are errors; error text is kept short and printable.
pub fn parse_response(p: &[u8]) -> Result<Packet> {
    if p.len() < 8 {
        return Err(Error::Protocol("short packet"));
    }
    let (action, transaction, body) = (be32(p), be32(&p[4..]), &p[8..]);
    match action {
        CONNECT => {
            let id: [u8; 8] = body
                .try_into()
                .map_err(|_| Error::Protocol("connect reply length"))?;
            Ok(Packet::Connect {
                transaction,
                connection_id: u64::from_be_bytes(id),
            })
        }
        SCRAPE => {
            if body.len() % 12 != 0 || body.len() / 12 > MAX_HASHES {
                return Err(Error::Protocol("scrape reply length"));
            }
            let counts = body
                .chunks_exact(12)
                .map(|c| Counts {
                    seeders: be32(c),
                    completed: be32(&c[4..]),
                    leechers: be32(&c[8..]),
                })
                .collect();
            Ok(Packet::Scrape {
                transaction,
                counts,
            })
        }
        ERROR => Ok(Packet::Error {
            transaction,
            message: message(body),
        }),
        _ => Err(Error::Protocol("unexpected action")),
    }
}

/// One UDP tracker: its socket and connection id.
#[derive(Debug)]
pub struct UdpTracker {
    host: String,
    port: u16,
    cfg: UdpConfig,
    socket: Option<(UdpSocket, Instant)>,
    connection: Option<(u64, Instant)>,
}

impl UdpTracker {
    pub fn new(host: &str, port: u16, cfg: UdpConfig) -> UdpTracker {
        UdpTracker {
            host: host.to_owned(),
            port,
            cfg,
            socket: None,
            connection: None,
        }
    }

    /// The tracker's address, once resolved.
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.socket.as_ref().and_then(|(s, _)| s.peer_addr().ok())
    }

    /// Scrapes up to [`MAX_HASHES`] hashes.
    pub async fn scrape(&mut self, hashes: &[InfoHash]) -> Result<Reply> {
        check_batch(hashes, MAX_HASHES)?;
        let result = self.try_scrape(hashes).await;
        if matches!(result, Err(Error::Io(_) | Error::Resolve)) {
            // Start over next time: a new address, a new socket.
            self.socket = None;
            self.connection = None;
        }
        result
    }

    async fn try_scrape(&mut self, hashes: &[InfoHash]) -> Result<Reply> {
        let mut rejected_cached = false;
        let mut attempt = 0;
        loop {
            let cached = self
                .connection
                .is_some_and(|(_, at)| at.elapsed() < CONNECTION_TTL);
            let result = match self.connection_id().await {
                Ok(id) => {
                    let tid = rand::random();
                    let request = scrape_request(id, tid, hashes);
                    self.exchange(&request, tid).await
                }
                Err(e) => Err(e),
            };
            match result {
                Ok(Packet::Scrape { counts, .. }) => {
                    if counts.len() > hashes.len() {
                        return Err(Error::Protocol("more entries than hashes"));
                    }
                    let mut counts: Vec<Option<Counts>> = counts.into_iter().map(Some).collect();
                    counts.resize(hashes.len(), None);
                    return Ok(Reply {
                        counts,
                        retry_after: None,
                    });
                }
                // A cached connection id may have expired early on the
                // tracker's side: reconnect once before believing it.
                Ok(Packet::Error { .. }) if cached && !rejected_cached => {
                    rejected_cached = true;
                    self.connection = None;
                }
                Ok(Packet::Error { message, .. }) => return Err(Error::Refused(message)),
                Ok(Packet::Connect { .. }) => return Err(Error::Protocol("unexpected action")),
                Err(Error::Timeout) if attempt < self.cfg.retries => {
                    attempt += 1;
                    self.connection = None;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn connection_id(&mut self) -> Result<u64> {
        if let Some((id, at)) = self.connection {
            if at.elapsed() < CONNECTION_TTL {
                return Ok(id);
            }
        }
        let tid = rand::random();
        match self.exchange(&connect_request(tid), tid).await? {
            Packet::Connect { connection_id, .. } => {
                self.connection = Some((connection_id, Instant::now()));
                Ok(connection_id)
            }
            Packet::Error { message, .. } => Err(Error::Refused(message)),
            Packet::Scrape { .. } => Err(Error::Protocol("unexpected action")),
        }
    }

    async fn socket(&mut self) -> Result<&UdpSocket> {
        let stale = self
            .socket
            .as_ref()
            .is_none_or(|(_, at)| at.elapsed() >= self.cfg.resolve_every);
        if stale {
            let addr = tokio::net::lookup_host((self.host.as_str(), self.port))
                .await
                .map_err(|_| Error::Resolve)?
                .find(|a| self.cfg.bind.is_none_or(|b| b.is_ipv4() == a.is_ipv4()))
                .ok_or(Error::Resolve)?;
            let bind = self.cfg.bind.unwrap_or_else(|| match addr {
                SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
                SocketAddr::V6(_) => SocketAddr::from(([0u16; 8], 0)),
            });
            let socket = UdpSocket::bind(bind).await?;
            socket.connect(addr).await?;
            self.socket = Some((socket, Instant::now()));
            self.connection = None;
        }
        Ok(&self.socket.as_ref().expect("socket just set").0)
    }

    /// Sends `request` and waits for the reply carrying `tid`. Datagrams
    /// that don't parse or answer something else are skipped.
    async fn exchange(&mut self, request: &[u8], tid: u32) -> Result<Packet> {
        let wait = self.cfg.timeout;
        let socket = self.socket().await?;
        socket.send(request).await?;
        let deadline = Instant::now() + wait;
        let mut buf = [0u8; RECV_BUFFER];
        loop {
            let n = match timeout_at(deadline, socket.recv(&mut buf)).await {
                Ok(n) => n?,
                Err(_) => return Err(Error::Timeout),
            };
            match parse_response(&buf[..n]) {
                Ok(p) if p.transaction() == tid => return Ok(p),
                _ => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(action: u32, tid: u32, body: &[u8]) -> Vec<u8> {
        let mut p = action.to_be_bytes().to_vec();
        p.extend_from_slice(&tid.to_be_bytes());
        p.extend_from_slice(body);
        p
    }

    #[test]
    fn requests_follow_bep15() {
        let c = connect_request(0xdead_beef);
        assert_eq!(
            c,
            [
                0, 0, 4, 0x17, 0x27, 0x10, 0x19, 0x80, 0, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef
            ]
        );
        let s = scrape_request(7, 9, &[InfoHash([1; 20]), InfoHash([2; 20])]);
        assert_eq!(s.len(), 16 + 40);
        assert_eq!(&s[..8], &7u64.to_be_bytes());
        assert_eq!(&s[8..12], &2u32.to_be_bytes());
        assert_eq!(&s[12..16], &9u32.to_be_bytes());
        assert_eq!(&s[36..], &[2; 20]);
    }

    #[test]
    fn parses_replies() {
        assert_eq!(
            parse_response(&reply(0, 5, &42u64.to_be_bytes())).unwrap(),
            Packet::Connect {
                transaction: 5,
                connection_id: 42
            }
        );
        let mut body = Vec::new();
        for v in [3u32, 10, 1] {
            body.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(
            parse_response(&reply(2, 6, &body)).unwrap(),
            Packet::Scrape {
                transaction: 6,
                counts: vec![Counts {
                    seeders: 3,
                    completed: 10,
                    leechers: 1
                }]
            }
        );
        assert_eq!(
            parse_response(&reply(2, 6, &[])).unwrap(),
            Packet::Scrape {
                transaction: 6,
                counts: vec![]
            }
        );
        assert_eq!(
            parse_response(&reply(3, 7, b"slow down")).unwrap(),
            Packet::Error {
                transaction: 7,
                message: "slow down".into()
            }
        );
    }

    #[test]
    fn rejects_malformed_replies() {
        let bad = |p: &[u8]| assert!(parse_response(p).is_err(), "{p:?}");
        bad(&[0; 7]);
        bad(&reply(0, 1, &[0; 7]));
        bad(&reply(0, 1, &[0; 9]));
        bad(&reply(2, 1, &[0; 13]));
        bad(&reply(2, 1, &vec![0; 12 * (MAX_HASHES + 1)]));
        // Announce replies answer a request we never send.
        bad(&reply(1, 1, &[0; 12]));
        bad(&reply(9, 1, &[]));
    }
}
