//! The coordinator's cluster port and its sessions (§19.2, §19.3, §19.6).
//!
//! **Before authentication** a connection can cost the coordinator only a
//! bounded amount:
//! - at most [`ServerConfig::max_pending`] connections are in their
//!   handshake at once, and [`ServerConfig::max_pending_per_ip`] from one
//!   address (IPv6 by /64); the excess is closed as soon as it's accepted;
//! - the whole handshake (TLS, then the WebSocket upgrade, or the join
//!   exchange) must finish within [`ServerConfig::handshake_timeout`];
//! - the buffers involved are rustls' (a ClientHello is at most 64 KiB) and,
//!   on a join connection, one request of at most
//!   [`MAX_JOIN_REQUEST`](crate::frame::MAX_JOIN_REQUEST) bytes.
//!
//! A cluster connection is identified ([`CoordinatorTls::identify`]: an
//! issued, unrevoked, unexpired certificate) right after the TLS handshake,
//! before the WebSocket upgrade request or any message is read. A join
//! connection may only redeem one token.
//!
//! **After authentication** each connection is one task with bounded
//! queues. No lock is held across an `.await`: the fleet state (leases,
//! sessions, clock offsets) is behind a mutex taken only for synchronous
//! work, and the PKI (the store and the SQLite work it does) only inside
//! `spawn_blocking`.
//!
//! **Every heartbeat tick** ([`ClusterServer::tick`]) the server
//! - reloads the PKI from the store, so a revocation made by any process
//!   reaches it, and runs `maintain` once an hour;
//! - stops every session whose certificate no longer stands (revocation
//!   closes the connection within one heartbeat, §19.8) and returns its
//!   leases, and every session silent for three heartbeats;
//! - expires leases of workers silent past the TTL and grants the keys to
//!   live workers under new fencing tokens.
//!
//! **Leases** live in a [`LeaseAuthority`] the server shares with the
//! pipeline ([`ServerHooks::authority`], WK-0): the pipeline builds it for
//! its built-in worker and hands an `Arc` to the server only when remote
//! workers are allowed, so a key is never leased twice and a single-machine
//! install opens no cluster CA. Lock order: the fleet, then the authority.
//!
//! **Hooks** for the slices that build on the protocol: an
//! [`InboundSink`] for what workers send (results, counters, outcomes,
//! snapshot acknowledgements, re-verification replies, hints for the
//! built-in worker), a [`GrantAdmission`] check for backpressure, and
//! [`ClusterServer::send`] / [`ClusterServer::broadcast`]. Each session's
//! outgoing queue is capped by count and by bytes
//! ([`ServerConfig::session_queue_bytes`]).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lodestar_integrations::SecretStore;
use lodestar_metainfo::InfoHash;
use lodestar_store::{SourceId, Store};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::time::timeout;
use tokio_rustls::LazyConfigAcceptor;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use zeroize::Zeroizing;

use crate::clock::{Clock, ClockTime};
use crate::frame::{self, MAX_JOIN_REQUEST};
use crate::link::{BoxIo, CoordinatorLink, Link, WorkerLink, local_pair, ws_config};
use crate::proto::*;
use crate::{
    ALPN_JOIN, BatchAnswer, Claim, Claimed, ClusterError, Coordinator, CoordinatorTls, JoinToken,
    Lease, LeaseAuthority, NoEvents, PeerIdentity, QUEUE_BYTES, Token, WorkerId,
};

/// The WebSocket path on the cluster port.
pub const CLUSTER_PATH: &str = "/cluster";

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// §19.2: every 5 s.
    pub heartbeat: Duration,
    /// A session silent for this many heartbeats is closed.
    pub heartbeat_timeout_beats: u32,
    /// §19.3: 10 minutes.
    pub lease_ttl_secs: i64,
    pub handshake_timeout: Duration,
    /// Connections still in their handshake, in total and from one address.
    pub max_pending: usize,
    pub max_pending_per_ip: usize,
    /// Messages queued to one session.
    pub session_queue: usize,
    /// Bytes queued to one session (plan Q5: 8 MiB). A message that
    /// doesn't fit is refused ([`SendError::Full`]); one larger than the
    /// cap goes only into an empty queue.
    pub session_queue_bytes: usize,
    /// §19.3: a worker more than 60 s off is flagged.
    pub max_skew_ms: i64,
    pub maintain_every_secs: i64,
    /// Tick every heartbeat by itself. Tests turn it off and call
    /// [`ClusterServer::tick`].
    pub auto_tick: bool,
}

impl Default for ServerConfig {
    fn default() -> ServerConfig {
        ServerConfig {
            heartbeat: Duration::from_secs(5),
            heartbeat_timeout_beats: 3,
            lease_ttl_secs: 600,
            handshake_timeout: Duration::from_secs(10),
            max_pending: 256,
            max_pending_per_ip: 8,
            session_queue: 256,
            session_queue_bytes: QUEUE_BYTES,
            max_skew_ms: 60_000,
            maintain_every_secs: 3600,
            auto_tick: true,
        }
    }
}

/// Whether keys are already in the store (the authoritative check before a
/// grant, §19.3). It runs on a blocking thread.
pub trait StoredKeys: Send + Sync + 'static {
    fn stored(&self, keys: &[InfoHash]) -> Vec<bool>;
}

/// Where the server hands what workers send. Every method runs on the
/// session's task with no lock held, so it must not block: queue the work
/// (a bounded channel's `try_send`) and return. By default results and
/// counters are refused (never acknowledged and lost) and the rest dropped.
pub trait InboundSink: Send + Sync + 'static {
    /// A `Results` batch. `false` refuses it for now (backpressure): the
    /// worker is told to send it again later.
    fn results(&self, _worker: WorkerId, _batch: Payload) -> bool {
        false
    }
    /// A `Counters` batch, as [`results`](Self::results).
    fn counters(&self, _worker: WorkerId, _batch: Payload) -> bool {
        false
    }
    /// Outcomes under the holder's current token (stale ones never get
    /// here). `PeerFound` and `Lost` ended the lease; `Fetched` didn't: the
    /// commit of its result ends it.
    fn outcomes(&self, _worker: WorkerId, _accepted: Vec<(InfoHash, Lease, Outcome)>) {}
    fn snapshot_ack(&self, _worker: WorkerId, _ack: SnapshotAck) {}
    fn reverified(&self, _worker: WorkerId, _reply: ReverifyReply) {}
    /// Hints a remote worker found for keys the built-in worker holds.
    /// In memory only (§18.7).
    fn local_hints(&self, _hints: Vec<HintItem>) {}
}

/// Whether grants may go out now (§19.5 backpressure): of `wanted` keys
/// that would get a new lease (keys already held don't count), how many to
/// grant. The rest are denied as [`DenyReason::Busy`]. It runs under the
/// fleet and lease locks, so it must only read a number.
pub trait GrantAdmission: Send + Sync + 'static {
    fn admit(&self, worker: WorkerId, wanted: usize) -> usize;
}

/// What the server shares with the rest of the coordinator.
#[derive(Clone)]
pub struct ServerHooks {
    /// The pipeline's lease authority.
    pub authority: Arc<LeaseAuthority>,
    pub stored: Option<Arc<dyn StoredKeys>>,
    pub sink: Option<Arc<dyn InboundSink>>,
    pub admission: Option<Arc<dyn GrantAdmission>>,
}

impl std::fmt::Debug for ServerHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHooks")
            .field("authority", &self.authority)
            .field("stored", &self.stored.is_some())
            .field("sink", &self.sink.is_some())
            .field("admission", &self.admission.is_some())
            .finish()
    }
}

/// Why [`ClusterServer::send`] didn't queue a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The worker has no session.
    NotConnected,
    /// The session's queue is at its count or byte cap.
    Full,
    /// Over a protocol limit, or a message only the server sends.
    Invalid,
}

/// What the PKI needs: its state, the database and the secrets key.
#[derive(Debug)]
pub struct Pki {
    pub coordinator: Coordinator,
    pub store: Store,
    pub secrets: SecretStore,
}

#[derive(Debug)]
struct PkiState {
    pki: Pki,
    last_maintain: i64,
}

impl PkiState {
    fn refresh(&mut self, now: i64, maintain_every: i64) -> Result<(), ClusterError> {
        let Pki {
            coordinator,
            store,
            secrets,
        } = &mut self.pki;
        coordinator.reload(store, secrets)?;
        if now - self.last_maintain >= maintain_every {
            coordinator.maintain(store, secrets, now)?;
            self.last_maintain = now;
        }
        Ok(())
    }
}

/// A message queued to a session, holding its share of the byte cap.
type Queued = (ToWorker, OwnedSemaphorePermit);

struct SessionSlot {
    id: u64,
    ctl: mpsc::Sender<Queued>,
    bytes: Arc<Semaphore>,
    budget: usize,
    stop: mpsc::Sender<StopReason>,
    /// A drain request, ahead of everything queued: it can't be crowded out.
    drain: mpsc::Sender<()>,
}

impl SessionSlot {
    /// Queues a best-effort message (hints) only while the queue is under
    /// half its count and byte caps, so it never crowds out grants or
    /// snapshot chunks.
    fn try_send_spare(&self, msg: ToWorker) -> bool {
        let half_bytes = self.bytes.available_permits() >= self.budget / 2;
        let half_count = self.ctl.capacity() >= self.ctl.max_capacity() / 2;
        half_bytes && half_count && self.try_send(msg).is_ok()
    }

    fn stop(&self, reason: StopReason) {
        // Capacity one: if it's full, a stop is already on its way.
        let _ = self.stop.try_send(reason);
    }

    /// Queues a message if it fits under both caps. Never waits.
    fn try_send(&self, msg: ToWorker) -> Result<(), SendError> {
        let need = u32::try_from(msg.approx_size().min(self.budget)).unwrap_or(u32::MAX);
        let permit = self
            .bytes
            .clone()
            .try_acquire_many_owned(need)
            .map_err(|_| SendError::Full)?;
        self.ctl.try_send((msg, permit)).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => SendError::Full,
            mpsc::error::TrySendError::Closed(_) => SendError::NotConnected,
        })
    }
}

struct WorkerState {
    session: Option<SessionSlot>,
    identity: Option<PeerIdentity>,
    last_seen_ms: i64,
    offset_ms: i64,
    skewed: bool,
    draining: bool,
    stats: Stats,
    capabilities: Capabilities,
    /// The last acknowledgement sent: its sequence number and when.
    last_ack: Option<(u64, i64)>,
}

struct Fleet {
    workers: HashMap<WorkerId, WorkerState>,
}

impl Fleet {
    /// Whether `peer`'s session is still its worker's registered one.
    fn is_current(&self, peer: &Peer) -> bool {
        self.workers
            .get(&peer.worker)
            .and_then(|s| s.session.as_ref())
            .is_some_and(|s| s.id == peer.session)
    }
}

#[derive(Default)]
struct Gate {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
}

#[derive(Debug, Default)]
struct Counters {
    refused: AtomicU64,
    handshake_timeouts: AtomicU64,
    handshake_failures: AtomicU64,
    join_requests: AtomicU64,
    sessions: AtomicU64,
    revoked: AtomicU64,
    protocol_errors: AtomicU64,
    version_refused: AtomicU64,
    harvested: AtomicU64,
    outcomes: AtomicU64,
    stale_reports: AtomicU64,
    regrants: AtomicU64,
    busy: AtomicU64,
    hints_relayed: AtomicU64,
    inbound_refused: AtomicU64,
    fetched: AtomicU64,
    skew_flags: AtomicU64,
    pki_errors: AtomicU64,
    ticks: AtomicU64,
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

/// One worker as Settings → Workers will show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerStatus {
    pub worker: WorkerId,
    pub connected: bool,
    pub draining: bool,
    pub offset_ms: i64,
    pub skewed: bool,
    pub leases: usize,
    pub last_seen_ms: i64,
    pub stats: Stats,
}

/// A consistent view of the fleet and the server's counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSnapshot {
    pub workers: Vec<WorkerStatus>,
    /// Leases held now.
    pub leases: usize,
    /// Keys whose lease ended without an outcome, not yet granted again.
    pub pending: usize,
    /// Connections in their handshake.
    pub handshaking: usize,
    /// Keys granted for the first time ("harvested", §19.3).
    pub harvested: u64,
    /// Terminal outcomes accepted (losses; they end the lease).
    pub outcomes: u64,
    /// `Fetched` reports accepted (the lease stays until the commit).
    pub fetched: u64,
    /// Reports with a stale fencing token, ignored.
    pub stale_reports: u64,
    pub regrants: u64,
    /// Claimed keys denied because grants were held back (§19.5).
    pub busy: u64,
    /// Keys whose peer hints were relayed to their holder.
    pub hints_relayed: u64,
    /// `Results` and `Counters` batches the sink refused for now.
    pub inbound_refused: u64,
    pub refused: u64,
    pub handshake_timeouts: u64,
    pub handshake_failures: u64,
    pub join_requests: u64,
    pub sessions: u64,
    pub revoked: u64,
    pub protocol_errors: u64,
    pub version_refused: u64,
    pub skew_flags: u64,
    pub pki_errors: u64,
    pub ticks: u64,
}

/// Who is on the other end of a session.
struct Peer {
    worker: WorkerId,
    /// `None` for the built-in local worker, which has no certificate.
    identity: Option<PeerIdentity>,
    /// Set once the session is registered. Claims, outcomes and heartbeats
    /// count only while it is still the worker's current session.
    session: u64,
}

/// The coordinator's cluster server: the listener, every session, the
/// lease table and the PKI.
pub struct ClusterServer {
    config: ServerConfig,
    clock: Arc<dyn Clock>,
    tls: CoordinatorTls,
    pki: Arc<Mutex<PkiState>>,
    fleet: Mutex<Fleet>,
    leases: Arc<LeaseAuthority>,
    gate: Mutex<Gate>,
    stored: Option<Arc<dyn StoredKeys>>,
    sink: Option<Arc<dyn InboundSink>>,
    admission: Option<Arc<dyn GrantAdmission>>,
    counters: Counters,
    next_session: AtomicU64,
    shutdown: watch::Sender<bool>,
}

impl std::fmt::Debug for ClusterServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterServer").finish_non_exhaustive()
    }
}

/// A slot among the connections in their handshake, given back on drop.
struct Permit {
    server: Arc<ClusterServer>,
    ip: IpAddr,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut gate = lock(&self.server.gate);
        gate.total -= 1;
        if let Some(n) = gate.per_ip.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                gate.per_ip.remove(&self.ip);
            }
        }
    }
}

enum Accepted {
    /// A join connection whose request has been read in full.
    Join(BoxIo, Zeroizing<Vec<u8>>),
    Cluster(CoordinatorLink, PeerIdentity),
}

/// What a message handler tells the session loop.
enum Flow {
    Continue,
    Stop(StopReason),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reads a join connection's one request: a big-endian `u32` length (at
/// most [`MAX_JOIN_REQUEST`]) and the frame.
async fn read_join(tls: &mut BoxIo) -> Option<Zeroizing<Vec<u8>>> {
    let len = usize::try_from(tls.read_u32().await.ok()?).ok()?;
    if len > MAX_JOIN_REQUEST {
        return None;
    }
    let mut buf = Zeroizing::new(vec![0u8; len]);
    tls.read_exact(&mut buf).await.ok()?;
    Some(buf)
}

async fn write_join(mut tls: BoxIo, reply: &JoinReply) {
    let Ok(bytes) = frame::encode(reply) else {
        return;
    };
    let Ok(len) = u32::try_from(bytes.len()) else {
        return;
    };
    let _ = tls.write_u32(len).await;
    let _ = tls.write_all(&bytes).await;
    let _ = tls.shutdown().await;
}

/// One address's key for the per-IP cap: IPv4 as is, IPv6 by its /64 (one
/// host usually has a whole /64).
fn ip_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
    }
}

/// A peer hint worth relaying: a real unicast address with a port.
fn usable_hint(a: &std::net::SocketAddr) -> bool {
    let ip = a.ip();
    a.port() != 0
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && ip != IpAddr::V4(std::net::Ipv4Addr::BROADCAST)
}

fn clamp(v: i128) -> i64 {
    i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}

impl ClusterServer {
    /// A server with a lease authority of its own and no accounting (for a
    /// server on its own; the coordinator uses [`with_hooks`](Self::with_hooks)).
    pub fn new(
        pki: Pki,
        clock: Arc<dyn Clock>,
        config: ServerConfig,
        stored: Option<Arc<dyn StoredKeys>>,
    ) -> Result<Arc<ClusterServer>, ClusterError> {
        // The lease epoch is the start time, so tokens never repeat across
        // restarts (leases.rs).
        let epoch = u32::try_from(clock.now_secs().rem_euclid(1 << 32)).unwrap_or(0);
        let authority = Arc::new(LeaseAuthority::new(
            epoch,
            config.lease_ttl_secs,
            Arc::new(NoEvents),
        ));
        let hooks = ServerHooks {
            authority,
            stored,
            sink: None,
            admission: None,
        };
        ClusterServer::with_hooks(pki, clock, config, hooks)
    }

    /// A server on the coordinator's lease authority, with its hooks.
    /// Building one needs the PKI, so call it only once workers are
    /// allowed (§19.6: a single-machine install never opens a cluster CA).
    /// The lease TTL is the authority's, whatever `config` says, so workers
    /// are told the TTL that's enforced.
    pub fn with_hooks(
        pki: Pki,
        clock: Arc<dyn Clock>,
        mut config: ServerConfig,
        hooks: ServerHooks,
    ) -> Result<Arc<ClusterServer>, ClusterError> {
        config.lease_ttl_secs = hooks.authority.ttl_secs();
        hooks.authority.serve_remote();
        let tls = pki
            .coordinator
            .tls()
            .with_time_provider(Arc::new(ClockTime(clock.clone())))?;
        let now = clock.now_secs();
        Ok(Arc::new(ClusterServer {
            fleet: Mutex::new(Fleet {
                workers: HashMap::new(),
            }),
            leases: hooks.authority,
            sink: hooks.sink,
            admission: hooks.admission,
            config,
            clock,
            tls,
            pki: Arc::new(Mutex::new(PkiState {
                pki,
                // `Coordinator::open` just ran `maintain`.
                last_maintain: now,
            })),
            gate: Mutex::new(Gate::default()),
            stored: hooks.stored,
            counters: Counters::default(),
            next_session: AtomicU64::new(1),
            shutdown: watch::channel(false).0,
        }))
    }

    /// Runs `f` on the PKI on a blocking thread.
    async fn with_pki<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut PkiState) -> R + Send + 'static,
    ) -> Option<R> {
        let pki = self.pki.clone();
        tokio::task::spawn_blocking(move || f(&mut lock(&pki)))
            .await
            .ok()
    }

    /// A one-time join token (§19.6), for Settings → Workers → Add worker.
    pub async fn create_join_token(&self, name: &str) -> Result<JoinToken, ClusterError> {
        let name = name.to_owned();
        let now = self.clock.now_secs();
        self.with_pki(move |p| {
            p.pki.coordinator.reload(&p.pki.store, &p.pki.secrets)?;
            let Pki {
                coordinator, store, ..
            } = &mut p.pki;
            coordinator.create_join_token(store, &name, now)
        })
        .await
        .unwrap_or(Err(ClusterError::Corrupt("the PKI task failed")))
    }

    /// Revokes a worker: its certificates are refused from now on, its
    /// connection is closed with [`StopReason::Revoked`], and its leases are
    /// granted to other workers. Returns how many certificates it had.
    pub async fn revoke(&self, worker: WorkerId) -> Result<usize, ClusterError> {
        let n = self
            .with_pki(move |p| {
                let Pki {
                    coordinator, store, ..
                } = &mut p.pki;
                coordinator.revoke(store, worker)
            })
            .await
            .unwrap_or(Err(ClusterError::Corrupt("the PKI task failed")))?;
        let mut fleet = lock(&self.fleet);
        // Connected or not, its leases go back now (§19.6).
        self.leases.evict(worker);
        self.sweep(&mut fleet, self.clock.now_ms());
        Ok(n)
    }

    /// Asks a worker to drain: it gets no new leases, finishes the ones it
    /// holds, says `Drained` and is stopped. The drain stands across
    /// reconnects (it's asked for again after `Welcome`) until
    /// [`resume`](Self::resume). `false` if the worker isn't connected: a
    /// drain is a request the worker has to act on, so it needs a session
    /// to go to (to stop a disconnected worker for good, revoke it).
    pub fn drain(&self, worker: WorkerId) -> bool {
        let mut fleet = lock(&self.fleet);
        let Some(state) = fleet.workers.get_mut(&worker) else {
            return false;
        };
        let Some(slot) = &state.session else {
            return false;
        };
        state.draining = true;
        // Full means a drain request is already on its way.
        let _ = slot.drain.try_send(());
        true
    }

    /// Ends a drain: the worker may claim again, and keys waiting for a
    /// worker may be granted to it at once. Unlike [`drain`](Self::drain)
    /// it works on a disconnected worker too, so a drain can be lifted
    /// before the worker comes back. Nothing is sent to the worker: it
    /// simply stops getting `Draining` denials. `false` if it wasn't
    /// draining.
    pub fn resume(&self, worker: WorkerId) -> bool {
        let mut fleet = lock(&self.fleet);
        let Some(state) = fleet.workers.get_mut(&worker) else {
            return false;
        };
        if !std::mem::replace(&mut state.draining, false) {
            return false;
        }
        self.regrant(&mut fleet, self.clock.now_secs());
        true
    }

    pub fn snapshot(&self) -> FleetSnapshot {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let fleet = lock(&self.fleet);
        let ids: Vec<WorkerId> = fleet.workers.keys().copied().collect();
        let held = self.leases.held_by_each(&ids);
        let mut workers: Vec<WorkerStatus> = ids
            .iter()
            .zip(held)
            .map(|(worker, leases)| {
                let s = &fleet.workers[worker];
                WorkerStatus {
                    worker: *worker,
                    connected: s.session.is_some(),
                    draining: s.draining,
                    offset_ms: s.offset_ms,
                    skewed: s.skewed,
                    leases,
                    last_seen_ms: s.last_seen_ms,
                    stats: s.stats,
                }
            })
            .collect();
        workers.sort_by_key(|w| w.worker);
        FleetSnapshot {
            workers,
            leases: self.leases.len(),
            pending: self.leases.pending(),
            handshaking: lock(&self.gate).total,
            harvested: get(&c.harvested),
            outcomes: get(&c.outcomes),
            stale_reports: get(&c.stale_reports),
            regrants: get(&c.regrants),
            busy: get(&c.busy),
            fetched: get(&c.fetched),
            hints_relayed: get(&c.hints_relayed),
            inbound_refused: get(&c.inbound_refused),
            refused: get(&c.refused),
            handshake_timeouts: get(&c.handshake_timeouts),
            handshake_failures: get(&c.handshake_failures),
            join_requests: get(&c.join_requests),
            sessions: get(&c.sessions),
            revoked: get(&c.revoked),
            protocol_errors: get(&c.protocol_errors),
            version_refused: get(&c.version_refused),
            skew_flags: get(&c.skew_flags),
            pki_errors: get(&c.pki_errors),
            ticks: get(&c.ticks),
        }
    }

    /// The lease on a key, if one is held.
    pub fn lease(&self, key: &InfoHash) -> Option<Lease> {
        self.leases.get(key)
    }

    /// The lease authority this server grants from.
    pub fn authority(&self) -> &Arc<LeaseAuthority> {
        &self.leases
    }

    /// Queues a message to one worker's session without waiting. Messages
    /// the server alone may send (`Welcome`, `Grants`, `HeartbeatAck`,
    /// `Renewed`, `RenewRefused`, `Stop`, `Accepted`) are refused, as is
    /// anything over a protocol limit. Pace large transfers (snapshots) by
    /// their acknowledgements: a full queue is [`SendError::Full`].
    pub fn send(&self, worker: WorkerId, msg: ToWorker) -> Result<(), SendError> {
        if !Self::sendable(&msg) {
            return Err(SendError::Invalid);
        }
        let fleet = lock(&self.fleet);
        let slot = fleet
            .workers
            .get(&worker)
            .and_then(|s| s.session.as_ref())
            .ok_or(SendError::NotConnected)?;
        slot.try_send(msg)
    }

    /// Queues a message to every connected worker; returns the workers it
    /// couldn't be queued to (full queues), for the caller to retry.
    pub fn broadcast(&self, msg: &ToWorker) -> Result<Vec<WorkerId>, SendError> {
        if !Self::sendable(msg) {
            return Err(SendError::Invalid);
        }
        let fleet = lock(&self.fleet);
        let mut missed = Vec::new();
        for (&w, state) in &fleet.workers {
            if let Some(slot) = &state.session
                && slot.try_send(msg.clone()).is_err()
            {
                missed.push(w);
            }
        }
        missed.sort();
        Ok(missed)
    }

    /// Bytes queued to a worker's session and not yet on the wire (at most
    /// [`ServerConfig::session_queue_bytes`]); `None` if it isn't connected.
    pub fn queued_bytes(&self, worker: WorkerId) -> Option<usize> {
        let fleet = lock(&self.fleet);
        let slot = fleet.workers.get(&worker)?.session.as_ref()?;
        Some(slot.budget.saturating_sub(slot.bytes.available_permits()))
    }

    fn sendable(msg: &ToWorker) -> bool {
        msg.validate()
            && matches!(
                msg,
                ToWorker::Assign(_)
                    | ToWorker::Drain
                    | ToWorker::HaveSnapshot(_)
                    | ToWorker::HaveDelta(_)
                    | ToWorker::Update(_)
                    | ToWorker::Hints(_)
                    | ToWorker::Snapshot(_)
                    | ToWorker::Reverify(_)
            )
    }

    /// Relays peer hints to the worker holding `key` under `token` (in
    /// memory only, §18.7): the built-in worker's go to the sink, a remote
    /// worker's to its session. Best effort: a hint that doesn't fit, or
    /// whose lease has moved, is dropped. `true` if it was passed on.
    pub fn relay_hints(
        &self,
        key: InfoHash,
        token: Token,
        hints: Vec<std::net::SocketAddr>,
    ) -> bool {
        if hints.is_empty() {
            return false;
        }
        let Some(holder) = self.leases.current(&key, token).map(|l| l.holder) else {
            return false;
        };
        let mut hints = hints;
        hints.retain(usable_hint);
        hints.truncate(MAX_HINTS);
        if hints.is_empty() {
            return false;
        }
        let item = HintItem {
            key: key.0,
            token: token.0,
            hints,
        };
        let sent = if holder == WorkerId::LOCAL {
            match &self.sink {
                Some(sink) => {
                    sink.local_hints(vec![item]);
                    true
                }
                None => false,
            }
        } else {
            let fleet = lock(&self.fleet);
            fleet
                .workers
                .get(&holder)
                .and_then(|s| s.session.as_ref())
                .is_some_and(|slot| slot.try_send_spare(ToWorker::Hints(vec![item])))
        };
        if sent {
            bump(&self.counters.hints_relayed);
        }
        sent
    }

    /// Stops the listener, the ticks and every session.
    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
        let fleet = lock(&self.fleet);
        for state in fleet.workers.values() {
            if let Some(slot) = &state.session {
                slot.stop(StopReason::Shutdown);
            }
        }
    }

    /// Serves the cluster port until [`shutdown`](Self::shutdown), ticking
    /// every heartbeat unless `auto_tick` is off.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) {
        let mut stop = self.shutdown.subscribe();
        if self.config.auto_tick {
            let me = self.clone();
            let mut stop = self.shutdown.subscribe();
            tokio::spawn(async move {
                let mut every = tokio::time::interval(me.config.heartbeat);
                every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                every.tick().await;
                loop {
                    tokio::select! {
                        _ = stop.changed() => return,
                        _ = every.tick() => me.tick().await,
                    }
                }
            });
        }
        loop {
            let accepted = tokio::select! {
                _ = stop.changed() => return,
                a = listener.accept() => a,
            };
            let (tcp, addr) = match accepted {
                Ok(a) => a,
                // Out of file descriptors and the like: back off a little.
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let Some(permit) = self.admit(addr.ip()) else {
                bump(&self.counters.refused);
                drop(tcp);
                continue;
            };
            let me = self.clone();
            tokio::spawn(async move { me.connection(tcp, permit).await });
        }
    }

    fn admit(self: &Arc<Self>, ip: IpAddr) -> Option<Permit> {
        let ip = ip_key(ip);
        let mut gate = lock(&self.gate);
        let here = gate.per_ip.get(&ip).copied().unwrap_or(0);
        if gate.total >= self.config.max_pending || here >= self.config.max_pending_per_ip {
            return None;
        }
        gate.total += 1;
        *gate.per_ip.entry(ip).or_insert(0) += 1;
        Some(Permit {
            server: self.clone(),
            ip,
        })
    }

    async fn connection(self: Arc<Self>, tcp: TcpStream, permit: Permit) {
        let _ = tcp.set_nodelay(true);
        match timeout(self.config.handshake_timeout, self.handshake(tcp)).await {
            Err(_) => bump(&self.counters.handshake_timeouts),
            Ok(None) => bump(&self.counters.handshake_failures),
            Ok(Some(Accepted::Join(tls, request))) => {
                // The redeem runs to completion once the request is in: a
                // timeout here would spend the token and drop the grant.
                // (A worker whose reply is lost anyway can ask again with
                // the same key, cluster1.) Writing the reply is bounded.
                let reply = self.answer_join(&request).await;
                drop(request);
                let _ = timeout(self.config.handshake_timeout, write_join(tls, &reply)).await;
                drop(permit);
            }
            Ok(Some(Accepted::Cluster(link, identity))) => {
                drop(permit);
                let peer = Peer {
                    worker: identity.worker,
                    identity: Some(identity),
                    session: 0,
                };
                self.session(link, peer).await;
            }
        }
    }

    async fn handshake(&self, tcp: TcpStream) -> Option<Accepted> {
        let start = LazyConfigAcceptor::new(rustls::server::Acceptor::default(), tcp)
            .await
            .ok()?;
        let config = self.tls.config_for(&start.client_hello())?;
        let tls = start.into_stream(config).await.ok()?;
        if tls.get_ref().1.alpn_protocol() == Some(ALPN_JOIN) {
            let mut tls = Box::new(tls) as BoxIo;
            let request = read_join(&mut tls).await?;
            return Some(Accepted::Join(tls, request));
        }
        // Authenticated before anything else is read.
        let identity = self.tls.identify(tls.get_ref().1)?;
        // The signature is tungstenite's handshake callback's.
        #[allow(clippy::result_large_err)]
        let check_path = |req: &Request, resp: Response| {
            if req.uri().path() == CLUSTER_PATH {
                Ok(resp)
            } else {
                let mut refusal = ErrorResponse::new(None);
                *refusal.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
                Err(refusal)
            }
        };
        let ws = tokio_tungstenite::accept_hdr_async_with_config(
            Box::new(tls) as BoxIo,
            check_path,
            Some(ws_config()),
        )
        .await
        .ok()?;
        Some(Accepted::Cluster(Link::ws(ws), identity))
    }

    /// The answer to a join request. Every refusal is the same `Refused`.
    async fn answer_join(&self, request: &[u8]) -> JoinReply {
        bump(&self.counters.join_requests);
        match frame::decode_join_request(request) {
            Ok(req) if req.major == PROTOCOL_MAJOR => {
                let text = Zeroizing::new(req.token);
                match JoinToken::parse(&text) {
                    Ok(token) => self.redeem(token, req.public_key).await,
                    Err(_) => JoinReply::Refused,
                }
            }
            _ => JoinReply::Refused,
        }
    }

    async fn redeem(&self, token: JoinToken, public_key: [u8; 32]) -> JoinReply {
        let now = self.clock.now_secs();
        let grant = self
            .with_pki(move |p| {
                // A token made by another process (the CLI) is in the
                // store, not yet in memory.
                p.pki
                    .coordinator
                    .reload(&p.pki.store, &p.pki.secrets)
                    .ok()?;
                let Pki {
                    coordinator, store, ..
                } = &mut p.pki;
                coordinator.redeem(store, &token, &public_key, now).ok()
            })
            .await
            .flatten();
        match grant {
            Some(g) => JoinReply::Granted(Box::new(g.into())),
            None => JoinReply::Refused,
        }
    }

    /// Serves the coordinator's built-in worker (§19.2) over in-process
    /// channels, through the same session code as a remote worker.
    pub fn connect_local(self: &Arc<Self>) -> WorkerLink {
        let (coordinator, worker) = local_pair(self.config.session_queue);
        let me = self.clone();
        tokio::spawn(async move {
            me.session(
                coordinator,
                Peer {
                    worker: WorkerId::LOCAL,
                    identity: None,
                    session: 0,
                },
            )
            .await;
        });
        worker
    }

    fn send_timeout(&self) -> Duration {
        self.config.heartbeat * self.config.heartbeat_timeout_beats.max(1)
    }

    async fn session(self: Arc<Self>, mut link: CoordinatorLink, mut peer: Peer) {
        let hello = match timeout(self.config.handshake_timeout, link.recv()).await {
            Ok(Ok(Some(ToCoordinator::Hello(hello)))) => hello,
            _ => {
                bump(&self.counters.protocol_errors);
                self.finish(&mut link, Some(StopReason::Protocol)).await;
                return;
            }
        };
        if hello.major != PROTOCOL_MAJOR {
            bump(&self.counters.version_refused);
            let reason = StopReason::VersionMismatch {
                coordinator_major: PROTOCOL_MAJOR,
            };
            self.finish(&mut link, Some(reason)).await;
            return;
        }
        if WorkerId(hello.worker) != peer.worker {
            bump(&self.counters.protocol_errors);
            self.finish(&mut link, Some(StopReason::IdentityMismatch))
                .await;
            return;
        }

        let id = self.next_session.fetch_add(1, Ordering::Relaxed);
        peer.session = id;
        let (ctl_tx, mut ctl) = mpsc::channel(self.config.session_queue.max(1));
        let budget = self.config.session_queue_bytes.clamp(1, u32::MAX as usize);
        let (stop_tx, mut stop) = mpsc::channel(1);
        let (drain_tx, mut drain) = mpsc::channel(1);
        let now_ms = self.clock.now_ms();
        let state_draining;
        {
            let mut fleet = lock(&self.fleet);
            let state = fleet
                .workers
                .entry(peer.worker)
                .or_insert_with(|| WorkerState {
                    session: None,
                    identity: None,
                    last_seen_ms: now_ms,
                    offset_ms: 0,
                    skewed: false,
                    draining: false,
                    stats: Stats::default(),
                    capabilities: Capabilities::default(),
                    last_ack: None,
                });
            if let Some(old) = state.session.take() {
                old.stop(StopReason::Replaced);
            }
            state.session = Some(SessionSlot {
                id,
                ctl: ctl_tx,
                bytes: Arc::new(Semaphore::new(budget)),
                budget,
                stop: stop_tx,
                drain: drain_tx,
            });
            state.identity = peer.identity.clone();
            state.last_seen_ms = now_ms;
            // A drain survives a reconnect: it's asked for again below.
            state.last_ack = None;
            state.capabilities = hello.capabilities;
            self.leases.heartbeat(peer.worker, now_ms.div_euclid(1000));
            state_draining = fleet.workers.get(&peer.worker).is_some_and(|s| s.draining);
        }
        bump(&self.counters.sessions);

        let welcome = ToWorker::Welcome(Welcome {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            worker: peer.worker.0,
            heartbeat_ms: u32::try_from(self.config.heartbeat.as_millis()).unwrap_or(u32::MAX),
            lease_ttl_secs: u32::try_from(self.config.lease_ttl_secs).unwrap_or(u32::MAX),
            renew_now: peer.identity.as_ref().is_some_and(|p| p.renew_now),
        });
        let mut greeted = true;
        let mut greeting = vec![welcome, ToWorker::Assign(Assign::default())];
        if state_draining {
            greeting.push(ToWorker::Drain);
        }
        for msg in greeting {
            greeted = greeted && self.send_on(&mut link, msg).await;
        }
        // A re-grant may have waited for this worker.
        {
            let mut fleet = lock(&self.fleet);
            self.regrant(&mut fleet, self.clock.now_secs());
        }
        let reason = if greeted {
            self.serve_session(&peer, &mut link, &mut ctl, &mut stop, &mut drain)
                .await
        } else {
            None
        };
        self.finish(&mut link, reason).await;
        let mut fleet = lock(&self.fleet);
        if let Some(state) = fleet.workers.get_mut(&peer.worker)
            && state.session.as_ref().is_some_and(|s| s.id == id)
        {
            state.session = None;
        }
    }

    /// The session's loop, until it stops (with the reason to send) or the
    /// connection ends.
    async fn serve_session(
        &self,
        peer: &Peer,
        link: &mut CoordinatorLink,
        ctl: &mut mpsc::Receiver<Queued>,
        stop: &mut mpsc::Receiver<StopReason>,
        drain: &mut mpsc::Receiver<()>,
    ) -> Option<StopReason> {
        loop {
            tokio::select! {
                biased;
                r = stop.recv() => return Some(r.unwrap_or(StopReason::Shutdown)),
                Some(()) = drain.recv() => {
                    if !self.send_on(link, ToWorker::Drain).await {
                        return None;
                    }
                }
                m = ctl.recv() => match m {
                    // The permit is held until the message is on the wire.
                    Some((msg, _permit)) => {
                        if !self.send_on(link, msg).await {
                            return None;
                        }
                    }
                    None => return None,
                },
                r = link.recv() => match r {
                    Ok(Some(msg)) => {
                        if let Flow::Stop(reason) = self.handle(peer, msg, link).await {
                            return Some(reason);
                        }
                    }
                    Ok(None) => return None,
                    Err(_) => {
                        bump(&self.counters.protocol_errors);
                        return Some(StopReason::Protocol);
                    }
                },
            }
        }
    }

    /// Sends one message, giving up on a peer that doesn't read for three
    /// heartbeats.
    async fn send_on(&self, link: &mut CoordinatorLink, msg: ToWorker) -> bool {
        matches!(
            timeout(self.send_timeout(), link.send(msg)).await,
            Ok(Ok(()))
        )
    }

    async fn finish(&self, link: &mut CoordinatorLink, reason: Option<StopReason>) {
        if let Some(reason) = reason {
            let _ = self.send_on(link, ToWorker::Stop(reason)).await;
        }
        let _ = timeout(self.send_timeout(), link.close()).await;
    }

    async fn handle(&self, peer: &Peer, msg: ToCoordinator, link: &mut CoordinatorLink) -> Flow {
        let w = peer.worker;
        match msg {
            ToCoordinator::Hello(_) => {
                bump(&self.counters.protocol_errors);
                Flow::Stop(StopReason::Protocol)
            }
            ToCoordinator::Heartbeat(hb) => {
                let Some(ack) = self.heartbeat(peer, &hb) else {
                    // Replaced or revoked in the meantime: stopping.
                    return Flow::Continue;
                };
                self.reply(link, ToWorker::HeartbeatAck(ack)).await
            }
            ToCoordinator::Claim(mut batch) => {
                if batch
                    .items
                    .iter()
                    .any(|i| SourceId::new(i.source).is_none())
                {
                    bump(&self.counters.protocol_errors);
                    return Flow::Stop(StopReason::Protocol);
                }
                // Commits from here on are remembered, so one racing the
                // store check is still caught (`claim_batch`).
                let checked_at = self.leases.commit_seq();
                let stored = match &self.stored {
                    Some(check) => {
                        let check = check.clone();
                        let keys: Vec<InfoHash> =
                            batch.items.iter().map(|i| InfoHash(i.key)).collect();
                        match tokio::task::spawn_blocking(move || check.stored(&keys)).await {
                            Ok(s) if s.len() == batch.items.len() => s,
                            _ => vec![false; batch.items.len()],
                        }
                    }
                    None => vec![false; batch.items.len()],
                };
                let items = self.claim(peer, &mut batch.items, &stored, checked_at);
                self.reply(
                    link,
                    ToWorker::Grants(GrantBatch {
                        batch: batch.batch,
                        items,
                    }),
                )
                .await
            }
            ToCoordinator::Outcome(report) => {
                self.outcome(peer, &report.items);
                Flow::Continue
            }
            ToCoordinator::Renew { public_key } => {
                let reply = match &peer.identity {
                    Some(identity) => self.renew(identity.clone(), public_key).await,
                    None => ToWorker::RenewRefused(RenewRefusal::Failed),
                };
                self.reply(link, reply).await
            }
            ToCoordinator::Drained => {
                let mut fleet = lock(&self.fleet);
                if !fleet.is_current(peer) {
                    return Flow::Stop(StopReason::Drained);
                }
                self.leases.evict(w);
                self.regrant(&mut fleet, self.clock.now_secs());
                Flow::Stop(StopReason::Drained)
            }
            ToCoordinator::Results(batch) | ToCoordinator::Counters(batch)
                if !lock(&self.fleet).is_current(peer) =>
            {
                // A replaced or revoked session's payload isn't taken; the
                // worker sends it again on its current session.
                let _ = batch;
                Flow::Continue
            }
            ToCoordinator::Results(batch) => {
                let id = batch.batch;
                let accepted = self.sink.as_ref().is_some_and(|s| s.results(w, batch));
                self.accepted(link, id, accepted).await
            }
            ToCoordinator::Counters(batch) => {
                let id = batch.batch;
                let accepted = self.sink.as_ref().is_some_and(|s| s.counters(w, batch));
                self.accepted(link, id, accepted).await
            }
            // As for payloads: a replaced or revoked session's answers
            // aren't taken.
            ToCoordinator::SnapshotAck(ack) => {
                if let Some(sink) = &self.sink
                    && lock(&self.fleet).is_current(peer)
                {
                    sink.snapshot_ack(w, ack);
                }
                Flow::Continue
            }
            ToCoordinator::Reverified(reply) => {
                if let Some(sink) = &self.sink
                    && lock(&self.fleet).is_current(peer)
                {
                    sink.reverified(w, reply);
                }
                Flow::Continue
            }
        }
    }

    async fn accepted(&self, link: &mut CoordinatorLink, batch: u64, accepted: bool) -> Flow {
        if !accepted {
            bump(&self.counters.inbound_refused);
        }
        self.reply(link, ToWorker::Accepted { batch, accepted })
            .await
    }

    async fn reply(&self, link: &mut CoordinatorLink, msg: ToWorker) -> Flow {
        if self.send_on(link, msg).await {
            Flow::Continue
        } else {
            Flow::Stop(StopReason::Protocol)
        }
    }

    /// Records a heartbeat: renews the worker's leases and estimates its
    /// clock offset θ (worker minus coordinator). With `t0` the worker's
    /// send time and `t1` our receive time, and `t2`/`t3` our last
    /// acknowledgement's send time and the worker's receive time,
    /// θ = ((t0 − t1) + (t3 − t2)) / 2, which cancels symmetric latency.
    /// Before the first acknowledgement comes back, θ ≈ t0 − t1.
    fn heartbeat(&self, peer: &Peer, hb: &Heartbeat) -> Option<HeartbeatAck> {
        let w = peer.worker;
        let t1 = self.clock.now_ms();
        let mut fleet = lock(&self.fleet);
        if !fleet.is_current(peer) {
            return None;
        }
        self.leases.heartbeat(w, t1.div_euclid(1000));
        let leases = u32::try_from(self.leases.held_by(w)).unwrap_or(u32::MAX);
        let state = fleet.workers.get_mut(&w)?;
        let forward = i128::from(hb.sent_ms) - i128::from(t1);
        let offset = match (hb.last_ack, state.last_ack) {
            (Some(echo), Some((seq, t2))) if echo.seq == seq => {
                (forward + (i128::from(echo.received_ms) - i128::from(t2))) / 2
            }
            _ => forward,
        };
        let offset = clamp(offset);
        let skewed = offset.unsigned_abs() > self.config.max_skew_ms.unsigned_abs();
        if skewed && !state.skewed {
            bump(&self.counters.skew_flags);
        }
        state.offset_ms = offset;
        state.skewed = skewed;
        state.last_seen_ms = t1;
        state.stats = hb.stats;
        state.last_ack = Some((hb.seq, t1));
        Some(HeartbeatAck {
            seq: hb.seq,
            coordinator_ms: t1,
            offset_ms: offset,
            skewed,
            leases,
        })
    }

    /// Grants or denies each claimed key. A session that is no longer the
    /// worker's current one (revoked, replaced or stopped while the claim
    /// waited on the store check) gets no lease: every key is denied as if
    /// draining, and the session is about to stop. Keys over what the
    /// [`GrantAdmission`] hook allows are denied as busy. A key held by
    /// another worker has its source merged into that lease and its peer
    /// hints relayed to the holder (§19.3).
    fn claim(
        &self,
        peer: &Peer,
        items: &mut [ClaimItem],
        stored: &[bool],
        checked_at: u64,
    ) -> Vec<GrantItem> {
        let w = peer.worker;
        let now_ms = self.clock.now_ms();
        let now = now_ms.div_euclid(1000);
        let skew = self.config.max_skew_ms.abs();
        let (earliest, latest) = (now_ms.saturating_sub(skew), now_ms.saturating_add(skew));
        let ttl_secs = u32::try_from(self.config.lease_ttl_secs).unwrap_or(u32::MAX);
        let fleet = lock(&self.fleet);
        let current = fleet.is_current(peer);
        let (draining, offset) = fleet
            .workers
            .get(&w)
            .map_or((true, 0), |s| (s.draining || !current, s.offset_ms));
        let mut out: Vec<Option<GrantItem>> = vec![None; items.len()];
        let mut asks: Vec<(usize, Claim)> = Vec::with_capacity(items.len());
        for (i, (item, &stored)) in items.iter().zip(stored).enumerate() {
            let key = item.key;
            let deny = |reason| Some(GrantItem::Denied { key, reason });
            if draining {
                out[i] = deny(DenyReason::Draining);
            } else if stored {
                out[i] = deny(DenyReason::Stored);
            } else if let Some(source) = SourceId::new(item.source) {
                // Discovery times are the worker's clock; correct them (§19.3).
                // Then clamped to now ± the skew limit: a worker's clock
                // never puts a key in another hour's cohort (§10.9.3).
                let at = clamp(i128::from(item.discovered_ms) - i128::from(offset))
                    .clamp(earliest, latest)
                    .div_euclid(1000);
                let claim = Claim {
                    key: InfoHash(key),
                    source,
                    strategy: item.strategy,
                    at,
                };
                asks.push((i, claim));
            }
        }
        let claims: Vec<Claim> = asks.iter().map(|(_, c)| *c).collect();
        // Admission applies to new leases only, under the authority's lock;
        // keys held elsewhere still merge their source and relay hints.
        let admit = self
            .admission
            .as_ref()
            .map(|gate| move |wanted: usize| gate.admit(w, wanted));
        let admit_ref = admit.as_ref().map(|f| f as &dyn Fn(usize) -> usize);
        let answers = self
            .leases
            .claim_batch(w, &claims, now, admit_ref, Some(checked_at));
        // Hints per holder, one message each.
        let mut relays: HashMap<WorkerId, Vec<HintItem>> = HashMap::new();
        for (&(i, claim), answer) in asks.iter().zip(answers) {
            let key = claim.key.0;
            let deny = |reason| GrantItem::Denied { key, reason };
            out[i] = Some(match answer {
                BatchAnswer::Busy => {
                    bump(&self.counters.busy);
                    deny(DenyReason::Busy)
                }
                BatchAnswer::Committed => deny(DenyReason::Stored),
                BatchAnswer::Claimed {
                    claimed: Claimed::Granted(token),
                    harvested,
                } => {
                    if harvested {
                        bump(&self.counters.harvested);
                    }
                    GrantItem::Granted {
                        key,
                        token: token.0,
                        ttl_secs,
                    }
                }
                // A repeated claim by the holder (after a reconnect, say)
                // gets its lease back.
                BatchAnswer::Claimed {
                    claimed: Claimed::Held { holder, token },
                    ..
                } if holder == w => GrantItem::Granted {
                    key,
                    token: token.0,
                    ttl_secs,
                },
                BatchAnswer::Claimed {
                    claimed: Claimed::Held { holder, token },
                    ..
                } => {
                    let mut hints = std::mem::take(&mut items[i].hints);
                    hints.retain(usable_hint);
                    if !hints.is_empty() {
                        let list = relays.entry(holder).or_default();
                        if list.len() < MAX_HINT_BATCH {
                            list.push(HintItem {
                                key,
                                token: token.0,
                                hints,
                            });
                        }
                    }
                    deny(DenyReason::HeldElsewhere)
                }
            });
        }
        let local_hints = relays.remove(&WorkerId::LOCAL);
        for (holder, hints) in relays {
            let n = hints.len() as u64;
            let sent = fleet
                .workers
                .get(&holder)
                .and_then(|s| s.session.as_ref())
                .is_some_and(|slot| slot.try_send_spare(ToWorker::Hints(hints)));
            if sent {
                self.counters.hints_relayed.fetch_add(n, Ordering::Relaxed);
            }
        }
        drop(fleet);
        if let Some(hints) = local_hints
            && let Some(sink) = &self.sink
        {
            self.counters
                .hints_relayed
                .fetch_add(hints.len() as u64, Ordering::Relaxed);
            sink.local_hints(hints);
        }
        out.into_iter().flatten().collect()
    }

    /// Accepts outcomes. One counts only with the current token from the
    /// current holder; anything else is stale and ignored, so a key is
    /// never counted twice (§19.3). `PeerFound` and `Lost` are terminal
    /// losses (their reason is the funnel's loss code) and end the lease.
    /// `Fetched` is progress: the lease stays until its result is committed
    /// under the same token (the commit ends it, as for the built-in
    /// worker), so the funnel gets its stored or lost row. Accepted ones go
    /// to the sink.
    ///
    /// This doesn't ask whether the session is still current: a report from
    /// an older connection of the same worker, with a token that is still
    /// current, is a real outcome (dropping it would strand a lease the new
    /// session's heartbeats keep renewing). A revoked worker's leases were
    /// evicted when it was revoked, so its tokens are already stale.
    fn outcome(&self, peer: &Peer, items: &[OutcomeItem]) {
        let w = peer.worker;
        let mut accepted = Vec::new();
        for item in items {
            let key = InfoHash(item.key);
            let token = Token(item.token);
            let lease = match item.outcome {
                Outcome::Fetched => self.leases.held(w, &key, token).inspect(|_| {
                    bump(&self.counters.fetched);
                }),
                Outcome::PeerFound { reason } | Outcome::Lost { reason } => self
                    .leases
                    .report(w, &key, token, Some(reason))
                    .inspect(|_| bump(&self.counters.outcomes)),
            };
            match lease {
                Some(lease) => accepted.push((key, lease, item.outcome)),
                None => bump(&self.counters.stale_reports),
            }
        }
        if !accepted.is_empty()
            && let Some(sink) = &self.sink
        {
            sink.outcomes(w, accepted);
        }
    }

    async fn renew(&self, identity: PeerIdentity, public_key: [u8; 32]) -> ToWorker {
        let now = self.clock.now_secs();
        let result = self
            .with_pki(move |p| {
                p.pki.coordinator.reload(&p.pki.store, &p.pki.secrets)?;
                let Pki {
                    coordinator, store, ..
                } = &mut p.pki;
                coordinator.renew(store, &identity, &public_key, now)
            })
            .await;
        match result {
            Some(Ok(grant)) => ToWorker::Renewed(Box::new(grant.into())),
            Some(Err(ClusterError::TooSoon)) => ToWorker::RenewRefused(RenewRefusal::TooSoon),
            Some(Err(ClusterError::Revoked)) => ToWorker::RenewRefused(RenewRefusal::Revoked),
            Some(Err(ClusterError::Expired)) => ToWorker::RenewRefused(RenewRefusal::Expired),
            _ => ToWorker::RenewRefused(RenewRefusal::Failed),
        }
    }

    /// One heartbeat tick (see the module docs). The server runs it every
    /// heartbeat; tests call it by hand.
    pub async fn tick(&self) {
        bump(&self.counters.ticks);
        let now_ms = self.clock.now_ms();
        let now = now_ms.div_euclid(1000);
        let every = self.config.maintain_every_secs;
        if !matches!(
            self.with_pki(move |p| p.refresh(now, every)).await,
            Some(Ok(()))
        ) {
            // The previous state stays in force; the next tick tries again.
            bump(&self.counters.pki_errors);
        }
        let mut fleet = lock(&self.fleet);
        self.sweep(&mut fleet, now_ms);
        self.leases.expire(now);
        self.regrant(&mut fleet, now);
    }

    /// Stops sessions whose certificate no longer stands (returning their
    /// leases) and sessions silent for too long.
    fn sweep(&self, fleet: &mut Fleet, now_ms: i64) {
        let silent_ms = i64::try_from(self.send_timeout().as_millis()).unwrap_or(i64::MAX);
        let mut revoked = Vec::new();
        for (&w, state) in &mut fleet.workers {
            let standing = state
                .identity
                .as_ref()
                .is_none_or(|id| self.tls.still_valid(id));
            if !standing {
                // Connected or not, a worker without a standing
                // certificate loses its leases (§19.6).
                revoked.push(w);
                if let Some(slot) = state.session.take() {
                    slot.stop(StopReason::Revoked);
                    bump(&self.counters.revoked);
                }
                continue;
            }
            let Some(slot) = &state.session else {
                continue;
            };
            if now_ms.saturating_sub(state.last_seen_ms) > silent_ms {
                slot.stop(StopReason::HeartbeatTimeout);
                state.session = None;
            }
        }
        for w in revoked {
            self.leases.evict(w);
            // Its certificates are gone, so it can't come back under this
            // id: forget it rather than check it every tick.
            fleet.workers.remove(&w);
        }
        self.regrant(fleet, now_ms.div_euclid(1000));
    }

    /// Grants keys whose lease ended without an outcome to the connected,
    /// non-draining workers holding the fewest leases, as unsolicited
    /// `Grants` (batch 0). Keys wait while no worker is available, or go
    /// back to waiting if a worker's queue is full.
    fn regrant(&self, fleet: &mut Fleet, now: i64) {
        let targets: Vec<WorkerId> = fleet
            .workers
            .iter()
            .filter(|(_, s)| s.session.is_some() && !s.draining)
            .map(|(&w, _)| w)
            .collect();
        if targets.is_empty() || self.leases.pending() == 0 {
            return;
        }
        let ttl_secs = u32::try_from(self.config.lease_ttl_secs).unwrap_or(u32::MAX);
        for (w, keys) in self.leases.regrant_pending(&targets, MAX_GRANT_BATCH, now) {
            let items: Vec<GrantItem> = keys
                .iter()
                .map(|&(key, token)| GrantItem::Granted {
                    key: key.0,
                    token: token.0,
                    ttl_secs,
                })
                .collect();
            let n = items.len() as u64;
            let slot = fleet.workers.get(&w).and_then(|s| s.session.as_ref());
            let sent = slot.is_some_and(|slot| {
                slot.try_send(ToWorker::Grants(GrantBatch { batch: 0, items }))
                    .is_ok()
            });
            if sent {
                self.counters.regrants.fetch_add(n, Ordering::Relaxed);
                continue;
            }
            // The worker's queue is full: take the leases back.
            for (key, token) in keys {
                self.leases.requeue(&key, token);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_addresses_share_a_slot_per_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::2".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(ip_key(a), ip_key(b));
        assert_ne!(ip_key(a), ip_key(c));
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        assert_eq!(ip_key(mapped), "192.0.2.7".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn clamping_never_overflows() {
        assert_eq!(clamp(i128::from(i64::MAX) * 4), i64::MAX);
        assert_eq!(clamp(i128::from(i64::MIN) * 4), i64::MIN);
        assert_eq!(clamp(-5), -5);
    }
}
