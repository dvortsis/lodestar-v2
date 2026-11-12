//! The worker's side of the cluster port: joining over the network,
//! connecting, and the session that keeps the connection alive.
//!
//! [`WorkerSession`] runs one task per connection. It sends heartbeats
//! (every interval the coordinator's `Welcome` sets, or on request),
//! matches replies to requests, tracks the leases it was granted, and
//! hands everything unsolicited (re-grants, `Assign`, `Drain`, `Stop`) to
//! the caller as [`WorkerEvent`]s. It runs the same over a WebSocket and
//! over the in-process link.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use zeroize::{Zeroize, Zeroizing};

use crate::clock::{Clock, ClockTime};
use crate::frame::{self, MAX_JOIN_REPLY};
use crate::link::{BoxIo, Link, LinkError, WorkerLink, ws_config};
use crate::proto::*;
use crate::server::CLUSTER_PATH;
use crate::{
    ClusterError, Credentials, JoinGrant, Joining, coordinator_server_name, x509::COORDINATOR_NAME,
};

#[derive(Debug)]
pub enum ClientError {
    Io(std::io::ErrorKind),
    Cluster(ClusterError),
    Link(LinkError),
    /// The coordinator refused the join token.
    Refused,
    Timeout,
    /// The coordinator stopped the session.
    Stopped(StopReason),
    /// The coordinator broke the protocol.
    Protocol,
    /// A request over a protocol limit.
    Invalid,
    /// The session has ended.
    Closed,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(kind) => write!(f, "couldn't reach the coordinator: {kind}"),
            ClientError::Cluster(e) => e.fmt(f),
            ClientError::Link(e) => e.fmt(f),
            ClientError::Refused => f.write_str(
                "the coordinator refused the join token (mistyped, expired or already used)",
            ),
            ClientError::Timeout => f.write_str("the coordinator didn't answer in time"),
            ClientError::Stopped(r) => write!(f, "the coordinator stopped this worker: {r:?}"),
            ClientError::Protocol => f.write_str("the coordinator broke the cluster protocol"),
            ClientError::Invalid => f.write_str("request over a protocol limit"),
            ClientError::Closed => f.write_str("the session has ended"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<ClusterError> for ClientError {
    fn from(e: ClusterError) -> Self {
        ClientError::Cluster(e)
    }
}

impl From<LinkError> for ClientError {
    fn from(e: LinkError) -> Self {
        ClientError::Link(e)
    }
}

fn io(e: std::io::Error) -> ClientError {
    ClientError::Io(e.kind())
}

/// Joins the coordinator at `addr` (`host:port`) with a token, over the
/// join protocol: TLS pinned to the token's CA, one request, one reply.
pub async fn join(
    addr: &str,
    joining: Joining,
    clock: Arc<dyn Clock>,
    within: Duration,
) -> Result<Credentials, ClientError> {
    let config = joining.client_config(Some(Arc::new(ClockTime(clock.clone()))))?;
    let mut request = JoinRequest {
        major: PROTOCOL_MAJOR,
        token: joining.token().expose().to_string(),
        public_key: joining.public_key(),
    };
    let encoded = frame::encode(&request).map_err(|_| ClientError::Invalid);
    request.token.zeroize();
    let request = Zeroizing::new(encoded?);
    let exchange = async {
        let tcp = TcpStream::connect(addr).await.map_err(io)?;
        let _ = tcp.set_nodelay(true);
        let mut tls = TlsConnector::from(config)
            .connect(coordinator_server_name(), tcp)
            .await
            .map_err(io)?;
        let len = u32::try_from(request.len()).map_err(|_| ClientError::Invalid)?;
        tls.write_u32(len).await.map_err(io)?;
        tls.write_all(&request).await.map_err(io)?;
        tls.flush().await.map_err(io)?;
        let len = usize::try_from(tls.read_u32().await.map_err(io)?)
            .map_err(|_| ClientError::Protocol)?;
        if len > MAX_JOIN_REPLY {
            return Err(ClientError::Protocol);
        }
        let mut buf = vec![0u8; len];
        tls.read_exact(&mut buf).await.map_err(io)?;
        frame::decode_join_reply(&buf).map_err(|_| ClientError::Protocol)
    };
    let reply = timeout(within, exchange)
        .await
        .map_err(|_| ClientError::Timeout)??;
    match reply {
        JoinReply::Granted(grant) => Ok(joining.finish(JoinGrant::from(*grant), clock.now_secs())?),
        JoinReply::Refused => Err(ClientError::Refused),
    }
}

/// Connects to the coordinator at `addr` with mutual TLS and upgrades to
/// the cluster WebSocket.
pub async fn connect(
    addr: &str,
    credentials: &Credentials,
    clock: Arc<dyn Clock>,
    within: Duration,
) -> Result<WorkerLink, ClientError> {
    let config = credentials.client_config(Some(Arc::new(ClockTime(clock))))?;
    let dial = async {
        let tcp = TcpStream::connect(addr).await.map_err(io)?;
        let _ = tcp.set_nodelay(true);
        let tls = TlsConnector::from(config)
            .connect(coordinator_server_name(), tcp)
            .await
            .map_err(io)?;
        let url = format!("wss://{COORDINATOR_NAME}{CLUSTER_PATH}");
        let (ws, _) = tokio_tungstenite::client_async_with_config(
            url,
            Box::new(tls) as BoxIo,
            Some(ws_config()),
        )
        .await
        .map_err(|_| ClientError::Protocol)?;
        Ok(Link::ws(ws))
    };
    timeout(within, dial)
        .await
        .map_err(|_| ClientError::Timeout)?
}

/// Something the coordinator sent that no request was waiting for.
///
/// Its `Debug` prints the have-filter, snapshot and `Assign` payloads only
/// by length, as `ToWorker`'s does: the have-filter header and the filter
/// snapshot carry keys (WK-3, WK-8).
#[derive(Clone, PartialEq, Eq)]
pub enum WorkerEvent {
    Assign(Assign),
    /// Keys granted with no request waiting: leases that expired elsewhere,
    /// or the answer to a claim whose caller timed out. A key is handed
    /// over once: a late answer leaves out keys already held under the
    /// same token and keys a retried claim is still waiting for. (If the
    /// late answer comes first, a retry's own answer repeats the key with
    /// the same token, as the truthful answer to that claim; treat leases
    /// by key and token.)
    Regranted(Vec<GrantItem>),
    /// Finish the leases held, report them, then call
    /// [`WorkerSession::drained`].
    Drain,
    /// Peer hints another worker found for keys this worker holds. Use
    /// them in memory only (§18.7); a hint whose token isn't the current
    /// lease is stale.
    Hints(Vec<HintItem>),
    /// The coordinator took a `Results` or `Counters` batch (`accepted`),
    /// or refused it for now: send it again later.
    Accepted {
        batch: u64,
        accepted: bool,
    },
    /// One chunk of a snapshot; acknowledge it with
    /// [`WorkerSession::send`] and [`ToCoordinator::SnapshotAck`].
    Snapshot(SnapshotChunk),
    /// A have-filter snapshot's header (WK-3): its chunks follow as
    /// [`WorkerEvent::Snapshot`]s of the have-filter kind.
    HaveSnapshot(Vec<u8>),
    /// Keys stored since the have-filter's header, in order (WK-3).
    HaveDelta(Vec<u8>),
    /// Fetch these keys again and answer with [`ToCoordinator::Reverified`].
    Reverify(ReverifyRequest),
    /// The coordinator says this worker's clock is more than 60 s off.
    ClockSkewed {
        offset_ms: i64,
    },
    /// The session ended: stopped by the coordinator (with its reason) or
    /// the connection closed.
    Ended(Option<StopReason>),
}

impl WorkerEvent {
    /// An upper bound on the event's size, for the byte cap.
    fn approx_size(&self) -> usize {
        64 + match self {
            WorkerEvent::Assign(a) => a.payload.len(),
            WorkerEvent::Regranted(items) => items.len() * 40,
            WorkerEvent::Hints(h) => h.iter().map(|i| 40 + i.hints.len() * 32).sum::<usize>(),
            WorkerEvent::Snapshot(c) => c.bytes.len(),
            WorkerEvent::HaveSnapshot(b) | WorkerEvent::HaveDelta(b) => b.len(),
            WorkerEvent::Reverify(r) => r.keys.len() * 20,
            WorkerEvent::Accepted { .. }
            | WorkerEvent::Drain
            | WorkerEvent::ClockSkewed { .. }
            | WorkerEvent::Ended(_) => 0,
        }
    }
}

impl std::fmt::Debug for WorkerEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len =
            |name: &str, n: usize, f: &mut std::fmt::Formatter<'_>| write!(f, "{name}({n} bytes)");
        match self {
            WorkerEvent::Assign(a) => f
                .debug_struct("Assign")
                .field("revision", &a.revision)
                .field("payload", &a.payload.len())
                .finish(),
            WorkerEvent::Regranted(items) => f.debug_tuple("Regranted").field(items).finish(),
            WorkerEvent::Drain => f.write_str("Drain"),
            // Peers are kept in memory only (§18.7): counts, not addresses.
            WorkerEvent::Hints(h) => f
                .debug_struct("Hints")
                .field("keys", &h.len())
                .field("peers", &h.iter().map(|i| i.hints.len()).sum::<usize>())
                .finish(),
            WorkerEvent::Accepted { batch, accepted } => f
                .debug_struct("Accepted")
                .field("batch", batch)
                .field("accepted", accepted)
                .finish(),
            WorkerEvent::Snapshot(c) => f
                .debug_struct("Snapshot")
                .field("kind", &c.kind)
                .field("version", &c.version)
                .field("seq", &c.seq)
                .field("total", &c.total)
                .field("bytes", &c.bytes.len())
                .finish(),
            WorkerEvent::HaveSnapshot(b) => len("HaveSnapshot", b.len(), f),
            WorkerEvent::HaveDelta(b) => len("HaveDelta", b.len(), f),
            WorkerEvent::Reverify(r) => f.debug_tuple("Reverify").field(r).finish(),
            WorkerEvent::ClockSkewed { offset_ms } => f
                .debug_struct("ClockSkewed")
                .field("offset_ms", offset_ms)
                .finish(),
            WorkerEvent::Ended(r) => f.debug_tuple("Ended").field(r).finish(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Send heartbeats on the coordinator's interval by itself. Tests turn
    /// it off and call [`WorkerSession::heartbeat`].
    pub auto_heartbeat: bool,
    /// How long to wait for `Welcome` and for each reply.
    pub reply_timeout: Duration,
    /// Events queued for the caller.
    pub event_queue: usize,
    /// Bytes of events queued for the caller (plan Q5: 8 MiB). While the
    /// caller doesn't read, the session stops reading the connection, so a
    /// stalled caller holds at most this much (one event larger than the
    /// cap is let through alone).
    pub event_queue_bytes: usize,
}

/// The default byte cap on each queue between a worker and its caller, and
/// on each coordinator session (plan Q5).
pub const QUEUE_BYTES: usize = 8 << 20;

impl Default for WorkerConfig {
    fn default() -> WorkerConfig {
        WorkerConfig {
            auto_heartbeat: true,
            reply_timeout: Duration::from_secs(15),
            event_queue: 256,
            event_queue_bytes: QUEUE_BYTES,
        }
    }
}

enum Command {
    Claim(Vec<ClaimItem>, oneshot::Sender<Vec<GrantItem>>),
    Report(Vec<OutcomeItem>, oneshot::Sender<()>),
    Heartbeat(oneshot::Sender<HeartbeatAck>),
    Renew([u8; 32], oneshot::Sender<Result<JoinGrant, RenewRefusal>>),
    Drained(oneshot::Sender<()>),
    Stats(Stats),
    Send(ToCoordinator, oneshot::Sender<()>),
    Committed(Vec<([u8; 20], u64)>, oneshot::Sender<()>),
    Holding(Vec<HeldLease>, oneshot::Sender<ReconcileAnswer>),
}

type Queued = (WorkerEvent, Option<OwnedSemaphorePermit>);

/// A live session with the coordinator.
///
/// Read [`next_event`](Self::next_event) from its own task: while the event
/// queue is at its byte cap the session stops reading the connection, so a
/// caller that waits on `claim`, `report` or `send` without also reading
/// events stalls until `reply_timeout`.
#[derive(Debug)]
pub struct WorkerSession {
    welcome: Welcome,
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Queued>,
    bytes: Arc<Semaphore>,
    budget: usize,
    leases: Arc<AtomicU32>,
    reply_timeout: Duration,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Command")
    }
}

impl WorkerSession {
    /// Says `Hello` and waits for `Welcome`. A coordinator with another
    /// major version answers with a stop, returned as the error.
    pub async fn start(
        mut link: WorkerLink,
        hello: Hello,
        clock: Arc<dyn Clock>,
        config: WorkerConfig,
    ) -> Result<WorkerSession, ClientError> {
        link.send(ToCoordinator::Hello(hello)).await?;
        let welcome = match timeout(config.reply_timeout, link.recv()).await {
            Err(_) => return Err(ClientError::Timeout),
            Ok(Ok(Some(ToWorker::Welcome(w)))) => w,
            Ok(Ok(Some(ToWorker::Stop(reason)))) => return Err(ClientError::Stopped(reason)),
            Ok(Ok(None)) => return Err(ClientError::Closed),
            Ok(Err(e)) => return Err(e.into()),
            Ok(Ok(Some(_))) => return Err(ClientError::Protocol),
        };
        if welcome.major != PROTOCOL_MAJOR {
            link.close().await;
            return Err(ClientError::Protocol);
        }
        let (commands, command_rx) = mpsc::channel(64);
        let (event_tx, events) = mpsc::channel(config.event_queue.max(1));
        let budget = config.event_queue_bytes.clamp(1, u32::MAX as usize);
        let bytes = Arc::new(Semaphore::new(budget));
        let leases = Arc::new(AtomicU32::new(0));
        let driver = Driver {
            link,
            clock,
            commands: command_rx,
            events: event_tx,
            bytes: bytes.clone(),
            budget,
            leases: leases.clone(),
            held: HashMap::new(),
            fetched: HashMap::new(),
            ttl_ms: i64::from(welcome.lease_ttl_secs).saturating_mul(1000),
            claims: HashMap::new(),
            heartbeats: HashMap::new(),
            reconciling: None,
            renewal: None,
            next_batch: 1,
            seq: 0,
            last_ack: None,
            stats: Stats::default(),
            skew_reported: false,
        };
        let every = config
            .auto_heartbeat
            .then(|| Duration::from_millis(u64::from(welcome.heartbeat_ms.max(100))));
        tokio::spawn(driver.run(every));
        Ok(WorkerSession {
            welcome,
            commands,
            events,
            bytes,
            budget,
            leases,
            reply_timeout: config.reply_timeout,
        })
    }

    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// A cloneable handle for the requests (claims, reports, results,
    /// commits, stats), so the worker's runtime can make them from its own
    /// tasks while the caller keeps reading [`next_event`](Self::next_event).
    /// It fails with [`ClientError::Closed`] once the session ends.
    pub fn handle(&self) -> SessionHandle {
        SessionHandle {
            commands: self.commands.clone(),
            leases: self.leases.clone(),
            reply_timeout: self.reply_timeout,
        }
    }

    /// Bytes of events waiting for [`next_event`](Self::next_event) (at
    /// most [`WorkerConfig::event_queue_bytes`]).
    pub fn queued_bytes(&self) -> usize {
        self.budget.saturating_sub(self.bytes.available_permits())
    }

    /// Leases this worker holds.
    pub fn leases(&self) -> u32 {
        self.leases.load(Ordering::Relaxed)
    }

    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(make(tx))
            .await
            .map_err(|_| ClientError::Closed)?;
        timeout(self.reply_timeout, rx)
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|_| ClientError::Closed)
    }

    /// Claims newly found keys (1 to [`MAX_CLAIM_BATCH`]); one answer per
    /// key, in order.
    pub async fn claim(&self, items: Vec<ClaimItem>) -> Result<Vec<GrantItem>, ClientError> {
        if items.is_empty() || items.len() > MAX_CLAIM_BATCH {
            return Err(ClientError::Invalid);
        }
        self.ask(|tx| Command::Claim(items, tx)).await
    }

    /// Reports outcomes (sent once they're on the wire; the coordinator
    /// doesn't answer). `PeerFound` and `Lost` end the lease; `Fetched`
    /// doesn't: the lease stays held until the coordinator commits the
    /// result (see [`committed`](Self::committed)).
    pub async fn report(&self, items: Vec<OutcomeItem>) -> Result<(), ClientError> {
        if items.is_empty() || items.len() > MAX_OUTCOME_BATCH {
            return Err(ClientError::Invalid);
        }
        self.ask(|tx| Command::Report(items, tx)).await
    }

    /// Sends a heartbeat now and waits for its acknowledgement. Every
    /// message sent before it has been handled by the coordinator once this
    /// returns.
    pub async fn heartbeat(&self) -> Result<HeartbeatAck, ClientError> {
        self.ask(Command::Heartbeat).await
    }

    /// Updates the stats the next heartbeats carry.
    pub async fn set_stats(&self, stats: Stats) -> Result<(), ClientError> {
        self.commands
            .send(Command::Stats(stats))
            .await
            .map_err(|_| ClientError::Closed)
    }

    /// Asks for a renewed certificate over `public_key` (from
    /// [`Credentials::begin_renewal`]).
    pub async fn renew(
        &self,
        public_key: [u8; 32],
    ) -> Result<Result<JoinGrant, RenewRefusal>, ClientError> {
        self.ask(|tx| Command::Renew(public_key, tx)).await
    }

    /// Sends one of the messages that need no reply tracking: `Results`,
    /// `Counters`, `SnapshotAck` or `Reverified`. Returns once it's on the
    /// wire. Anything else has its own method and is refused here.
    pub async fn send(&self, msg: ToCoordinator) -> Result<(), ClientError> {
        if !Self::sendable(&msg) {
            return Err(ClientError::Invalid);
        }
        self.ask(|tx| Command::Send(msg, tx)).await
    }

    fn sendable(msg: &ToCoordinator) -> bool {
        matches!(
            msg,
            ToCoordinator::Results(_)
                | ToCoordinator::Counters(_)
                | ToCoordinator::SnapshotAck(_)
                | ToCoordinator::Reverified(_)
        ) && msg.validate()
    }

    /// Forgets leases whose results the coordinator accepted (an
    /// `Accepted { accepted: true }` for the `Results` batch that carried
    /// them): their commit ends them. Keys held under another token stay.
    pub async fn committed(&self, leases: Vec<([u8; 20], u64)>) -> Result<(), ClientError> {
        self.ask(|tx| Command::Committed(leases, tx)).await
    }

    /// Tells the coordinator the drain is done; it stops the session.
    pub async fn drained(&self) -> Result<(), ClientError> {
        self.ask(Command::Drained).await
    }

    /// Lists every lease the worker still holds (keys held and results
    /// not yet committed), in [`Holding`] parts, and waits for the
    /// coordinator's answer (WK-11b): the leases it has on record for this
    /// worker that the list leaves out, delivered on an earlier session,
    /// go back for re-grant. `Ok(None)`, nothing sent, when the
    /// coordinator's minor predates [`MINOR_HOLDING`]. Over [`MAX_HOLDING`]
    /// leases is [`ClientError::Invalid`]: a partial list would give back
    /// leases the worker holds.
    pub async fn holding(
        &self,
        leases: Vec<HeldLease>,
    ) -> Result<Option<ReconcileAnswer>, ClientError> {
        if self.welcome.minor < MINOR_HOLDING {
            return Ok(None);
        }
        if leases.len() > MAX_HOLDING {
            return Err(ClientError::Invalid);
        }
        self.ask(|tx| Command::Holding(leases, tx)).await.map(Some)
    }

    /// The next unsolicited message, or `None` after
    /// [`WorkerEvent::Ended`].
    pub async fn next_event(&mut self) -> Option<WorkerEvent> {
        // The permit goes back as the event leaves the queue.
        self.events.recv().await.map(|(event, _permit)| event)
    }
}

/// A session's requests, from any task ([`WorkerSession::handle`]).
#[derive(Debug, Clone)]
pub struct SessionHandle {
    commands: mpsc::Sender<Command>,
    leases: Arc<AtomicU32>,
    reply_timeout: Duration,
}

impl SessionHandle {
    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(make(tx))
            .await
            .map_err(|_| ClientError::Closed)?;
        timeout(self.reply_timeout, rx)
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|_| ClientError::Closed)
    }

    /// As [`WorkerSession::claim`].
    pub async fn claim(&self, items: Vec<ClaimItem>) -> Result<Vec<GrantItem>, ClientError> {
        if items.is_empty() || items.len() > MAX_CLAIM_BATCH {
            return Err(ClientError::Invalid);
        }
        self.ask(|tx| Command::Claim(items, tx)).await
    }

    /// As [`WorkerSession::report`].
    pub async fn report(&self, items: Vec<OutcomeItem>) -> Result<(), ClientError> {
        if items.is_empty() || items.len() > MAX_OUTCOME_BATCH {
            return Err(ClientError::Invalid);
        }
        self.ask(|tx| Command::Report(items, tx)).await
    }

    /// As [`WorkerSession::send`].
    pub async fn send(&self, msg: ToCoordinator) -> Result<(), ClientError> {
        if !WorkerSession::sendable(&msg) {
            return Err(ClientError::Invalid);
        }
        self.ask(|tx| Command::Send(msg, tx)).await
    }

    /// As [`WorkerSession::committed`].
    pub async fn committed(&self, leases: Vec<([u8; 20], u64)>) -> Result<(), ClientError> {
        self.ask(|tx| Command::Committed(leases, tx)).await
    }

    /// As [`WorkerSession::set_stats`].
    pub async fn set_stats(&self, stats: Stats) -> Result<(), ClientError> {
        self.commands
            .send(Command::Stats(stats))
            .await
            .map_err(|_| ClientError::Closed)
    }

    /// Leases the session holds.
    pub fn leases(&self) -> u32 {
        self.leases.load(Ordering::Relaxed)
    }

    /// Whether the session has ended.
    pub fn is_closed(&self) -> bool {
        self.commands.is_closed()
    }
}

type ClaimReply = oneshot::Sender<Vec<GrantItem>>;

struct Driver {
    link: WorkerLink,
    clock: Arc<dyn Clock>,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Queued>,
    /// The byte cap on queued events, as permits.
    bytes: Arc<Semaphore>,
    budget: usize,
    leases: Arc<AtomicU32>,
    /// Leases held: key → fencing token.
    held: HashMap<[u8; 20], u64>,
    /// Held keys reported `Fetched`, with when (this worker's clock, ms):
    /// they wait for the coordinator's commit (`committed`). One still
    /// waiting a lease TTL later is dropped from `held` (WK-9): its batch
    /// was lost, or never accepted, and the coordinator's lease outlives it
    /// only while this worker keeps it alive.
    fetched: HashMap<[u8; 20], (u64, i64)>,
    /// The lease TTL the coordinator stated (`Welcome`), in ms.
    ttl_ms: i64,
    /// The reconcile round awaiting its answer (one at a time).
    reconciling: Option<(u64, oneshot::Sender<ReconcileAnswer>)>,
    /// Claims awaiting their reply, with the keys each asked for.
    claims: HashMap<u64, (ClaimReply, Vec<[u8; 20]>)>,
    heartbeats: HashMap<u64, oneshot::Sender<HeartbeatAck>>,
    renewal: Option<oneshot::Sender<Result<JoinGrant, RenewRefusal>>>,
    next_batch: u64,
    seq: u64,
    last_ack: Option<AckEcho>,
    stats: Stats,
    skew_reported: bool,
}

enum Step {
    Continue,
    End(Option<StopReason>),
}

impl Driver {
    async fn run(mut self, every: Option<Duration>) {
        let mut ticker = every.map(|d| {
            let mut t = tokio::time::interval(d);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            t
        });
        if let Some(t) = &mut ticker {
            t.tick().await;
        }
        let end = loop {
            let step = tokio::select! {
                msg = self.link.recv() => match msg {
                    Ok(Some(msg)) => self.incoming(msg).await,
                    Ok(None) | Err(_) => Step::End(None),
                },
                cmd = self.commands.recv() => match cmd {
                    Some(cmd) => self.command(cmd).await,
                    // The session handle was dropped.
                    None => Step::End(None),
                },
                _ = async {
                    match &mut ticker {
                        Some(t) => { t.tick().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => self.beat(None).await,
            };
            if let Step::End(reason) = step {
                break reason;
            }
        };
        self.link.close().await;
        let _ = self.events.send((WorkerEvent::Ended(end), None)).await;
    }

    fn sync_leases(&self) {
        self.leases.store(
            u32::try_from(self.held.len()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
    }

    /// Whether this grant is one the worker already holds.
    fn holds(&self, item: &GrantItem) -> bool {
        match *item {
            GrantItem::Granted { key, token, .. } => self.held.get(&key) == Some(&token),
            GrantItem::Denied { .. } => false,
        }
    }

    fn take_grants(&mut self, items: &[GrantItem]) {
        for item in items {
            if let GrantItem::Granted { key, token, .. } = *item {
                self.held.insert(key, token);
            }
        }
        self.sync_leases();
    }

    async fn send(&mut self, msg: ToCoordinator) -> Step {
        match self.link.send(msg).await {
            Ok(()) => Step::Continue,
            Err(_) => Step::End(None),
        }
    }

    /// Forgets fetched keys whose commit never came within the lease TTL.
    fn prune(&mut self) {
        let oldest = self.clock.now_ms().saturating_sub(self.ttl_ms);
        let held = &mut self.held;
        self.fetched.retain(|key, &mut (token, at)| {
            let waiting = held.get(key) == Some(&token);
            if waiting && at < oldest {
                held.remove(key);
                return false;
            }
            waiting
        });
        self.sync_leases();
    }

    async fn beat(&mut self, reply: Option<oneshot::Sender<HeartbeatAck>>) -> Step {
        self.prune();
        self.seq += 1;
        if let Some(reply) = reply {
            self.heartbeats.insert(self.seq, reply);
        }
        let hb = Heartbeat {
            seq: self.seq,
            sent_ms: self.clock.now_ms(),
            last_ack: self.last_ack,
            leases: u32::try_from(self.held.len()).unwrap_or(u32::MAX),
            stats: self.stats,
        };
        self.send(ToCoordinator::Heartbeat(hb)).await
    }

    async fn command(&mut self, cmd: Command) -> Step {
        // Requests whose caller gave up (timed out) wait for no reply.
        self.claims.retain(|_, (tx, _)| !tx.is_closed());
        self.heartbeats.retain(|_, tx| !tx.is_closed());
        match cmd {
            Command::Claim(items, reply) => {
                let batch = self.next_batch;
                self.next_batch += 1;
                let keys = items.iter().map(|i| i.key).collect();
                self.claims.insert(batch, (reply, keys));
                self.stats.claimed += items.len() as u64;
                self.send(ToCoordinator::Claim(ClaimBatch { batch, items }))
                    .await
            }
            Command::Report(items, reply) => {
                // `Fetched` keeps the lease (the commit ends it), as on the
                // coordinator; the losses end it.
                let now = self.clock.now_ms();
                for item in &items {
                    if self.held.get(&item.key) != Some(&item.token) {
                        continue;
                    }
                    if item.outcome == Outcome::Fetched {
                        // Kept from the first report under this token; a
                        // new token (a re-grant) starts its wait again.
                        let stamp = self.fetched.entry(item.key).or_insert((item.token, now));
                        if stamp.0 != item.token {
                            *stamp = (item.token, now);
                        }
                    } else {
                        self.held.remove(&item.key);
                    }
                }
                self.sync_leases();
                let step = self
                    .send(ToCoordinator::Outcome(OutcomeBatch { items }))
                    .await;
                let _ = reply.send(());
                step
            }
            Command::Heartbeat(reply) => self.beat(Some(reply)).await,
            Command::Renew(public_key, reply) => {
                self.renewal = Some(reply);
                self.send(ToCoordinator::Renew { public_key }).await
            }
            Command::Drained(reply) => {
                let step = self.send(ToCoordinator::Drained).await;
                let _ = reply.send(());
                step
            }
            Command::Committed(leases, reply) => {
                for (key, token) in leases {
                    if self.held.get(&key) == Some(&token) {
                        self.held.remove(&key);
                        self.fetched.remove(&key);
                    }
                }
                self.sync_leases();
                let _ = reply.send(());
                Step::Continue
            }
            Command::Send(msg, reply) => {
                let step = self.send(msg).await;
                let _ = reply.send(());
                step
            }
            Command::Holding(leases, reply) => {
                // Its round number comes from the claims' counter: never 0,
                // never reused on this session.
                let round = self.next_batch;
                self.next_batch += 1;
                self.reconciling = Some((round, reply));
                let parts = leases.len().div_ceil(MAX_HOLDING_BATCH).max(1);
                let mut chunks = leases.chunks(MAX_HOLDING_BATCH);
                for part in 0..parts {
                    let msg = ToCoordinator::Holding(Holding {
                        round,
                        part: u32::try_from(part).unwrap_or(u32::MAX),
                        last: part + 1 == parts,
                        leases: HeldList(chunks.next().unwrap_or(&[]).to_vec()),
                    });
                    if let Step::End(r) = self.send(msg).await {
                        return Step::End(r);
                    }
                }
                Step::Continue
            }
            Command::Stats(stats) => {
                self.stats = Stats {
                    claimed: self.stats.claimed,
                    ..stats
                };
                Step::Continue
            }
        }
    }

    /// Hints for leases this worker holds under that token; the rest are
    /// stale and dropped.
    async fn hint(&mut self, mut items: Vec<HintItem>) -> Step {
        items.retain(|i| self.held.get(&i.key) == Some(&i.token));
        if items.is_empty() {
            Step::Continue
        } else {
            self.event(WorkerEvent::Hints(items)).await
        }
    }

    async fn event(&mut self, event: WorkerEvent) -> Step {
        // Waiting for room stops this loop reading the connection, which
        // pushes back on the coordinator (its sends time out).
        let need = u32::try_from(event.approx_size().min(self.budget)).unwrap_or(u32::MAX);
        let Ok(permit) = self.bytes.clone().acquire_many_owned(need).await else {
            return Step::End(None);
        };
        match self.events.send((event, Some(permit))).await {
            Ok(()) => Step::Continue,
            // Nobody is listening any more.
            Err(_) => Step::End(None),
        }
    }

    async fn incoming(&mut self, msg: ToWorker) -> Step {
        match msg {
            ToWorker::HeartbeatAck(ack) => {
                self.last_ack = Some(AckEcho {
                    seq: ack.seq,
                    received_ms: self.clock.now_ms(),
                });
                if let Some(reply) = self.heartbeats.remove(&ack.seq) {
                    let _ = reply.send(ack);
                }
                if ack.skewed && !self.skew_reported {
                    self.skew_reported = true;
                    return self
                        .event(WorkerEvent::ClockSkewed {
                            offset_ms: ack.offset_ms,
                        })
                        .await;
                }
                self.skew_reported &= ack.skewed;
                Step::Continue
            }
            ToWorker::Grants(GrantBatch { batch: 0, items }) => {
                self.take_grants(&items);
                self.event(WorkerEvent::Regranted(items)).await
            }
            ToWorker::Grants(GrantBatch { batch, items }) => {
                // Only claims someone still waits for may hold a key back.
                self.claims.retain(|_, (tx, _)| !tx.is_closed());
                let known: Vec<bool> = items.iter().map(|i| self.holds(i)).collect();
                self.take_grants(&items);
                let late = match self.claims.remove(&batch) {
                    Some((reply, _)) => match reply.send(items) {
                        Ok(()) => return Step::Continue,
                        Err(items) => items,
                    },
                    None => items,
                };
                // Its caller gave up, but the leases are ours: hand them
                // over once. Keys already held under the same token were
                // delivered before, and keys a retried claim is still
                // waiting for will come with its reply.
                let fresh: Vec<GrantItem> = late
                    .into_iter()
                    .zip(known)
                    .filter(|(item, known)| {
                        matches!(item, GrantItem::Granted { .. })
                            && !known
                            && !self
                                .claims
                                .values()
                                .any(|(_, keys)| keys.contains(&item.key()))
                    })
                    .map(|(item, _)| item)
                    .collect();
                if fresh.is_empty() {
                    Step::Continue
                } else {
                    self.event(WorkerEvent::Regranted(fresh)).await
                }
            }
            ToWorker::Renewed(grant) => {
                if let Some(reply) = self.renewal.take() {
                    let _ = reply.send(Ok(JoinGrant::from(*grant)));
                }
                Step::Continue
            }
            ToWorker::RenewRefused(why) => {
                if let Some(reply) = self.renewal.take() {
                    let _ = reply.send(Err(why));
                }
                Step::Continue
            }
            ToWorker::Assign(assign) => self.event(WorkerEvent::Assign(assign)).await,
            ToWorker::Drain => self.event(WorkerEvent::Drain).await,
            ToWorker::Stop(reason) => Step::End(Some(reason)),
            ToWorker::Welcome(_) => Step::End(None),
            ToWorker::Hints(items) => self.hint(items).await,
            ToWorker::Accepted { batch, accepted } => {
                self.event(WorkerEvent::Accepted { batch, accepted }).await
            }
            ToWorker::Snapshot(chunk) => self.event(WorkerEvent::Snapshot(chunk)).await,
            ToWorker::Reverify(req) => self.event(WorkerEvent::Reverify(req)).await,
            ToWorker::HaveSnapshot(b) => self.event(WorkerEvent::HaveSnapshot(b)).await,
            ToWorker::HaveDelta(b) => self.event(WorkerEvent::HaveDelta(b)).await,
            // Update offers are out of v1 (plan Q13).
            ToWorker::Update(_) => Step::Continue,
            ToWorker::Reconciled(r) => {
                if self
                    .reconciling
                    .as_ref()
                    .is_some_and(|(round, _)| *round == r.round)
                    && let Some((_, reply)) = self.reconciling.take()
                {
                    let _ = reply.send(r.answer);
                }
                Step::Continue
            }
        }
    }
}

#[cfg(test)]
mod held_prune {
    use super::*;
    use crate::{ManualClock, local_pair};

    /// A key reported `Fetched` whose commit never comes (its batch was
    /// lost, or never accepted) is dropped from the held set a lease TTL
    /// later, on the injected clock; one committed in time, or still being
    /// fetched, isn't (WK-9, from WK-0's review).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unanswered_fetched_key_is_pruned_after_the_ttl() {
        let (mut coord, worker) = local_pair(16);
        let clock = Arc::new(ManualClock::new(1_000_000));
        let config = WorkerConfig {
            auto_heartbeat: false,
            ..WorkerConfig::default()
        };
        let (session, ()) = tokio::join!(
            WorkerSession::start(
                worker,
                Hello::new(0, Capabilities::default()),
                clock.clone(),
                config
            ),
            async {
                assert!(matches!(
                    coord.recv().await,
                    Ok(Some(ToCoordinator::Hello(_)))
                ));
                coord
                    .send(ToWorker::Welcome(Welcome {
                        major: PROTOCOL_MAJOR,
                        minor: PROTOCOL_MINOR,
                        worker: 0,
                        heartbeat_ms: 5_000,
                        lease_ttl_secs: 600,
                        renew_now: false,
                    }))
                    .await
                    .unwrap();
            }
        );
        let s = session.unwrap();
        // The coordinator grants keys 1, 2 and 3 unasked (re-grants).
        let grant = |n: u8| GrantItem::Granted {
            key: [n; 20],
            token: u64::from(n) + 10,
            ttl_secs: 600,
        };
        coord
            .send(ToWorker::Grants(GrantBatch {
                batch: 0,
                items: vec![grant(1), grant(2), grant(3)],
            }))
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while s.leases() < 3 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "grants never arrived"
            );
            tokio::task::yield_now().await;
        }
        let fetched = |n: u8| OutcomeItem {
            key: [n; 20],
            token: u64::from(n) + 10,
            outcome: Outcome::Fetched,
        };
        s.report(vec![fetched(1), fetched(2)]).await.unwrap();
        // Key 2's batch is accepted; key 1's answer never comes; key 3 is
        // still being fetched.
        s.committed(vec![([2; 20], 12)]).await.unwrap();
        clock.advance(Duration::from_secs(599));
        let beat = |s: &WorkerSession| {
            let s = s.commands.clone();
            async move {
                let (tx, _rx) = oneshot::channel();
                s.send(Command::Heartbeat(tx)).await.unwrap();
            }
        };
        beat(&s).await;
        s.committed(vec![]).await.unwrap();
        assert_eq!(s.leases(), 2, "within the TTL: kept");
        clock.advance(Duration::from_secs(2));
        beat(&s).await;
        s.committed(vec![]).await.unwrap();
        assert_eq!(s.leases(), 1, "key 1 pruned; key 3 still fetching");
    }
}

#[cfg(test)]
mod handle {
    use super::*;
    use crate::{ManualClock, local_pair};

    /// The handle makes requests from another task while the session's
    /// owner reads events; the have-filter messages reach the caller (they
    /// were dropped before WK-10b); and no event prints a payload byte.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_handle_works_from_another_task_and_events_print_no_payload() {
        let (mut coord, worker) = local_pair(16);
        let config = WorkerConfig {
            auto_heartbeat: false,
            ..WorkerConfig::default()
        };
        let clock = Arc::new(ManualClock::new(1_000_000));
        let (session, ()) = tokio::join!(
            WorkerSession::start(
                worker,
                Hello::new(0, Capabilities::default()),
                clock,
                config
            ),
            async {
                assert!(matches!(
                    coord.recv().await,
                    Ok(Some(ToCoordinator::Hello(_)))
                ));
                coord
                    .send(ToWorker::Welcome(Welcome {
                        major: PROTOCOL_MAJOR,
                        minor: PROTOCOL_MINOR,
                        worker: 0,
                        heartbeat_ms: 5_000,
                        lease_ttl_secs: 600,
                        renew_now: false,
                    }))
                    .await
                    .unwrap();
            }
        );
        let mut s = session.unwrap();
        let h = s.handle();
        let claimed = tokio::spawn(async move {
            h.claim(vec![ClaimItem {
                key: [4; 20],
                source: 0,
                strategy: 0,
                discovered_ms: 0,
                hints: Vec::new(),
            }])
            .await
        });
        let Ok(Some(ToCoordinator::Claim(batch))) = coord.recv().await else {
            panic!("no claim");
        };
        coord
            .send(ToWorker::Grants(GrantBatch {
                batch: batch.batch,
                items: vec![GrantItem::Granted {
                    key: [4; 20],
                    token: 9,
                    ttl_secs: 600,
                }],
            }))
            .await
            .unwrap();
        let grants = claimed.await.unwrap().unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(s.handle().leases(), 1);
        // Requests the session refuses are refused by the handle too.
        assert!(matches!(
            s.handle().send(ToCoordinator::Drained).await,
            Err(ClientError::Invalid)
        ));

        let secret = vec![0xAB; 48];
        for msg in [
            ToWorker::HaveSnapshot(secret.clone()),
            ToWorker::HaveDelta(secret.clone()),
            ToWorker::Snapshot(SnapshotChunk {
                kind: SnapshotKind::Filter,
                version: 3,
                seq: 0,
                total: 1,
                bytes: secret.clone(),
            }),
        ] {
            coord.send(msg).await.unwrap();
        }
        let mut seen = Vec::new();
        while seen.len() < 3 {
            let e = timeout(Duration::from_secs(15), s.next_event())
                .await
                .unwrap()
                .unwrap();
            let shown = format!("{e:?}");
            assert!(
                !shown.contains("171") && !shown.to_lowercase().contains("ab, ab"),
                "{shown}"
            );
            seen.push(e);
        }
        assert_eq!(seen[0], WorkerEvent::HaveSnapshot(secret.clone()));
        assert_eq!(seen[1], WorkerEvent::HaveDelta(secret));
        assert!(format!("{:?}", seen[0]).contains("48 bytes"));

        // Once the session ends, the handle says so.
        let h = s.handle();
        coord.close().await;
        drop(coord);
        timeout(Duration::from_secs(15), async {
            while s.next_event().await.is_some() {}
        })
        .await
        .expect("the session ends");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while !h.is_closed() {
            assert!(tokio::time::Instant::now() < deadline, "still open");
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            h.report(vec![OutcomeItem {
                key: [4; 20],
                token: 9,
                outcome: Outcome::Lost { reason: 1 },
            }])
            .await,
            Err(ClientError::Closed)
        ));
    }
}
