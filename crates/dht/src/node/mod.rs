//! The DHT node runtime (architecture §5).
//!
//! One task owns everything: the sockets (one per address family), the
//! routing tables, the transaction table, lookups and rate limiters. It's
//! an actor fed by socket reads, a maintenance tick and a command channel,
//! so there are no locks; the outside world talks to it through [`Node`]
//! and reads counters from shared atomics ([`Stats`]).
//!
//! Defaults are conservative on purpose: DHT fan-out can exhaust a home
//! router's NAT table, so outbound queries are rate-limited and the number
//! outstanding is capped by the [`Intensity`] preset.

mod actor;
mod lookup;
mod tokens;

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::compact::NodeId;
use crate::state::StateError;

/// Crawl intensity presets (architecture §5: Low / Balanced / Aggressive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Intensity {
    Low,
    #[default]
    Balanced,
    Aggressive,
}

impl Intensity {
    /// Outbound queries per second.
    pub fn queries_per_sec(self) -> u32 {
        match self {
            Intensity::Low => 50,
            Intensity::Balanced => 200,
            Intensity::Aggressive => 1000,
        }
    }

    /// Cap on queries awaiting an answer: each is a NAT/conntrack entry on
    /// a home router.
    pub fn max_outstanding(self) -> usize {
        match self {
            Intensity::Low => 256,
            Intensity::Balanced => 1024,
            Intensity::Aggressive => 4096,
        }
    }
}

/// The public routers used when there's no saved state.
pub const DEFAULT_BOOTSTRAP: &[&str] = &[
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "router.utorrent.com:6881",
    "dht.libtorrent.org:25401",
];

#[derive(Debug, Clone)]
pub struct Config {
    /// One UDP socket per address; at most one IPv4 and one IPv6.
    pub bind: Vec<SocketAddr>,
    /// How many BEP 42 ids to present (architecture §5: 1–16, default 4).
    pub virtual_ids: usize,
    pub intensity: Intensity,
    /// `host:port` entries, resolved at start.
    pub bootstrap: Vec<String>,
    /// Where to keep `state.bin`; `None` disables persistence.
    pub state_path: Option<PathBuf>,
    pub query_timeout: Duration,
    /// Maintenance cadence (timeouts, refresh, pings).
    pub tick: Duration,
    pub save_every: Duration,
    /// BEP 43: don't answer queries and ask others not to route to us.
    pub read_only: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: vec![SocketAddr::from(([0, 0, 0, 0], 6881))],
            virtual_ids: 4,
            intensity: Intensity::default(),
            bootstrap: DEFAULT_BOOTSTRAP.iter().map(|s| s.to_string()).collect(),
            state_path: None,
            query_timeout: Duration::from_secs(4),
            tick: Duration::from_secs(1),
            save_every: Duration::from_secs(600),
            read_only: false,
        }
    }
}

/// Counters shared with the rest of the process; all monotonic except the
/// gauges noted.
#[derive(Debug, Default)]
pub struct Stats {
    pub packets_in: AtomicU64,
    pub packets_out: AtomicU64,
    pub malformed_in: AtomicU64,
    pub queries_in: AtomicU64,
    pub responses_in: AtomicU64,
    pub errors_in: AtomicU64,
    pub queries_out: AtomicU64,
    pub timeouts: AtomicU64,
    /// Queries not sent because of the rate limit or outstanding cap.
    pub throttled: AtomicU64,
    /// Replies not sent because of the reply rate limit.
    pub replies_dropped: AtomicU64,
    pub send_errors: AtomicU64,
    /// Responses from an address other than the one queried.
    pub spoofed: AtomicU64,
    /// Gauge: nodes across routing tables.
    pub routing_nodes: AtomicU64,
    /// Gauge: queries awaiting an answer.
    pub outstanding: AtomicU64,
}

impl Stats {
    pub(crate) fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        StatsSnapshot {
            packets_in: g(&self.packets_in),
            packets_out: g(&self.packets_out),
            malformed_in: g(&self.malformed_in),
            queries_in: g(&self.queries_in),
            responses_in: g(&self.responses_in),
            errors_in: g(&self.errors_in),
            queries_out: g(&self.queries_out),
            timeouts: g(&self.timeouts),
            throttled: g(&self.throttled),
            replies_dropped: g(&self.replies_dropped),
            send_errors: g(&self.send_errors),
            spoofed: g(&self.spoofed),
            routing_nodes: g(&self.routing_nodes),
            outstanding: g(&self.outstanding),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatsSnapshot {
    pub packets_in: u64,
    pub packets_out: u64,
    pub malformed_in: u64,
    pub queries_in: u64,
    pub responses_in: u64,
    pub errors_in: u64,
    pub queries_out: u64,
    pub timeouts: u64,
    pub throttled: u64,
    pub replies_dropped: u64,
    pub send_errors: u64,
    pub spoofed: u64,
    pub routing_nodes: u64,
    pub outstanding: u64,
}

impl StatsSnapshot {
    /// Responses per query sent. A collapsing ratio is the symptom of NAT
    /// saturation the dashboard warns about (§5).
    pub fn reply_ratio(&self) -> f64 {
        if self.queries_out == 0 {
            0.0
        } else {
            (self.responses_in + self.errors_in) as f64 / self.queries_out as f64
        }
    }
}

/// Point-in-time view of the node's identity and tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub ids: Vec<NodeId>,
    pub external_ip: Option<IpAddr>,
    /// Routing-table size per bound address, in `Config::bind` order.
    pub table_sizes: Vec<usize>,
    /// Every node in the routing tables.
    pub nodes: Vec<crate::compact::NodeInfo>,
    pub lookups: usize,
}

pub(crate) enum Command {
    Snapshot(oneshot::Sender<Snapshot>),
}

/// Binds a non-blocking UDP socket and returns it twice: as a tokio socket
/// for receiving, and as a std clone for sending. Sends never wait: a
/// `send_to` on a non-blocking UDP socket only fails when the kernel buffer
/// is full, whereas tokio's `try_send_to` also fails until the reactor has
/// seen the socket become writable, which drops a fresh node's first
/// (bootstrap) packets. IPv6 sockets are IPv6-only, so an IPv4 and an IPv6
/// socket can share a port.
fn bind(addr: SocketAddr) -> io::Result<(UdpSocket, std::net::UdpSocket)> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    // Room for bursts of replies; best effort, the OS may cap it.
    let _ = socket.set_recv_buffer_size(4 << 20);
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    let std_socket: std::net::UdpSocket = socket.into();
    let send = std_socket.try_clone()?;
    Ok((UdpSocket::from_std(std_socket)?, send))
}

/// Handle to a running node.
#[derive(Debug)]
pub struct Node {
    commands: mpsc::Sender<Command>,
    shutdown: watch::Sender<bool>,
    stats: Arc<Stats>,
    local_addrs: Vec<SocketAddr>,
    task: JoinHandle<Result<(), StateError>>,
}

impl Node {
    /// Binds the sockets (so address errors surface here) and starts the
    /// node in the background.
    pub async fn start(config: Config) -> io::Result<Node> {
        if config.bind.is_empty() || config.bind.len() > 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bind one IPv4 and/or one IPv6 address",
            ));
        }
        if config.bind.len() == 2 && config.bind[0].is_ipv6() == config.bind[1].is_ipv6() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "two sockets of the same address family",
            ));
        }
        let mut sockets = Vec::new();
        for addr in &config.bind {
            sockets.push(bind(*addr)?);
        }
        let local_addrs = sockets
            .iter()
            .map(|(recv, _)| recv.local_addr())
            .collect::<io::Result<Vec<_>>>()?;
        let stats = Arc::new(Stats::default());
        let (commands, command_rx) = mpsc::channel(64);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(actor::run(
            config,
            sockets,
            stats.clone(),
            command_rx,
            shutdown_rx,
        ));
        Ok(Node {
            commands,
            shutdown,
            stats,
            local_addrs,
            task,
        })
    }

    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.local_addrs
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    /// `None` if the node has stopped.
    pub async fn snapshot(&self) -> Option<Snapshot> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(Command::Snapshot(tx)).await.ok()?;
        rx.await.ok()
    }

    /// Stops the node, saving its state when persistence is on.
    pub async fn shutdown(self) -> Result<(), StateError> {
        let _ = self.shutdown.send(true);
        match self.task.await {
            Ok(result) => result,
            Err(e) => Err(StateError::Io(io::Error::other(e))),
        }
    }
}
