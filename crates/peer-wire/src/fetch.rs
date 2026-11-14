//! Fetching an info dictionary from one peer (BEP 9), over TCP or uTP.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
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
use crate::route;
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
    /// CR-4 (`dht.route_backoff`): a peer whose address family has no
    /// local route (a cached UDP-connect probe, `crate::route`) isn't
    /// dialled. Off probes nothing and dials as before.
    pub route_backoff: bool,
    /// X-fetch-meta-reserve: awaited once a peer has announced a valid
    /// `metadata_size`, before the first piece request. `None` reserves
    /// nothing (the fetch downloads as soon as the size checks out).
    pub reserve: Option<Reserve>,
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

/// Reserves memory for a fetch's result before it's downloaded
/// (X-fetch-meta-reserve).
///
/// The hook gets `(now, total)`. `total` is an upper bound on the heap
/// bytes the [`Fetched`] it may return can hold: the announced
/// `metadata_size` (already checked against
/// [`FetchOptions::max_metadata`]), the client name's length and room for
/// the most PEX peers kept. `now` is what to reserve before the next
/// requests go out. [`fetch_on_stream`] calls it in at most two steps, and
/// sends no request the reservation doesn't cover until the returned future
/// completes. A reservation that waits pauses the download, and the fetch's
/// deadline still runs.
/// - **At the extended handshake:** `now` covers the first request window
///   (its first 4 pieces, 64 KiB of the dict, or all of a smaller one).
/// - **Once the whole first window has arrived**, if the dict is larger:
///   `now == total`, before any request past the window.
///
/// A peer that announces a large size can therefore hold at most one
/// window of the budget until it has sent that window's 64 KiB, not its
/// whole claim for nothing.
///
/// The promise: a [`Fetched`] from that attempt has `info.capacity()`,
/// `pex.capacity() × size_of::<SocketAddr>()` and the client name's
/// capacity summing to at most `total`, and the dict's buffer never passes
/// what `now` covered. A peer that sends more than it announced, or a
/// piece it wasn't asked for, is refused.
///
/// A fetch that retries (MSE then plaintext, uTP then TCP) may call it
/// again for the same fetch, each call covering that attempt alone: keep
/// the largest. The hook never releases anything; whoever set it releases
/// when the fetch's future ends, whichever way it ends.
#[derive(Clone)]
pub struct Reserve(pub Arc<dyn Fn(usize, usize) -> ReserveFuture + Send + Sync>);

/// What a [`Reserve`] hook returns: completes once the bytes are reserved.
type ReserveFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

impl Reserve {
    pub fn reserve(&self, now: usize, total: usize) -> ReserveFuture {
        (self.0)(now, total)
    }
}

impl std::fmt::Debug for Reserve {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reserve")
    }
}

/// Pieces in the first request window: what a [`Reserve`] covers before
/// the peer has sent anything. 64 KiB holds the 10–60 KB dicts most
/// torrents have (architecture §3) whole, while a peer that announces a
/// size and sends nothing pins only this much (runner, 2026-11-07).
const FIRST_PIECES: usize = 4;
/// The dict bytes the first request window covers.
pub(crate) const FIRST_WINDOW: usize = FIRST_PIECES * METADATA_PIECE;

/// The heap bytes a [`Fetched`] can hold, for [`Reserve`]: the dict at
/// `dict` bytes, the client name, and a full PEX list.
pub(crate) fn result_bound(dict: usize, client: Option<&String>) -> usize {
    dict.saturating_add(client.map_or(0, String::len))
        .saturating_add(MAX_PEX * std::mem::size_of::<SocketAddr>())
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
            route_backoff: false,
            reserve: None,
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
    if opts.route_backoff && !opts.stats.routes.routed(addr, std::time::Instant::now()) {
        opts.stats.routes.skipped(route::family_index(addr));
        return Err(Error::Connect(io::Error::new(
            io::ErrorKind::NetworkUnreachable,
            "no local route to this address family",
        )));
    }
    let deadline = Instant::now() + opts.timeout;
    if opts.transport == Transport::Tcp {
        return fetch_via(Transport::Tcp, addr, info_hash, opts, deadline)
            .await
            .0;
    }
    // No uTP socket, none for this family (the DHT bound only the other
    // one), or no local route to it: straight to TCP rather than waiting
    // out a uTP connect that can't open.
    let utp_reaches = opts.utp.as_ref().is_some_and(|u| u.reaches(addr))
        && opts.stats.routes.routed(addr, std::time::Instant::now());
    if !utp_reaches && opts.fallback_to_tcp {
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
    let family = route::family_index(addr);
    // A uTP attempt counts once known not to have been refused by uTP's
    // own connection table (below), so a full table can't inflate the
    // uTP share.
    if transport == Transport::Tcp {
        opts.stats.attempted(transport, family);
    }
    let connected = AtomicBool::new(false);
    let connected = &connected;
    let run = |encrypted: bool| async move {
        timeout_at(
            deadline,
            attempt(
                transport, addr, &info_hash, opts, encrypted, connected, deadline,
            ),
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
    if transport == Transport::Utp && !utp_table_full(&r) {
        opts.stats.attempted(transport, family);
    }
    match &r {
        Ok(_) => opts.stats.succeeded(transport, family),
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

/// uTP refused the connect locally: its connection table (or the per-IP
/// share of it) is full. Nothing was sent.
fn utp_table_full(r: &Result<Fetched, Error>) -> bool {
    matches!(r, Err(Error::Connect(e)) if e.kind() == io::ErrorKind::QuotaExceeded)
}

async fn attempt(
    transport: Transport,
    addr: SocketAddr,
    info_hash: &InfoHash,
    opts: &FetchOptions,
    encrypted: bool,
    connected: &AtomicBool,
    deadline: Instant,
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
                .map_err(|e| {
                    if route::is_unreachable(&e) {
                        opts.stats.routes.unreachable(route::family_index(addr));
                    }
                    Error::Connect(e)
                })?;
            prepare_outbound(&tcp);
            connected.store(true, Ordering::Relaxed);
            exchange(BufReader::new(tcp), info_hash, opts, encrypted, deadline).await
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
            exchange(BufReader::new(stream), info_hash, opts, encrypted, deadline).await
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
/// The pace a large dict must keep ([`Pace`]) is measured against
/// `opts.timeout` from now.
pub async fn fetch_on_stream<S>(
    stream: S,
    info_hash: &InfoHash,
    opts: &FetchOptions,
    encrypted: bool,
) -> Result<Fetched, Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let deadline = Instant::now() + opts.timeout;
    exchange(stream, info_hash, opts, encrypted, deadline).await
}

/// The pace a dict larger than the first request window must keep once
/// that window is in and the rest is reserved: by then it holds room for
/// its whole announced size on its peer's word, so the peer has to be
/// sending it.
///
/// The time left to the fetch's deadline at that moment is cut into
/// [`PACE_PARTS`] parts. At the end of each part from the second, the peer
/// must have sent at least half of what an even pace would have brought by
/// then, less one piece for rounding. A peer that has sent less is ended
/// as a timeout, which gives its room back at once. No check comes in the
/// first part or in the first [`PACE_FREE`], whichever is longer, for a
/// transport or a peer that starts slowly; with less time than that left,
/// the deadline alone ends the fetch. A wait for room is never counted:
/// the clock starts when the rest is reserved.
#[derive(Debug)]
struct Pace {
    start: Instant,
    part: Duration,
    /// Dict bytes already in when the clock started, and those to come.
    base: usize,
    need: usize,
    /// The part whose end is checked next.
    next: u32,
}

const PACE_PARTS: u32 = 8;
/// The least time a peer has before its first pace check.
const PACE_FREE: Duration = Duration::from_secs(2);

impl Pace {
    /// `None` if the deadline has passed (it ends the fetch itself).
    fn start(now: Instant, deadline: Instant, got: usize, size: usize) -> Option<Pace> {
        let left = deadline
            .checked_duration_since(now)
            .filter(|d| !d.is_zero())?;
        let part = left / PACE_PARTS;
        // The first part to check: the second, or the first to end past
        // `PACE_FREE`.
        let next = if part.is_zero() {
            PACE_PARTS
        } else {
            let free = PACE_FREE.as_nanos().div_ceil(part.as_nanos());
            u32::try_from(free)
                .unwrap_or(PACE_PARTS)
                .clamp(2, PACE_PARTS)
        };
        Some(Pace {
            start: now,
            part,
            base: got,
            need: size.saturating_sub(got),
            next,
        })
    }

    /// When the next check is due. The last part ends at the deadline,
    /// which needs no check.
    fn next_check(&self) -> Option<Instant> {
        (self.next < PACE_PARTS).then(|| self.start + self.part * self.next)
    }

    /// Runs the checks due by `now` against the `got` dict bytes in so
    /// far. `false`: the peer is behind.
    fn keeps_up(&mut self, now: Instant, got: usize) -> bool {
        while let Some(at) = self.next_check() {
            if now < at {
                break;
            }
            let even = self.need as u128 * u128::from(self.next) / u128::from(PACE_PARTS);
            let sent = (got.saturating_sub(self.base) + METADATA_PIECE) as u128;
            if sent < even / 2 {
                return false;
            }
            self.next += 1;
        }
        true
    }
}

/// A write to the peer, which can't block past the next pace check or the
/// deadline: a peer that stops reading reaches its checks whatever we are
/// writing. `got` is the dict bytes in so far, for the checks.
async fn bounded<T>(
    write: impl Future<Output = io::Result<T>>,
    pace: &mut Option<Pace>,
    got: usize,
    deadline: Instant,
) -> Result<T, Error> {
    tokio::pin!(write);
    loop {
        let due = match pace.as_mut() {
            Some(p) => {
                if !p.keeps_up(Instant::now(), got) {
                    return Err(Error::Timeout);
                }
                p.next_check().unwrap_or(deadline)
            }
            None => deadline,
        };
        match timeout_at(due, &mut write).await {
            Ok(written) => return Ok(written?),
            Err(_) if due >= deadline => return Err(Error::Timeout),
            Err(_) => {}
        }
    }
}

/// [`fetch_on_stream`], against the fetch's own deadline.
async fn exchange<S>(
    mut stream: S,
    info_hash: &InfoHash,
    opts: &FetchOptions,
    encrypted: bool,
    deadline: Instant,
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
        bounded(stream.write_all(&hello), &mut None, 0, deadline).await?;
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
    bounded(
        wire.send_extended(0, &message::our_extended_handshake(None)),
        &mut None,
        0,
        deadline,
    )
    .await?;

    let mut their_metadata_id = None;
    let mut client = None;
    let mut pex = Vec::new();
    let mut assembly: Option<Assembly> = None;
    // The result's bound, while only the first window is reserved.
    let mut unreserved: Option<usize> = None;
    let mut pace: Option<Pace> = None;
    loop {
        let incoming = {
            let read = wire.read_message(MAX_EXTENDED, MAX_OTHER);
            tokio::pin!(read);
            loop {
                // Checked before every read as well as while one waits, so
                // a peer can't put the checks off by sending other things.
                let got = assembly.as_ref().map_or(0, |a| a.got);
                let due = match pace.as_mut() {
                    Some(p) => {
                        if !p.keeps_up(Instant::now(), got) {
                            return Err(Error::Timeout);
                        }
                        p.next_check()
                    }
                    None => None,
                };
                let Some(due) = due else {
                    break read.await?;
                };
                if let Ok(message) = timeout_at(due, &mut read).await {
                    break message?;
                }
            }
        };
        let payload = match incoming {
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
                // The first window's bytes are reserved before any is
                // requested, so a full budget pauses here, not after the
                // download. The rest waits until the peer has sent the window.
                if let Some(reserve) = &opts.reserve {
                    let total = result_bound(size, client.as_ref());
                    let now = result_bound(size.min(FIRST_WINDOW), client.as_ref());
                    reserve.reserve(now, total).await;
                    if now < total {
                        unreserved = Some(total);
                    }
                }
                // Its piece bitmap (1 B per 16 KiB piece, ≤ 1 KiB) sits
                // outside the reservation, in the socket's working memory.
                let mut a = Assembly::new(size);
                if unreserved.is_some() {
                    a.limit = FIRST_PIECES;
                }
                while let Some(piece) = a.next_request() {
                    let request = message::metadata_request(piece);
                    bounded(wire.send_extended(id, &request), &mut pace, a.got, deadline).await?;
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
                            debug_assert!(info.capacity() <= a.size, "grown past its size");
                            let v2 = verify(&info, info_hash)?;
                            // Exact capacities, so the result stays inside
                            // the bound reserved for it (`Reserve`).
                            return Ok(Fetched {
                                info,
                                v2,
                                encrypted: rc4,
                                pex: pex.into_boxed_slice().into_vec(),
                                client: client.map(|c| c.into_boxed_str().into_string()),
                            });
                        }
                        // Past the first window only once all of it has
                        // arrived and the whole result is reserved: a peer
                        // pins a large claim only by sending 64 KiB first.
                        if let (Some(total), Some(reserve)) = (unreserved, &opts.reserve) {
                            if a.in_flight > 0 {
                                continue;
                            }
                            reserve.reserve(total, total).await;
                            unreserved = None;
                            a.limit = usize::MAX;
                        }
                        // From here a large dict's peer must keep pace.
                        if pace.is_none() && a.window_is_in() {
                            pace = Pace::start(Instant::now(), deadline, a.got, a.size);
                        }
                        while let Some(piece) = a.next_request() {
                            let request = message::metadata_request(piece);
                            let send = wire.send_extended(their_id, &request);
                            bounded(send, &mut pace, a.got, deadline).await?;
                        }
                    }
                    MetadataMessage::Reject { .. } => return Err(Error::Rejected),
                    MetadataMessage::Request { piece } => {
                        // We serve nothing; say so politely, until the
                        // pace starts. From then on a request isn't
                        // answered, so a peer can't hold a fetch in our
                        // writes, where no pace is checked.
                        if pace.is_none() {
                            let piece = usize::try_from(piece).unwrap_or(0);
                            let reject = message::metadata_reject(piece);
                            bounded(
                                wire.send_extended(their_id, &reject),
                                &mut None,
                                0,
                                deadline,
                            )
                            .await?;
                        }
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
/// merely claims a large size costs nothing until it sends the bytes. It
/// never grows past `size`, the announced size a [`Reserve`] covered.
struct Assembly {
    size: usize,
    buf: Vec<u8>,
    received: Vec<bool>,
    remaining: usize,
    next: usize,
    in_flight: usize,
    /// No piece at or past this is requested (the first window, until a
    /// large dict's whole bound is reserved).
    limit: usize,
    /// Dict bytes received.
    got: usize,
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
            limit: usize::MAX,
            got: 0,
        }
    }

    /// Whether the dict is larger than the first request window and that
    /// whole window has arrived.
    fn window_is_in(&self) -> bool {
        self.received.len() > FIRST_PIECES && self.received[..FIRST_PIECES].iter().all(|r| *r)
    }

    fn piece_len(&self, piece: usize) -> usize {
        (self.size - piece * METADATA_PIECE).min(METADATA_PIECE)
    }

    fn next_request(&mut self) -> Option<usize> {
        if self.in_flight >= REQUEST_WINDOW || self.next >= self.received.len().min(self.limit) {
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
                // Doubling as `Vec` does, but never past the pieces
                // requested so far, which is what's reserved (the first
                // window until a large dict's growth, then the whole
                // size). `end` is within them: the piece was requested.
                if self.buf.capacity() < end {
                    let requested = (self.next * METADATA_PIECE).min(self.size);
                    let want = (self.buf.capacity() * 2).clamp(end, requested);
                    self.buf.reserve_exact(want - self.buf.len());
                }
                self.buf.resize(end, 0);
            }
            self.buf[start..end].copy_from_slice(data);
            self.received[piece] = true;
            self.got += data.len();
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

    const MIB: usize = 1 << 20;

    fn pace(need: usize) -> (Pace, Instant) {
        let t0 = Instant::now();
        let p = Pace::start(
            t0,
            t0 + Duration::from_secs(8),
            FIRST_WINDOW,
            FIRST_WINDOW + need,
        )
        .expect("time is left");
        (p, t0)
    }

    /// A peer that sends nothing more is behind at the first check, the
    /// end of the second part, and not before.
    #[tokio::test(start_paused = true)]
    async fn a_peer_under_half_pace_is_behind_at_the_next_check() {
        let (mut p, t0) = pace(MIB);
        assert!(p.keeps_up(t0 + Duration::from_millis(1_999), FIRST_WINDOW));
        assert!(!p.keeps_up(t0 + Duration::from_secs(2), FIRST_WINDOW));
        // Just under half of an even pace, less the piece allowed for
        // rounding, is behind; half is not.
        let half = MIB / 8;
        let (mut p, t0) = pace(MIB);
        let at = t0 + Duration::from_secs(2);
        assert!(!p.keeps_up(at, FIRST_WINDOW + half - METADATA_PIECE - 1));
        let (mut p, _) = pace(MIB);
        assert!(p.keeps_up(at, FIRST_WINDOW + half - METADATA_PIECE));
        // A check that comes late still counts every part passed.
        let (mut p, t0) = pace(MIB);
        assert!(!p.keeps_up(t0 + Duration::from_secs(7), FIRST_WINDOW + MIB / 4));
        let (mut p, t0) = pace(MIB);
        assert!(p.keeps_up(t0 + Duration::from_secs(7), FIRST_WINDOW + MIB / 2));
        assert_eq!(p.next_check(), None, "the deadline itself needs no check");
    }

    /// A peer that sends nothing for most of the first part and then runs
    /// fast enough to finish keeps pace throughout (a transport that
    /// starts slowly).
    #[tokio::test(start_paused = true)]
    async fn a_slow_start_keeps_pace() {
        for need in [MIB, 16 * MIB, 5 * METADATA_PIECE] {
            let (mut p, t0) = pace(need);
            // Silent for 0.9 s, then even over the next 4 s.
            let sent = |ms: u64| {
                let run = ms.saturating_sub(900).min(4_000) as u128;
                FIRST_WINDOW + (need as u128 * run / 4_000) as usize
            };
            for ms in (0..=8_000).step_by(50) {
                assert!(
                    p.keeps_up(t0 + Duration::from_millis(ms), sent(ms)),
                    "{need} bytes, behind at {ms} ms"
                );
            }
        }
    }

    /// Where half the pace is under a piece a part, rounding never ends a
    /// fetch: a peer exactly on an even pace, a piece at a time, keeps up,
    /// and so does one a whole piece behind it.
    #[tokio::test(start_paused = true)]
    async fn rounding_never_ends_a_fetch_on_pace() {
        for pieces in 1..=40usize {
            let need = pieces * METADATA_PIECE - 1;
            let (mut p, t0) = pace(need);
            for part in 1..=8u64 {
                // Whole pieces only, as they arrive.
                let even = need * part as usize / 8;
                let whole = even / METADATA_PIECE * METADATA_PIECE;
                let behind = whole.saturating_sub(METADATA_PIECE);
                assert!(
                    p.keeps_up(t0 + Duration::from_secs(part), FIRST_WINDOW + behind),
                    "{pieces} pieces, part {part}"
                );
            }
        }
        // With under two pieces to come, there is nothing to be behind on.
        let (mut p, t0) = pace(2 * METADATA_PIECE - 1);
        assert!(p.keeps_up(t0 + Duration::from_secs(8), FIRST_WINDOW));
    }

    /// No time left: no pace to keep (the deadline ends the fetch).
    #[tokio::test(start_paused = true)]
    async fn no_pace_starts_past_the_deadline() {
        let now = Instant::now();
        assert!(Pace::start(now, now, FIRST_WINDOW, MIB).is_none());
    }

    /// What a scripted peer does once it has served its pieces.
    #[derive(Clone, Copy, PartialEq)]
    enum Then {
        Silence,
        KeepAlives,
        /// Keep-alives as fast as the stream takes them, so one is always
        /// waiting to be read.
        Flood,
        /// Metadata requests in bursts, never reading what comes back.
        Requests,
        /// Neither reads nor sends any more, with the stream left open.
        Deaf,
        /// Serves every piece, the first past the first window after
        /// `first` ms (a round trip) and each after that `each` ms apart.
        Slow {
            first: u64,
            each: u64,
        },
    }

    /// A plaintext peer on `stream`: announces `info`'s size, serves the
    /// first `serve` pieces asked for, then goes on as `then` says.
    async fn scripted(
        mut stream: tokio::io::DuplexStream,
        info: Vec<u8>,
        serve: usize,
        then: Then,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let key: [u8; 20] = Sha1::digest(&info).into();
        let frame = |ext_id: u8, body: Vec<u8>| {
            let mut f = ((body.len() + 2) as u32).to_be_bytes().to_vec();
            f.extend([20, ext_id]);
            f.extend(body);
            f
        };
        let mut theirs = [0u8; HANDSHAKE_LEN];
        stream.read_exact(&mut theirs).await.unwrap();
        let hello = message::handshake(&key, b"-SC0001-scriptscript", true);
        stream.write_all(&hello).await.unwrap();
        let ext = message::our_extended_handshake(Some(info.len()));
        stream.write_all(&frame(0, ext)).await.unwrap();
        let mut served = 0;
        loop {
            if served >= serve && then == Then::Flood {
                let burst = vec![0u8; 64 << 10];
                while stream.write_all(&burst).await.is_ok() {}
                return;
            }
            if served >= serve && then == Then::Deaf {
                std::future::pending::<()>().await;
            }
            if served >= serve && then == Then::Requests {
                let request = frame(OUR_UT_METADATA, message::metadata_request(0));
                let burst: Vec<u8> = request.repeat(2_000);
                while stream.write_all(&burst).await.is_ok() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                return;
            }
            if served >= serve && then == Then::KeepAlives {
                // Other traffic, for as long as the fetch listens.
                loop {
                    if stream.write_all(&[0, 0, 0, 0]).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            let mut len = [0u8; 4];
            if stream.read_exact(&mut len).await.is_err() {
                return;
            }
            let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
            if stream.read_exact(&mut body).await.is_err() {
                return;
            }
            if body.len() < 2 || body[0] != 20 || body[1] != OUR_UT_METADATA || served >= serve {
                continue;
            }
            if let Ok(MetadataMessage::Request { piece }) =
                message::parse_metadata_message(&body[2..])
            {
                let start = piece as usize * METADATA_PIECE;
                let end = (start + METADATA_PIECE).min(info.len());
                let reply = message::metadata_data(piece as usize, info.len(), &info[start..end]);
                if let Then::Slow { first, each } = then
                    && served >= FIRST_PIECES
                {
                    let wait = if served == FIRST_PIECES { first } else { each };
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                }
                if stream
                    .write_all(&frame(OUR_UT_METADATA, reply))
                    .await
                    .is_err()
                {
                    return;
                }
                served += 1;
            }
        }
    }

    fn plain(timeout: Duration) -> FetchOptions {
        FetchOptions {
            timeout,
            encryption: Encryption::Disable,
            ..FetchOptions::default()
        }
    }

    /// A large dict's peer that serves the first window and then stops
    /// ends as a timeout at its first check, a quarter of the way to the
    /// deadline, whether it goes silent, sends other messages instead or
    /// asks us for metadata without reading the answers.
    #[tokio::test(start_paused = true)]
    async fn a_large_dicts_peer_that_stops_sending_ends_at_its_next_check() {
        for then in [Then::Silence, Then::KeepAlives, Then::Requests] {
            let info = vec![7u8; MIB];
            let key = InfoHash(Sha1::digest(&info).into());
            let (ours, theirs) = tokio::io::duplex(1 << 16);
            let peer = tokio::spawn(scripted(theirs, info, FIRST_PIECES, then));
            let started = Instant::now();
            let opts = plain(Duration::from_secs(8));
            let got = timeout(
                Duration::from_secs(8),
                fetch_on_stream(ours, &key, &opts, false),
            )
            .await
            .expect("it ends before the deadline");
            assert!(matches!(got, Err(Error::Timeout)), "{got:?}");
            let took = started.elapsed();
            assert!(
                took >= Duration::from_millis(1_900) && took <= Duration::from_millis(2_100),
                "ended after {took:?}"
            );
            peer.abort();
        }
    }

    /// A peer that floods other messages once the first window is in, so
    /// one is always waiting to be read, still ends at its first check:
    /// the pace is checked before every read, not only while one waits.
    /// Real time: a flood leaves a paused clock no idle moment to advance.
    #[tokio::test]
    async fn a_large_dicts_peer_that_floods_other_messages_ends_at_its_check() {
        let info = vec![7u8; MIB];
        let key = InfoHash(Sha1::digest(&info).into());
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let peer = tokio::spawn(scripted(theirs, info, FIRST_PIECES, Then::Flood));
        let started = Instant::now();
        let deadline = Duration::from_secs(8);
        let got = timeout(
            deadline * 2,
            fetch_on_stream(ours, &key, &plain(deadline), false),
        )
        .await
        .expect("it ends");
        assert!(matches!(got, Err(Error::Timeout)), "{got:?}");
        // The first check is a quarter of the way in, at two seconds; the
        // deadline is the slack either side.
        let took = started.elapsed();
        assert!(
            took < deadline * 3 / 4,
            "ended after {took:?}, near the deadline"
        );
        peer.abort();
    }

    /// No check comes in the first two seconds, however little time is
    /// left: with four seconds left the first is at two, and with two or
    /// less there are none.
    #[tokio::test(start_paused = true)]
    async fn the_first_check_leaves_two_seconds() {
        let t0 = Instant::now();
        let mut p = Pace::start(t0, t0 + Duration::from_secs(4), FIRST_WINDOW, MIB).unwrap();
        assert_eq!(p.next_check(), Some(t0 + PACE_FREE));
        assert!(p.keeps_up(t0 + Duration::from_millis(1_999), FIRST_WINDOW));
        assert!(!p.keeps_up(t0 + PACE_FREE, FIRST_WINDOW));
        for left in [2_000, 1_000, 10] {
            let p = Pace::start(t0, t0 + Duration::from_millis(left), FIRST_WINDOW, MIB).unwrap();
            assert_eq!(p.next_check(), None, "{left} ms left");
        }
    }

    /// A dict of one window or less has no pace to keep: its peer may
    /// stall to the deadline, holding only that window.
    #[tokio::test(start_paused = true)]
    async fn a_small_dicts_peer_is_left_to_the_deadline() {
        let info = vec![7u8; FIRST_WINDOW];
        let key = InfoHash(Sha1::digest(&info).into());
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let peer = tokio::spawn(scripted(theirs, info, 2, Then::Silence));
        let opts = plain(Duration::from_secs(8));
        let got = timeout(
            Duration::from_secs(8),
            fetch_on_stream(ours, &key, &opts, false),
        )
        .await;
        assert!(got.is_err(), "still fetching at the deadline: {got:?}");
        peer.abort();
    }

    /// A large dict served whole is fetched, pace or no pace, and a wait
    /// for the rest's room isn't counted against its peer.
    #[tokio::test(start_paused = true)]
    async fn a_wait_for_room_is_not_the_peers() {
        let info: Vec<u8> = (0..MIB).map(|i| (i % 251) as u8).collect();
        let key = InfoHash(Sha1::digest(&info).into());
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let peer = tokio::spawn(scripted(theirs, info.clone(), usize::MAX, Then::Silence));
        let mut opts = plain(Duration::from_secs(8));
        // The second reservation (the rest) waits 6 s for room.
        opts.reserve = Some(Reserve(Arc::new(|now, total| {
            Box::pin(async move {
                if now == total {
                    tokio::time::sleep(Duration::from_secs(6)).await;
                }
            })
        })));
        let got = timeout(
            Duration::from_secs(8),
            fetch_on_stream(ours, &key, &opts, false),
        )
        .await
        .expect("in time")
        .expect("fetched");
        assert_eq!(got.info, info);
        peer.abort();
    }

    /// A peer that stops reading while we send it our requests ends at
    /// its first check: a write can't hold the fetch past it.
    #[tokio::test(start_paused = true)]
    async fn a_peer_that_stops_reading_ends_at_its_check() {
        let info = vec![7u8; MIB];
        let key = InfoHash(Sha1::digest(&info).into());
        // Too small a pipe for all of the rest's requests.
        let (ours, theirs) = tokio::io::duplex(256);
        let peer = tokio::spawn(scripted(theirs, info, FIRST_PIECES, Then::Deaf));
        // Reserving, as a pool's fetches do: the rest is asked for once
        // the first window is in.
        let mut opts = plain(Duration::from_secs(8));
        opts.reserve = Some(Reserve(Arc::new(|_, _| Box::pin(async {}))));
        let started = Instant::now();
        let got = timeout(
            Duration::from_secs(8),
            fetch_on_stream(ours, &key, &opts, false),
        )
        .await
        .expect("it ends before the deadline");
        assert!(matches!(got, Err(Error::Timeout)), "{got:?}");
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(1_900) && took <= Duration::from_millis(2_100),
            "ended after {took:?}"
        );
        peer.abort();
    }

    /// An honest peer on a slow link with little time left finishes: with
    /// 2 s left after a wait for room, a peer 450 ms away sending 1 MB/s
    /// brings the rest of a 1 MiB dict in about 1.5 s, and no check comes
    /// to end it first.
    #[tokio::test(start_paused = true)]
    async fn a_slow_honest_peer_with_little_time_left_finishes() {
        let info: Vec<u8> = (0..MIB).map(|i| (i % 251) as u8).collect();
        let key = InfoHash(Sha1::digest(&info).into());
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let slow = Then::Slow {
            first: 450,
            each: 16,
        };
        let peer = tokio::spawn(scripted(theirs, info.clone(), usize::MAX, slow));
        let mut opts = plain(Duration::from_secs(8));
        opts.reserve = Some(Reserve(Arc::new(|now, total| {
            Box::pin(async move {
                if now == total {
                    tokio::time::sleep(Duration::from_secs(6)).await;
                }
            })
        })));
        let got = timeout(
            Duration::from_secs(8),
            fetch_on_stream(ours, &key, &opts, false),
        )
        .await
        .expect("in time")
        .expect("fetched");
        assert_eq!(got.info, info);
        peer.abort();
    }

    /// With `route_backoff` on, a family whose probe found no route isn't
    /// dialled: the fetch fails at once as unreachable, counts a skip and
    /// no attempt. Once the probe sees the route again, it dials.
    #[tokio::test]
    async fn an_unrouted_family_is_skipped_until_its_route_returns() {
        let opts = FetchOptions {
            route_backoff: true,
            connect_timeout: Duration::from_millis(1),
            timeout: Duration::from_millis(50),
            ..FetchOptions::default()
        };
        let v6: SocketAddr = "[2606:4700::1111]:6881".parse().unwrap();
        let now = std::time::Instant::now();
        assert!(!opts.stats.routes.routed_with(v6, now, |_| false));
        let r = fetch(v6, InfoHash([1; 20]), &opts).await;
        assert!(
            matches!(&r, Err(Error::Connect(e)) if e.kind() == io::ErrorKind::NetworkUnreachable),
            "{r:?}"
        );
        assert_eq!(opts.stats.routes.skips(1), 1);
        assert_eq!(opts.stats.family_attempts(1), 0);
        // The route is back: the next probe interval dials again.
        let later = now + crate::route::PROBE_EVERY;
        assert!(opts.stats.routes.routed_with(v6, later, |_| true));
        let _ = fetch(v6, InfoHash([1; 20]), &opts).await;
        assert_eq!(opts.stats.routes.skips(1), 1, "not skipped again");
        assert_eq!(opts.stats.family_attempts(1), 1);
    }

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

    /// The buffer grows as pieces arrive, in any order, but never past
    /// the pieces requested so far, which is what a `Reserve` covered.
    #[test]
    fn assembly_never_grows_past_what_was_requested() {
        // A large dict's first window out of order, with only that window
        // requested.
        let size = METADATA_PIECE * 1024;
        let mut a = Assembly::new(size);
        a.limit = FIRST_PIECES;
        let window: Vec<usize> = std::iter::from_fn(|| a.next_request()).collect();
        assert_eq!(window.len() * METADATA_PIECE, FIRST_WINDOW);
        for p in [3, 2, 0, 1] {
            a.accept(p as i64, &vec![1; METADATA_PIECE]).unwrap();
            assert!(
                a.buf.capacity() <= FIRST_WINDOW,
                "{p}: {}",
                a.buf.capacity()
            );
        }
        // Once grown, a full request window out of order: never past the
        // 64 pieces requested.
        let mut a = Assembly::new(size);
        let window: Vec<usize> = std::iter::from_fn(|| a.next_request()).collect();
        assert_eq!(window.len(), REQUEST_WINDOW);
        for p in [62, 63, 0, 2, 3, 6, 12, 24, 48, 61] {
            a.accept(p as i64, &vec![1; METADATA_PIECE]).unwrap();
            assert!(
                a.buf.capacity() <= REQUEST_WINDOW * METADATA_PIECE,
                "{p}: {}",
                a.buf.capacity()
            );
        }
    }

    #[test]
    fn assembly_never_grows_past_its_size() {
        for size in [
            1,
            METADATA_PIECE,
            METADATA_PIECE * 3 + 1,
            METADATA_PIECE * 37 + 5,
        ] {
            let mut a = Assembly::new(size);
            let pieces: Vec<usize> = std::iter::from_fn(|| a.next_request()).collect();
            // Last first, then the rest in order.
            let order = pieces.iter().rev().take(1).chain(pieces.iter()).copied();
            let requested = |a: &Assembly| (a.next * METADATA_PIECE).min(size);
            for p in order {
                a.accept(p as i64, &vec![1; a.piece_len(p)]).unwrap();
                assert!(
                    a.buf.capacity() <= requested(&a),
                    "{size}: {}",
                    a.buf.capacity()
                );
                while let Some(next) = a.next_request() {
                    a.accept(next as i64, &vec![2; a.piece_len(next)]).unwrap();
                    assert!(
                        a.buf.capacity() <= requested(&a),
                        "{size}: {}",
                        a.buf.capacity()
                    );
                }
            }
            assert!(a.is_complete());
            assert_eq!(a.buf.len(), size);
        }
    }

    #[test]
    fn the_result_bound_covers_the_name_and_a_full_pex_list() {
        let client = "x".repeat(300);
        assert_eq!(
            result_bound(1000, Some(&client)),
            1000 + 300 + MAX_PEX * std::mem::size_of::<SocketAddr>()
        );
        assert_eq!(result_bound(usize::MAX, None), usize::MAX);
        // The first window is exactly what's requested before any reply.
        let mut a = Assembly::new(FIRST_WINDOW * 3);
        a.limit = FIRST_PIECES;
        let requested: Vec<usize> = std::iter::from_fn(|| a.next_request()).collect();
        let first: usize = requested.iter().map(|&p| a.piece_len(p)).sum();
        assert_eq!(first, FIRST_WINDOW);
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
