//! The tokio layer: a socket that routes datagrams to connections, a timer
//! task per connection, and `UtpStream` (`AsyncRead + AsyncWrite`).
//!
//! The socket either owns its UDP socket ([`UtpSocket::bind`]) or shares
//! one with the DHT ([`UtpSocket::with_sink`]): the DHT keeps receiving,
//! hands uTP datagrams to [`UtpSocket::feed`], and uTP sends through the
//! same socket, so both use port 6881 (§5).
//!
//! Each connection's state sits behind a short-held mutex. Packets, timer
//! ticks and stream calls all go through `drive`, which applies the event,
//! sends whatever the connection wants to send, and wakes waiters. Sends
//! never block: a full socket buffer drops the packet and uTP retransmits.

use std::collections::HashMap;
use std::fmt;
use std::future::poll_fn;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use rand::RngExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Notify, mpsc};
use tokio::task::AbortHandle;

use crate::conn::{Closed, Connection, Limits, State};
use crate::packet::{self, PacketType};

/// Where a socket's outgoing datagrams go. Sends must not block; return
/// an error (the packet is dropped and later retransmitted) instead.
pub trait PacketSink: Send + Sync + 'static {
    fn send_to(&self, packet: &[u8], to: SocketAddr) -> io::Result<()>;

    /// Whether a send to `to`'s address family can go out at all, so a
    /// caller can skip uTP for a family with no socket instead of waiting
    /// for a connect that can't succeed.
    fn reaches(&self, _to: SocketAddr) -> bool {
        true
    }
}

/// A non-blocking std socket, such as the DHT's sending half. (Not a
/// tokio socket: its `try_send_to` refuses until the reactor has seen the
/// socket writable, which would drop the first SYN.)
impl PacketSink for std::net::UdpSocket {
    fn send_to(&self, packet: &[u8], to: SocketAddr) -> io::Result<()> {
        std::net::UdpSocket::send_to(self, packet, to).map(drop)
    }
}

/// Sends by address family, for a socket fed from the DHT's separate
/// IPv4 and IPv6 sockets. A family without a socket refuses the send.
#[derive(Clone, Default)]
pub struct DualSink {
    pub v4: Option<Arc<dyn PacketSink>>,
    pub v6: Option<Arc<dyn PacketSink>>,
}

impl fmt::Debug for DualSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DualSink")
            .field("v4", &self.v4.is_some())
            .field("v6", &self.v6.is_some())
            .finish()
    }
}

impl PacketSink for DualSink {
    fn send_to(&self, packet: &[u8], to: SocketAddr) -> io::Result<()> {
        let sink = if to.is_ipv4() { &self.v4 } else { &self.v6 };
        match sink {
            Some(s) => s.send_to(packet, to),
            None => Err(io::ErrorKind::AddrNotAvailable.into()),
        }
    }

    fn reaches(&self, to: SocketAddr) -> bool {
        if to.is_ipv4() {
            self.v4.as_ref().is_some_and(|s| s.reaches(to))
        } else {
            self.v6.as_ref().is_some_and(|s| s.reaches(to))
        }
    }
}

/// An owned socket. Peers are keyed by plain IPv4 addresses, which a
/// dual-stack IPv6 socket must send to in their mapped form.
struct OwnedSink {
    udp: std::net::UdpSocket,
    ipv6: bool,
}

impl PacketSink for OwnedSink {
    fn send_to(&self, packet: &[u8], to: SocketAddr) -> io::Result<()> {
        let to = match to {
            SocketAddr::V4(v4) if self.ipv6 => {
                SocketAddr::new(v4.ip().to_ipv6_mapped().into(), v4.port())
            }
            other => other,
        };
        self.udp.send_to(packet, to).map(drop)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Receive buffer per connection, advertised as the window.
    pub recv_window: u32,
    /// Send buffer per connection (unsent plus unacknowledged).
    pub send_buffer: usize,
    /// Connections at once, incoming and outgoing.
    pub max_connections: usize,
    /// Connections at once with one IP address, closing ones included.
    pub max_per_ip: usize,
    /// How long a dropped stream may take to finish closing.
    pub linger: Duration,
    /// An open connection with nothing in flight that hears nothing from
    /// the peer for this long is closed as timed out.
    pub idle_timeout: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            recv_window: 64 << 10,
            send_buffer: 64 << 10,
            // Closed connections linger briefly (TIME-WAIT), so this sits
            // well above the 400 concurrent fetches of §6.
            max_connections: 4096,
            max_per_ip: 8,
            linger: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(60),
        }
    }
}

/// Counters since the socket was created.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub packets_in: u64,
    pub packets_out: u64,
    /// Datagrams that didn't parse as uTP.
    pub invalid: u64,
    /// Packets for no connection we know (dropped silently).
    pub unknown: u64,
    /// Sends the socket refused (full buffer, unreachable).
    pub send_errors: u64,
    pub retransmits: u64,
    pub connects: u64,
    pub accepts: u64,
    /// Connections that died because the peer stopped answering.
    pub timeouts: u64,
    pub resets: u64,
}

#[derive(Debug, Default)]
struct Stats {
    packets_in: AtomicU64,
    packets_out: AtomicU64,
    invalid: AtomicU64,
    unknown: AtomicU64,
    send_errors: AtomicU64,
    retransmits: AtomicU64,
    connects: AtomicU64,
    accepts: AtomicU64,
    timeouts: AtomicU64,
    resets: AtomicU64,
}

fn bump(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Ordering::Relaxed);
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// (peer address, the connection id the peer sends to us with).
type Key = (SocketAddr, u16);

/// Live connections, and how many each IP has (closing ones included,
/// so a peer can't churn past the cap).
#[derive(Default)]
struct Table {
    cells: HashMap<Key, Arc<Cell>>,
    per_ip: HashMap<IpAddr, usize>,
}

enum Refused {
    Full,
    Taken,
}
use Refused::{Full, Taken};

struct Shared {
    sink: Arc<dyn PacketSink>,
    config: Config,
    epoch: Instant,
    local_addr: Option<SocketAddr>,
    conns: Mutex<Table>,
    listener: Mutex<Option<mpsc::Sender<UtpStream>>>,
    stats: Stats,
    recv_task: Mutex<Option<AbortHandle>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(t) = lock(&self.recv_task).take() {
            t.abort();
        }
    }
}

struct Cell {
    key: Key,
    inner: Mutex<Inner>,
    /// Wakes the timer task when its deadline moved earlier or the
    /// connection closed.
    timer: Notify,
}

struct Inner {
    conn: Connection,
    out: Vec<u8>,
    read_waker: Option<Waker>,
    write_waker: Option<Waker>,
    /// The deadline the timer task is sleeping until.
    armed: Option<Instant>,
    linger_until: Option<Instant>,
    counted_close: bool,
}

impl Inner {
    fn deadline(&self) -> Option<Instant> {
        match (self.conn.deadline(), self.linger_until) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// A uTP endpoint. Cloning is cheap and shares the socket.
#[derive(Clone)]
pub struct UtpSocket {
    shared: Arc<Shared>,
}

impl fmt::Debug for UtpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UtpSocket")
            .field("local_addr", &self.shared.local_addr)
            .field("connections", &self.connections())
            .finish()
    }
}

/// A non-blocking UDP socket on `addr`; dual-stack when `addr` is IPv6.
fn bind_udp(addr: SocketAddr) -> io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// IPv4-mapped IPv6 addresses (from dual-stack sockets) as plain IPv4, so
/// a peer has one key whichever way its packets arrive.
fn canonical(a: SocketAddr) -> SocketAddr {
    match a {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(v4.into(), v6.port()),
            None => a,
        },
        v4 => v4,
    }
}

impl UtpSocket {
    /// Binds its own UDP socket and receives on it.
    pub async fn bind(addr: SocketAddr) -> io::Result<UtpSocket> {
        UtpSocket::bind_with(addr, Config::default()).await
    }

    /// An IPv6 address binds a dual-stack socket, which also reaches IPv4
    /// peers through v4-mapped addresses. `IPV6_V6ONLY` is cleared
    /// explicitly because its default differs: off on Linux and macOS, on
    /// on Windows.
    pub async fn bind_with(addr: SocketAddr, config: Config) -> io::Result<UtpSocket> {
        let std = bind_udp(addr)?;
        let send = Arc::new(OwnedSink {
            udp: std.try_clone()?,
            ipv6: std.local_addr()?.is_ipv6(),
        });
        let udp = tokio::net::UdpSocket::from_std(std)?;
        let socket = UtpSocket::new(send, config, Some(udp.local_addr()?));
        let weak: Weak<Shared> = Arc::downgrade(&socket.shared);
        let task = tokio::spawn(async move {
            let mut buf = vec![0u8; 65_536];
            loop {
                // Errors are per datagram: on Windows an ICMP "port
                // unreachable" for an earlier send surfaces here as
                // ConnectionReset, and must not stop the loop.
                let Ok((n, from)) = udp.recv_from(&mut buf).await else {
                    // A socket nobody holds any more can't recover; stop.
                    if weak.strong_count() == 0 {
                        break;
                    }
                    tokio::task::yield_now().await;
                    continue;
                };
                let Some(shared) = weak.upgrade() else { break };
                UtpSocket { shared }.feed(&buf[..n], from);
            }
        });
        *lock(&socket.shared.recv_task) = Some(task.abort_handle());
        Ok(socket)
    }

    /// A socket that sends through `sink` and receives only what the
    /// owner of the UDP socket passes to [`feed`](Self::feed).
    pub fn with_sink(sink: Arc<dyn PacketSink>, config: Config) -> UtpSocket {
        UtpSocket::new(sink, config, None)
    }

    fn new(sink: Arc<dyn PacketSink>, config: Config, local_addr: Option<SocketAddr>) -> UtpSocket {
        UtpSocket {
            shared: Arc::new(Shared {
                sink,
                config,
                epoch: Instant::now(),
                local_addr,
                conns: Mutex::new(Table::default()),
                listener: Mutex::new(None),
                stats: Stats::default(),
                recv_task: Mutex::new(None),
            }),
        }
    }

    /// The bound address (sockets from [`bind`](Self::bind) only).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.shared.local_addr
    }

    /// Whether the socket can send to `to`'s address family (see
    /// [`PacketSink::reaches`]).
    pub fn reaches(&self, to: SocketAddr) -> bool {
        self.shared.sink.reaches(canonical(to))
    }

    pub fn connections(&self) -> usize {
        lock(&self.shared.conns).cells.len()
    }

    pub fn stats(&self) -> StatsSnapshot {
        let s = &self.shared.stats;
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        StatsSnapshot {
            packets_in: g(&s.packets_in),
            packets_out: g(&s.packets_out),
            invalid: g(&s.invalid),
            unknown: g(&s.unknown),
            send_errors: g(&s.send_errors),
            retransmits: g(&s.retransmits),
            connects: g(&s.connects),
            accepts: g(&s.accepts),
            timeouts: g(&s.timeouts),
            resets: g(&s.resets),
        }
    }

    /// Handles one datagram received on the shared socket. Returns whether
    /// it parsed as uTP. Must run inside a tokio runtime (an accepted
    /// connection spawns its timer).
    pub fn feed(&self, datagram: &[u8], from: SocketAddr) -> bool {
        let shared = &self.shared;
        let Ok(p) = packet::parse(datagram) else {
            bump(&shared.stats.invalid, 1);
            return false;
        };
        bump(&shared.stats.packets_in, 1);
        let from = canonical(from);
        let id = match p.header.ty {
            // A SYN names the id it wants replies on; it sends with id + 1.
            PacketType::Syn => p.header.conn_id.wrapping_add(1),
            _ => p.header.conn_id,
        };
        let cell = lock(&shared.conns).cells.get(&(from, id)).cloned();
        match cell {
            Some(cell) => shared.drive(&cell, true, |inner, now| inner.conn.on_packet(&p, now)),
            None if p.header.ty == PacketType::Syn => self.accept_syn(&p.header, (from, id)),
            None => bump(&shared.stats.unknown, 1),
        }
        true
    }

    fn accept_syn(&self, syn: &packet::Header, key: Key) {
        let shared = &self.shared;
        let Some(tx) = lock(&shared.listener).clone() else {
            bump(&shared.stats.unknown, 1);
            return;
        };
        if tx.capacity() == 0 {
            return; // backlog full: the peer retries its SYN
        }
        let now = Instant::now();
        let conn = Connection::accept(
            syn,
            rand::rng().random(),
            shared.limits(),
            shared.epoch,
            now,
        );
        let Ok(cell) = shared.insert(key, conn) else {
            return;
        };
        bump(&shared.stats.accepts, 1);
        let stream = UtpStream {
            shared: shared.clone(),
            cell: cell.clone(),
        };
        shared.drive(&cell, false, |_, _| ());
        // If the listener went away meanwhile, the stream's drop closes it.
        let _ = tx.try_send(stream);
    }

    /// Accepts incoming connections from now on, queueing up to `backlog`
    /// unaccepted ones. Without a listener, SYNs are ignored.
    pub fn listen(&self, backlog: usize) -> UtpListener {
        let (tx, rx) = mpsc::channel(backlog.max(1));
        *lock(&self.shared.listener) = Some(tx);
        UtpListener { rx }
    }

    /// Opens a connection. Bound the wait with a timeout: the SYN is
    /// retried with backoff for several seconds before failing.
    pub async fn connect(&self, to: SocketAddr) -> io::Result<UtpStream> {
        let shared = &self.shared;
        let to = canonical(to);
        let now = Instant::now();
        let cell = loop {
            // A random id; another connect may take it first, so retry.
            let id: u16 = rand::rng().random();
            let conn = Connection::connect(id, 1, shared.limits(), shared.epoch, now);
            match shared.insert((to, id), conn) {
                Ok(cell) => break cell,
                Err(Full) => {
                    return Err(io::Error::other(
                        "too many uTP connections (in all or to this IP)",
                    ));
                }
                Err(Taken) => continue,
            }
        };
        bump(&shared.stats.connects, 1);
        // Dropping this (a cancelled connect) abandons the connection.
        let stream = UtpStream {
            shared: shared.clone(),
            cell,
        };
        shared.drive(&stream.cell, false, |_, _| ());
        poll_fn(|cx| stream.poll_connected(cx)).await?;
        Ok(stream)
    }
}

impl Shared {
    fn limits(&self) -> Limits {
        Limits {
            recv_window: self.config.recv_window,
            send_buffer: self.config.send_buffer,
            idle_timeout: self.config.idle_timeout,
        }
    }

    /// Registers a connection and starts its timer.
    fn insert(self: &Arc<Self>, key: Key, conn: Connection) -> Result<Arc<Cell>, Refused> {
        let cell = Arc::new(Cell {
            key,
            inner: Mutex::new(Inner {
                conn,
                out: Vec::with_capacity(packet::HEADER_LEN + 34 + crate::conn::MSS),
                read_waker: None,
                write_waker: None,
                armed: None,
                linger_until: None,
                counted_close: false,
            }),
            timer: Notify::new(),
        });
        {
            let mut table = lock(&self.conns);
            if table.cells.contains_key(&key) {
                return Err(Taken);
            }
            let ip = key.0.ip();
            if table.cells.len() >= self.config.max_connections
                || table.per_ip.get(&ip).copied().unwrap_or(0) >= self.config.max_per_ip
            {
                return Err(Full);
            }
            table.cells.insert(key, cell.clone());
            *table.per_ip.entry(ip).or_default() += 1;
        }
        tokio::spawn(run_timer(self.clone(), cell.clone()));
        Ok(cell)
    }

    /// Applies `f`, sends what the connection wants to send, updates the
    /// counters and, for network events (`wake`), wakes the stream.
    fn drive<R>(&self, cell: &Cell, wake: bool, f: impl FnOnce(&mut Inner, Instant) -> R) -> R {
        let now = Instant::now();
        let (r, wakers, notify) = {
            let mut g = lock(&cell.inner);
            let inner = &mut *g;
            let r = f(inner, now);
            while inner.conn.poll_transmit(now, &mut inner.out) {
                match self.sink.send_to(&inner.out, cell.key.0) {
                    Ok(()) => bump(&self.stats.packets_out, 1),
                    Err(_) => bump(&self.stats.send_errors, 1),
                }
            }
            bump(
                &self.stats.retransmits,
                std::mem::take(&mut inner.conn.retransmits),
            );
            let closed = inner.conn.is_closed();
            if closed && !inner.counted_close {
                inner.counted_close = true;
                match inner.conn.state() {
                    State::Closed(Closed::TimedOut) => bump(&self.stats.timeouts, 1),
                    State::Closed(Closed::Reset) => bump(&self.stats.resets, 1),
                    _ => {}
                }
            }
            let notify = closed
                || match (inner.armed, inner.deadline()) {
                    (_, None) => false,
                    (None, Some(_)) => true,
                    (Some(armed), Some(d)) => d < armed,
                };
            let wakers = if wake {
                (inner.read_waker.take(), inner.write_waker.take())
            } else {
                (None, None)
            };
            (r, wakers, notify)
        };
        if let Some(w) = wakers.0 {
            w.wake();
        }
        if let Some(w) = wakers.1 {
            w.wake();
        }
        if notify {
            cell.timer.notify_one();
        }
        r
    }
}

/// Fires the connection's retransmission timer and its linger deadline,
/// and unregisters it once closed.
async fn run_timer(shared: Arc<Shared>, cell: Arc<Cell>) {
    loop {
        let deadline = {
            let mut g = lock(&cell.inner);
            if g.conn.is_closed() {
                break;
            }
            let d = g.deadline();
            g.armed = d;
            d
        };
        match deadline {
            Some(d) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(d.into()) => {}
                    _ = cell.timer.notified() => continue,
                }
            }
            None => {
                cell.timer.notified().await;
                continue;
            }
        }
        shared.drive(&cell, true, |inner, now| {
            if inner.linger_until.is_some_and(|t| now >= t) {
                inner.conn.expire();
            } else {
                inner.conn.on_timeout(now);
            }
        });
    }
    let mut table = lock(&shared.conns);
    if table
        .cells
        .get(&cell.key)
        .is_some_and(|c| Arc::ptr_eq(c, &cell))
    {
        table.cells.remove(&cell.key);
        let ip = cell.key.0.ip();
        if let Some(n) = table.per_ip.get_mut(&ip) {
            *n -= 1;
            if *n == 0 {
                table.per_ip.remove(&ip);
            }
        }
    }
}

/// Incoming connections, from [`UtpSocket::listen`].
#[derive(Debug)]
pub struct UtpListener {
    rx: mpsc::Receiver<UtpStream>,
}

impl UtpListener {
    pub async fn accept(&mut self) -> io::Result<UtpStream> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| io::Error::other("uTP socket closed"))
    }
}

/// One uTP connection as a byte stream. Dropping it closes the connection
/// gracefully in the background (bounded by [`Config::linger`]).
pub struct UtpStream {
    shared: Arc<Shared>,
    cell: Arc<Cell>,
}

impl fmt::Debug for UtpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UtpStream")
            .field("peer", &self.cell.key.0)
            .finish()
    }
}

impl UtpStream {
    pub fn peer_addr(&self) -> SocketAddr {
        self.cell.key.0
    }

    fn poll_connected(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut g = lock(&self.cell.inner);
        match g.conn.state() {
            State::Connected | State::SynRecv => Poll::Ready(Ok(())),
            State::Closed(Closed::Reset) => {
                Poll::Ready(Err(io::ErrorKind::ConnectionRefused.into()))
            }
            State::Closed(Closed::TimedOut) => Poll::Ready(Err(io::ErrorKind::TimedOut.into())),
            State::Closed(_) => Poll::Ready(Err(io::ErrorKind::NotConnected.into())),
            State::SynSent => {
                g.write_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    /// Aborts the connection with a RESET.
    pub fn reset(&self) {
        self.shared
            .drive(&self.cell, false, |inner, _| inner.conn.reset());
    }
}

impl AsyncRead for UtpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let r = this.shared.drive(&this.cell, false, |inner, _| {
            match inner.conn.read(buf.initialize_unfilled()) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    inner.read_waker = Some(cx.waker().clone());
                    None
                }
                other => Some(other),
            }
        });
        match r {
            None => Poll::Pending,
            Some(Ok(n)) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Some(Err(e)) => Poll::Ready(Err(e)),
        }
    }
}

impl AsyncWrite for UtpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let r = this
            .shared
            .drive(&this.cell, false, |inner, _| match inner.conn.write(data) {
                Ok(0) if !data.is_empty() => {
                    inner.write_waker = Some(cx.waker().clone());
                    None
                }
                other => Some(other),
            });
        match r {
            None => Poll::Pending,
            Some(r) => Poll::Ready(r),
        }
    }

    /// Bytes are handed to uTP as they're written, as with TCP.
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.shared
            .drive(&this.cell, false, |inner, now| inner.conn.close(now));
        Poll::Ready(Ok(()))
    }
}

impl Drop for UtpStream {
    fn drop(&mut self) {
        let linger = self.shared.config.linger;
        self.shared.drive(&self.cell, false, |inner, now| {
            inner.conn.abandon(now);
            inner.linger_until = Some(now + linger);
        });
    }
}
