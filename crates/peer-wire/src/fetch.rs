//! Fetching an info dictionary from one peer (BEP 9), over TCP or uTP.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_utp::UtpSocket;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout, timeout_at};

use crate::Error;
use crate::message::{
    self, HANDSHAKE_LEN, METADATA_PIECE, MetadataMessage, OUR_UT_METADATA, OUR_UT_PEX,
};
use crate::mse::{self, Ciphers};
use crate::stats::{FetchStats, Transport};
use crate::wire::{Incoming, Wire};

/// How to use MSE/PE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encryption {
    /// Try MSE first (offering RC4 and plaintext), then plain BitTorrent if
    /// the peer doesn't do MSE. Reaches both kinds of peers.
    #[default]
    Prefer,
    /// MSE with RC4 only.
    Require,
    /// Plain BitTorrent only.
    Disable,
}

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub connect_timeout: Duration,
    /// Deadline for the whole fetch, including a plaintext retry.
    pub timeout: Duration,
    /// Larger metadata is refused (recorded upstream as "oversize").
    pub max_metadata: usize,
    pub encryption: Encryption,
    pub peer_id: [u8; 20],
    /// The transport to try first. TCP by default; uTP needs `utp`.
    pub transport: Transport,
    /// With `transport: Utp`: when the peer doesn't answer over uTP
    /// within `utp_fallback_after` (or `utp` is unset), fetch over TCP
    /// instead, inside the same deadline. A peer that answered over uTP
    /// isn't retried over TCP.
    pub fallback_to_tcp: bool,
    pub utp_fallback_after: Duration,
    /// The uTP socket, normally the one sharing the DHT's port (§5).
    pub utp: Option<UtpSocket>,
    /// Attempts and successes per transport; clones share it.
    pub stats: Arc<FetchStats>,
    /// Addresses never connected to, over any transport: subscribed IP blocklists (§20.13).
    pub deny: Option<Deny>,
    /// Explicit opt-in for isolated/local workflows. Public discovery leaves false.
    pub allow_private: bool,
}

/// A live address check, shared with whoever keeps the lists current.
#[derive(Clone)]
pub struct Deny(pub Arc<dyn Fn(IpAddr) -> bool + Send + Sync>);

impl Deny {
    pub fn denies(&self, ip: IpAddr) -> bool {
        (self.0)(ip)
    }
}

impl std::fmt::Debug for Deny {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Deny")
    }
}

impl Default for FetchOptions {
    fn default() -> Self {
        // Azureus-style peer id: client "LD", version 0001, random tail.
        let mut peer_id = *b"-LD0001-000000000000";
        let tail: [u8; 12] = rand::rng().random();
        for (dst, b) in peer_id[8..].iter_mut().zip(tail) {
            *dst = b"0123456789abcdefghijklmnopqrstuvwxyz"[usize::from(b) % 36];
        }
        FetchOptions {
            connect_timeout: Duration::from_secs(4),
            timeout: Duration::from_secs(8),
            max_metadata: 16 << 20,
            encryption: Encryption::default(),
            peer_id,
            transport: Transport::Tcp,
            fallback_to_tcp: true,
            utp_fallback_after: Duration::from_secs(2),
            utp: None,
            stats: Arc::new(FetchStats::default()),
            deny: None,
            allow_private: false,
        }
    }
}

impl FetchOptions {
    /// Apply before retaining peers and again immediately before a dial.
    pub fn allows(&self, addr: SocketAddr) -> bool {
        let addr = lodestar_network_policy::normalize(addr);
        lodestar_network_policy::allows(addr, self.allow_private)
            && !self.deny.as_ref().is_some_and(|d| d.denies(addr.ip()))
    }
}

/// A verified info dictionary and what we learned on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The raw info dictionary, verified against the info hash.
    pub info: Vec<u8>,
    /// Verified by truncated SHA-256 (a v2 swarm key) rather than SHA-1.
    pub v2: bool,
    /// The payload stream was RC4-encrypted.
    pub encrypted: bool,
    /// Peers the peer told us about via PEX.
    pub pex: Vec<SocketAddr>,
    /// The peer's client name from its extended handshake.
    pub client: Option<String>,
}

/// Extended messages larger than a metadata piece plus header are bogus.
const MAX_EXTENDED: usize = METADATA_PIECE + 1024;
/// Other messages (e.g. a huge bitfield) are skipped up to this size.
const MAX_OTHER: usize = 4 << 20;
/// Piece requests kept in flight.
const REQUEST_WINDOW: usize = 64;
const MAX_PEX: usize = 200;

/// Fetches and verifies the info dictionary for `info_hash` from `addr`.
///
/// The deadline, the size cap and every other limit are the same over
/// either transport; only how the connection is opened differs.
pub async fn fetch(
    addr: SocketAddr,
    info_hash: InfoHash,
    opts: &FetchOptions,
) -> Result<Fetched, Error> {
    let r = fetch_any(addr, info_hash, opts).await;
    // Counted here only, once per fetch, whichever transports it tried.
    if let Some(kind) = counted(&r) {
        crate::log_events::count(kind);
    }
    r
}

/// What a fetch's result counts as in `fetch.failed_summary`: a failure's
/// kind, or nothing for a success or a blocklisted address (never
/// contacted).
fn counted(r: &Result<Fetched, Error>) -> Option<crate::FailureKind> {
    match r {
        Err(Error::Denied) | Ok(_) => None,
        Err(e) => Some(e.kind()),
    }
}

/// [`fetch`], over uTP first and TCP as the fallback when allowed.
async fn fetch_any(
    addr: SocketAddr,
    info_hash: InfoHash,
    opts: &FetchOptions,
) -> Result<Fetched, Error> {
    let addr = lodestar_network_policy::normalize(addr);
    if !opts.allows(addr) {
        return Err(Error::Denied);
    }
    let deadline = Instant::now() + opts.timeout;
    if opts.transport == Transport::Tcp {
        return fetch_via(Transport::Tcp, addr, info_hash, opts, deadline)
            .await
            .0;
    }
    if opts.utp.is_none() && opts.fallback_to_tcp {
        return fetch_via(Transport::Tcp, addr, info_hash, opts, deadline)
            .await
            .0;
    }
    match fetch_via(Transport::Utp, addr, info_hash, opts, deadline).await {
        (Err(_), false) if opts.fallback_to_tcp && Instant::now() < deadline => {
            fetch_via(Transport::Tcp, addr, info_hash, opts, deadline)
                .await
                .0
        }
        (r, _) => r,
    }
}

/// One transport, with the MSE → plaintext retry. Also says whether a
/// connection ever opened (which rules out falling back).
async fn fetch_via(
    transport: Transport,
    addr: SocketAddr,
    info_hash: InfoHash,
    opts: &FetchOptions,
    deadline: Instant,
) -> (Result<Fetched, Error>, bool) {
    opts.stats.attempted(transport);
    let connected = AtomicBool::new(false);
    let connected = &connected;
    let run = |encrypted: bool| async move {
        timeout_at(
            deadline,
            attempt(transport, addr, &info_hash, opts, encrypted, connected),
        )
        .await
        .map_err(|_| Error::Timeout)?
    };
    let r = match opts.encryption {
        Encryption::Disable => run(false).await,
        Encryption::Require => run(true).await,
        Encryption::Prefer => match run(true).await {
            // A peer without MSE drops the connection on our DH key; retry
            // plain while time remains.
            Err(Error::Mse(_)) if Instant::now() < deadline => run(false).await,
            other => other,
        },
    };
    match &r {
        Ok(_) => opts.stats.succeeded(transport),
        Err(e) => {
            let kind = e.kind();
            lodestar_log::log_event!(
                crate::log_events::FETCH_FAILED,
                torrent = lodestar_log::Torrent(&info_hash.0),
                peer = lodestar_log::Peer(addr),
                error = lodestar_log::Error::kind(crate::log_events::label(kind)),
                attempt = match transport {
                    Transport::Tcp => "tcp",
                    Transport::Utp => "utp",
                }
            );
        }
    }
    (r, connected.load(Ordering::Relaxed))
}

async fn attempt(
    transport: Transport,
    addr: SocketAddr,
    info_hash: &InfoHash,
    opts: &FetchOptions,
    encrypted: bool,
    connected: &AtomicBool,
) -> Result<Fetched, Error> {
    if !opts.allows(addr) {
        return Err(Error::Denied);
    }
    let timed_out = || {
        opts.stats.connect_timed_out(transport);
        Error::Timeout
    };
    match transport {
        Transport::Tcp => {
            let tcp = timeout(opts.connect_timeout, TcpStream::connect(addr))
                .await
                .map_err(|_| timed_out())?
                .map_err(Error::Connect)?;
            prepare_outbound(&tcp);
            connected.store(true, Ordering::Relaxed);
            fetch_on_stream(BufReader::new(tcp), info_hash, opts, encrypted).await
        }
        Transport::Utp => {
            let Some(utp) = opts.utp.as_ref() else {
                return Err(Error::Connect(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "no uTP socket configured",
                )));
            };
            // Give TCP time to run when it's the fallback.
            let wait = if opts.fallback_to_tcp {
                opts.connect_timeout.min(opts.utp_fallback_after)
            } else {
                opts.connect_timeout
            };
            let stream = timeout(wait, utp.connect(addr))
                .await
                .map_err(|_| timed_out())?
                .map_err(Error::Connect)?;
            connected.store(true, Ordering::Relaxed);
            fetch_on_stream(BufReader::new(stream), info_hash, opts, encrypted).await
        }
    }
}

/// Readies an outbound peer connection: no Nagle delay, and on Windows
/// an abortive close (`SO_LINGER(0)`). A fetch is a few round trips, so
/// thousands of them a minute would otherwise leave their ports in
/// TIME_WAIT for two minutes each, and Windows' default 16k dynamic
/// ports run out (architecture §11). Nothing is in flight when a fetch
/// ends, so the reset loses nothing. Other systems keep the graceful
/// close: Linux's default range is larger, macOS holds TIME_WAIT for
/// 30 s rather than 120 s, and nothing measured there calls for it.
fn prepare_outbound(tcp: &TcpStream) {
    let _ = tcp.set_nodelay(true);
    #[cfg(windows)]
    let _ = tcp.set_zero_linger();
}

/// The BEP 3/10/9 exchange (and MSE when `encrypted`) over an already
/// open stream of any transport. One attempt: no plaintext retry and no
/// deadline of its own (bound it with `opts.timeout` as [`fetch`] does).
pub async fn fetch_on_stream<S>(
    mut stream: S,
    info_hash: &InfoHash,
    opts: &FetchOptions,
    encrypted: bool,
) -> Result<Fetched, Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let hello = message::handshake(&info_hash.0, &opts.peer_id, true);

    let (ciphers, rc4) = if encrypted {
        let provide = match opts.encryption {
            Encryption::Require => mse::RC4,
            _ => mse::RC4 | mse::PLAINTEXT,
        };
        // Our handshake rides inside MSE as the initial payload.
        // A Send RNG: the fetch future must be spawnable across threads.
        let mut rng = StdRng::from_rng(&mut rand::rng());
        match mse::initiate(&mut stream, &info_hash.0, provide, &hello, &mut rng).await {
            Ok((ciphers, selected)) => (ciphers, selected == mse::RC4),
            Err(Error::Io(_)) => return Err(Error::Mse("peer closed the encrypted handshake")),
            Err(e) => return Err(e),
        }
    } else {
        use tokio::io::AsyncWriteExt;
        stream.write_all(&hello).await?;
        (
            Ciphers {
                read: None,
                write: None,
            },
            false,
        )
    };
    let mut wire = Wire::new(stream, ciphers, Vec::new());

    let mut theirs = [0u8; HANDSHAKE_LEN];
    wire.read_exact(&mut theirs).await?;
    let hs = message::parse_handshake(&theirs)?;
    if hs.info_hash != info_hash.0 {
        return Err(Error::Handshake("wrong info hash"));
    }
    if !hs.extensions {
        return Err(Error::NoExtensions);
    }
    wire.send_extended(0, &message::our_extended_handshake(None))
        .await?;

    let mut their_metadata_id = None;
    let mut client = None;
    let mut pex = Vec::new();
    let mut assembly: Option<Assembly> = None;
    loop {
        let payload = match wire.read_message(MAX_EXTENDED, MAX_OTHER).await? {
            Incoming::Extended(p) => p,
            Incoming::KeepAlive | Incoming::Other(_) => continue,
        };
        let (ext_id, body) = (payload[0], &payload[1..]);
        match ext_id {
            0 => {
                if assembly.is_some() {
                    continue; // a repeated handshake changes nothing we need
                }
                let ext = message::parse_extended_handshake(body)?;
                client = ext.client;
                let id = ext.ut_metadata.ok_or(Error::NoMetadata)?;
                let size = ext.metadata_size.ok_or(Error::NoMetadata)?;
                let size = usize::try_from(size)
                    .ok()
                    .filter(|s| *s > 0 && *s <= opts.max_metadata)
                    .ok_or(Error::MetadataSize(size.max(0) as u64))?;
                their_metadata_id = Some(id);
                let mut a = Assembly::new(size);
                while let Some(piece) = a.next_request() {
                    wire.send_extended(id, &message::metadata_request(piece))
                        .await?;
                }
                assembly = Some(a);
            }
            OUR_UT_METADATA => {
                let (Some(a), Some(their_id)) = (assembly.as_mut(), their_metadata_id) else {
                    return Err(Error::Protocol("metadata before extended handshake"));
                };
                match message::parse_metadata_message(body)? {
                    MetadataMessage::Data {
                        piece,
                        total_size,
                        data,
                    } => {
                        if total_size.is_some_and(|t| t != a.size as i64) {
                            return Err(Error::Protocol("total_size changed"));
                        }
                        a.accept(piece, data)?;
                        if a.is_complete() {
                            let info = std::mem::take(&mut a.buf);
                            let v2 = verify(&info, info_hash)?;
                            return Ok(Fetched {
                                info,
                                v2,
                                encrypted: rc4,
                                pex,
                                client,
                            });
                        }
                        while let Some(piece) = a.next_request() {
                            wire.send_extended(their_id, &message::metadata_request(piece))
                                .await?;
                        }
                    }
                    MetadataMessage::Reject { .. } => return Err(Error::Rejected),
                    MetadataMessage::Request { piece } => {
                        // We serve nothing; say so politely.
                        let piece = usize::try_from(piece).unwrap_or(0);
                        wire.send_extended(their_id, &message::metadata_reject(piece))
                            .await?;
                    }
                }
            }
            OUR_UT_PEX => {
                for p in message::parse_pex(body) {
                    if pex.len() < MAX_PEX && !pex.contains(&p) {
                        pex.push(p);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Which hash the bytes match: SHA-1 (v1/hybrid key) or truncated
/// SHA-256 (pure v2 key). Returns whether it was the latter.
fn verify(info: &[u8], info_hash: &InfoHash) -> Result<bool, Error> {
    let sha1: [u8; 20] = Sha1::digest(info).into();
    if sha1 == info_hash.0 {
        return Ok(false);
    }
    let sha256 = Sha256::digest(info);
    if sha256[..20] == info_hash.0 {
        return Ok(true);
    }
    Err(Error::HashMismatch)
}

/// Collects metadata pieces, with a bounded window of outstanding requests.
/// The buffer grows as pieces arrive rather than up front, so a peer that
/// merely claims a large size costs nothing until it sends the bytes.
struct Assembly {
    size: usize,
    buf: Vec<u8>,
    received: Vec<bool>,
    remaining: usize,
    next: usize,
    in_flight: usize,
}

impl Assembly {
    fn new(size: usize) -> Assembly {
        let pieces = size.div_ceil(METADATA_PIECE);
        Assembly {
            size,
            buf: Vec::new(),
            received: vec![false; pieces],
            remaining: pieces,
            next: 0,
            in_flight: 0,
        }
    }

    fn piece_len(&self, piece: usize) -> usize {
        (self.size - piece * METADATA_PIECE).min(METADATA_PIECE)
    }

    fn next_request(&mut self) -> Option<usize> {
        if self.in_flight >= REQUEST_WINDOW || self.next >= self.received.len() {
            return None;
        }
        self.in_flight += 1;
        self.next += 1;
        Some(self.next - 1)
    }

    fn accept(&mut self, piece: i64, data: &[u8]) -> Result<(), Error> {
        let piece = usize::try_from(piece)
            .ok()
            .filter(|p| *p < self.next)
            .ok_or(Error::Protocol("unrequested metadata piece"))?;
        if data.len() != self.piece_len(piece) {
            return Err(Error::Protocol("metadata piece has the wrong length"));
        }
        if !self.received[piece] {
            let start = piece * METADATA_PIECE;
            let end = start + data.len();
            if self.buf.len() < end {
                self.buf.resize(end, 0);
            }
            self.buf[start..end].copy_from_slice(data);
            self.received[piece] = true;
            self.remaining -= 1;
            self.in_flight -= 1;
        }
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.remaining == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fetch counts once, by its final result: a success or a
    /// blocklisted address doesn't count, any other failure does by its
    /// kind. Only `fetch` counts (not each transport's try), so a uTP
    /// failure before a TCP success counts nothing.
    #[test]
    fn a_fetch_counts_by_its_final_result() {
        let fetched = Fetched {
            info: Vec::new(),
            v2: false,
            encrypted: false,
            pex: Vec::new(),
            client: None,
        };
        assert_eq!(counted(&Ok(fetched)), None);
        assert_eq!(counted(&Err(Error::Denied)), None);
        assert_eq!(
            counted(&Err(Error::Timeout)),
            Some(crate::FailureKind::TimedOut)
        );
        assert_eq!(
            counted(&Err(Error::Rejected)),
            Some(crate::FailureKind::Refused)
        );
    }

    #[tokio::test]
    async fn outbound_sockets_abort_on_windows_only() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        prepare_outbound(&tcp);
        assert!(tcp.nodelay().unwrap());
        let linger = tcp.linger().unwrap();
        if cfg!(windows) {
            assert_eq!(linger, Some(Duration::ZERO));
        } else {
            assert_eq!(linger, None);
        }
    }

    #[test]
    fn assembly_windows_and_validates() {
        let size = METADATA_PIECE * 100 + 10;
        let mut a = Assembly::new(size);
        assert_eq!(a.buf.capacity(), 0, "nothing allocated for a claimed size");
        let first: Vec<usize> = std::iter::from_fn(|| a.next_request()).collect();
        assert_eq!(first.len(), REQUEST_WINDOW);
        assert!(a.accept(100, &[0; 10]).is_err(), "not requested yet");
        assert!(a.accept(0, &[0; 5]).is_err(), "wrong length");
        a.accept(0, &[1; METADATA_PIECE]).unwrap();
        a.accept(0, &[1; METADATA_PIECE]).unwrap(); // duplicate ignored
        assert_eq!(a.next_request(), Some(REQUEST_WINDOW));
        assert!(a.accept(-1, &[]).is_err());
        // Out of order: the gap is zero-filled until its piece arrives.
        a.accept(2, &[2; METADATA_PIECE]).unwrap();
        assert_eq!(a.buf.len(), 3 * METADATA_PIECE);
        a.accept(1, &[3; METADATA_PIECE]).unwrap();
        assert_eq!(a.buf[METADATA_PIECE], 3);
        assert_eq!(a.buf.len(), 3 * METADATA_PIECE);
    }

    #[test]
    fn verification_accepts_v1_and_truncated_v2() {
        let data = b"d4:name1:xe";
        let v1 = InfoHash(Sha1::digest(data).into());
        let v2 = InfoHash(Sha256::digest(data)[..20].try_into().unwrap());
        assert!(!verify(data, &v1).unwrap());
        assert!(verify(data, &v2).unwrap());
        assert!(matches!(
            verify(data, &InfoHash([0; 20])),
            Err(Error::HashMismatch)
        ));
    }
}
