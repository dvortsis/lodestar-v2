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
use tokio::sync::{mpsc, oneshot};
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerEvent {
    Assign(Assign),
    /// Keys granted with no request waiting: leases that expired elsewhere,
    /// or the answer to a claim whose caller timed out.
    Regranted(Vec<GrantItem>),
    /// Finish the leases held, report them, then call
    /// [`WorkerSession::drained`].
    Drain,
    /// The coordinator says this worker's clock is more than 60 s off.
    ClockSkewed {
        offset_ms: i64,
    },
    /// The session ended: stopped by the coordinator (with its reason) or
    /// the connection closed.
    Ended(Option<StopReason>),
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
}

impl Default for WorkerConfig {
    fn default() -> WorkerConfig {
        WorkerConfig {
            auto_heartbeat: true,
            reply_timeout: Duration::from_secs(15),
            event_queue: 256,
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
}

/// A live session with the coordinator.
#[derive(Debug)]
pub struct WorkerSession {
    welcome: Welcome,
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<WorkerEvent>,
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
        let leases = Arc::new(AtomicU32::new(0));
        let driver = Driver {
            link,
            clock,
            commands: command_rx,
            events: event_tx,
            leases: leases.clone(),
            held: HashMap::new(),
            claims: HashMap::new(),
            heartbeats: HashMap::new(),
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
            leases,
            reply_timeout: config.reply_timeout,
        })
    }

    pub fn welcome(&self) -> &Welcome {
        &self.welcome
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

    /// Reports terminal outcomes (sent once they're on the wire; the
    /// coordinator doesn't answer).
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

    /// Tells the coordinator the drain is done; it stops the session.
    pub async fn drained(&self) -> Result<(), ClientError> {
        self.ask(Command::Drained).await
    }

    /// The next unsolicited message, or `None` after
    /// [`WorkerEvent::Ended`].
    pub async fn next_event(&mut self) -> Option<WorkerEvent> {
        self.events.recv().await
    }
}

struct Driver {
    link: WorkerLink,
    clock: Arc<dyn Clock>,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<WorkerEvent>,
    leases: Arc<AtomicU32>,
    /// Leases held: key → fencing token.
    held: HashMap<[u8; 20], u64>,
    claims: HashMap<u64, oneshot::Sender<Vec<GrantItem>>>,
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
        let _ = self.events.send(WorkerEvent::Ended(end)).await;
    }

    fn sync_leases(&self) {
        self.leases.store(
            u32::try_from(self.held.len()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
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

    async fn beat(&mut self, reply: Option<oneshot::Sender<HeartbeatAck>>) -> Step {
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
        self.claims.retain(|_, tx| !tx.is_closed());
        self.heartbeats.retain(|_, tx| !tx.is_closed());
        match cmd {
            Command::Claim(items, reply) => {
                let batch = self.next_batch;
                self.next_batch += 1;
                self.claims.insert(batch, reply);
                self.stats.claimed += items.len() as u64;
                self.send(ToCoordinator::Claim(ClaimBatch { batch, items }))
                    .await
            }
            Command::Report(items, reply) => {
                for item in &items {
                    if self.held.get(&item.key) == Some(&item.token) {
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
            Command::Stats(stats) => {
                self.stats = Stats {
                    claimed: self.stats.claimed,
                    ..stats
                };
                Step::Continue
            }
        }
    }

    async fn event(&mut self, event: WorkerEvent) -> Step {
        match self.events.send(event).await {
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
                self.take_grants(&items);
                match self.claims.remove(&batch) {
                    Some(reply) => {
                        let _ = reply.send(items);
                        Step::Continue
                    }
                    // Its caller timed out; the leases are still ours.
                    None => self.event(WorkerEvent::Regranted(items)).await,
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
            // Later slices.
            ToWorker::HaveSnapshot(_) | ToWorker::HaveDelta(_) | ToWorker::Update(_) => {
                Step::Continue
            }
        }
    }
}
