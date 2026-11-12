//! The worker's stages built without a [`Store`](lodestar_store::Store)
//! (architecture §19.4, WK-7): the fetch pool, the prepare stage and the
//! [`WorkerFunnel`](crate::worker) from a [`WorkerStages`] builder. A
//! worker needs only:
//!
//! - a [`Preparer`] built from dictionary bytes and id
//!   ([`Preparer::from_dict`]: the store's on the coordinator, WK-8's
//!   snapshot on a remote worker);
//! - a filter [`Handle`];
//! - a claim and report interface: [`Reports`] for terminal losses, and
//!   [`next_claims`] to batch claims (≤ [`CLAIM_BATCH`] keys or
//!   [`CLAIM_WAIT`], §19.2) for the wire.
//!
//! The built-in worker uses the same builder with [`Queues::local`], so its
//! path is today's: the same channels, batch sizes and direct calls on the
//! lease authority (plan Q4). A remote worker uses [`Queues::worker`]:
//! fetched info dicts and the results queued for the coordinator are
//! capped in bytes ([`byte_queue`]), so a slow link pauses fetching instead
//! of growing memory (owner §1.6.5: a worker uses ≤ 256 MB).

use std::borrow::Borrow;
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use lodestar_cluster::proto::MAX_CLAIM_BATCH;
use lodestar_cluster::{Token, WorkerId};
use lodestar_filter::Handle;
use lodestar_metainfo::InfoHash;
use lodestar_sources::pool;
use lodestar_sources::{
    Discovery, FetchedKey, Fetcher, FunnelCounts, Loss, PeerRequest, PoolConfig, PoolHandle,
    PoolIo, Shutdown,
};
use lodestar_store::{Preparer, QueuedKey, SourceId};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::JoinHandle;

use crate::coordinator::Coordinator;
use crate::gate::GateStats;
use crate::source_adult::AdultSources;
use crate::worker::{PrepareStage, WorkerFunnel, collect};
use crate::writer::WriterStats;

pub use crate::worker::{Insert, Results, WorkerLeases};

/// Keys per claim batch on the wire (§19.2: "≤ 512 or every 250 ms").
pub const CLAIM_BATCH: usize = MAX_CLAIM_BATCH;
/// The longest a claim waits for its batch to fill (§19.2).
pub const CLAIM_WAIT: Duration = Duration::from_millis(250);
/// Default cap on bytes queued for the coordinator on a remote worker
/// (plan §4.3 WK-7).
pub const OUTBOUND_BYTES: usize = 8 << 20;
/// Default cap on fetched info dicts waiting to be prepared or being
/// prepared, on a remote worker. Their prepared rows (up to about 3× a
/// dict: each file becomes a `FileInfo`) count in plan Q5's "prepare ≤ 40".
pub const FETCHED_BYTES: usize = 4 << 20;

/// How a worker reports a lease's terminal loss. The coordinator counts
/// it once, under the lease's current token (§19.3).
///
/// The built-in worker calls the lease authority directly; a remote worker
/// queues an `Outcome::Lost` for its session (WK-10). It's called on the
/// pool's and the prepare stage's tasks and must not block.
pub trait Reports: Send + Sync + 'static {
    /// The holder lost `key`. True if the loss counts (the token was
    /// current), so the caller's own counters stay in step with the
    /// funnel's. A remote worker can't know yet and says true; it must
    /// queue the report without dropping it (exactly-once, §19.3), in a
    /// queue whose length the leases it holds bound, which the worker's
    /// pool cap bounds (plan Q5).
    fn lost(&self, key: &InfoHash, token: Token, loss: Loss) -> bool;
}

impl Reports for Coordinator {
    fn lost(&self, key: &InfoHash, token: Token, loss: Loss) -> bool {
        Coordinator::lost(self, key, token, loss)
    }
}

/// Waits for the next claim and gathers more for the same batch: at most
/// [`CLAIM_BATCH`], for at most [`CLAIM_WAIT`] after the first (§19.2).
/// `None` once the channel is closed and empty. A remote worker sends each
/// batch as one `Claim` (WK-10); the built-in worker claims key by key,
/// since its lease authority is a function call away (plan Q4).
///
/// **Not cancel-safe:** dropping the future while a batch is gathering
/// (a `select!` with shutdown, a timeout) loses the claims it took from
/// the channel, so their keys are never claimed or reported. Await it to
/// the end; to stop, close the channel and drain what's left.
pub async fn next_claims<T: Send + 'static>(rx: &mut mpsc::Receiver<T>) -> Option<Vec<T>> {
    next_batch(rx, CLAIM_BATCH, CLAIM_WAIT).await
}

/// [`next_claims`] with its own limits (`max` ≥ 1). Not cancel-safe
/// either.
pub async fn next_batch<T: Send + 'static>(
    rx: &mut mpsc::Receiver<T>,
    max: usize,
    wait: Duration,
) -> Option<Vec<T>> {
    let first = rx.recv().await?;
    Some(collect(first, rx, max.max(1), wait).await)
}

/// An estimate of the memory a queued item holds, heap included. It counts
/// every buffer whose size the item's source controls; small fixed-size
/// parts may be approximated.
pub trait Weigh {
    fn weigh(&self) -> usize;
}

impl Weigh for Vec<u8> {
    fn weigh(&self) -> usize {
        size_of::<Self>() + self.capacity()
    }
}

impl Weigh for FetchedKey {
    fn weigh(&self) -> usize {
        size_of::<Self>()
            + self.fetched.info.capacity()
            + self.fetched.pex.capacity() * size_of::<std::net::SocketAddr>()
            + self.fetched.client.as_ref().map_or(0, |c| c.capacity())
    }
}

impl Weigh for Results {
    fn weigh(&self) -> usize {
        let t = &self.torrent;
        let vec = |v: &Option<Vec<u8>>| v.as_ref().map_or(0, Vec::capacity);
        let files: usize = self.files.capacity() * size_of::<lodestar_classify::FileInfo>()
            + self
                .files
                .iter()
                .map(|f| {
                    f.path.capacity() * size_of::<String>()
                        + f.path.iter().map(String::capacity).sum::<usize>()
                })
                .sum::<usize>();
        size_of::<Self>()
            + t.name.capacity()
            + t.name_raw.as_ref().map_or(0, |(v, _)| v.capacity())
            + t.files.data.capacity()
            + vec(&t.extra)
            + vec(&t.raw)
            + t.enrichment
                .as_ref()
                .map_or(0, |e| e.comp.capacity() + e.data.capacity())
            + files
            // The classification's, verdict's and trail's heap: a few
            // hundred bytes, independent of the torrent's size.
            + 512
    }
}

/// A queue capped by the bytes its items hold ([`Weigh`]) as well as by
/// count. A sender waits for room; an item larger than the whole cap
/// waits for the queue to empty and then passes alone. An item's bytes are
/// held until the [`Held`] the receiver hands out is dropped, so whatever
/// the consumer is still working on counts too. At most `bytes` (≥ 1,
/// clamped to what a semaphore and a `u32` hold) and `count` (≥ 1) items.
pub fn byte_queue<T: Weigh>(bytes: usize, count: usize) -> (ByteSender<T>, ByteReceiver<T>) {
    // Each send takes its bytes as one `u32` count of permits.
    let bytes = bytes.clamp(1, Semaphore::MAX_PERMITS.min(u32::MAX as usize));
    let budget = Arc::new(Semaphore::new(bytes));
    let (tx, rx) = mpsc::channel(count.max(1));
    (
        ByteSender {
            tx,
            budget: budget.clone(),
            cap: bytes,
        },
        ByteReceiver {
            rx,
            budget,
            cap: bytes,
            _t: PhantomData,
        },
    )
}

/// The sending half of a [`byte_queue`].
#[derive(Debug)]
pub struct ByteSender<T> {
    tx: mpsc::Sender<Held<T>>,
    budget: Arc<Semaphore>,
    cap: usize,
}

impl<T> Clone for ByteSender<T> {
    fn clone(&self) -> Self {
        ByteSender {
            tx: self.tx.clone(),
            budget: self.budget.clone(),
            cap: self.cap,
        }
    }
}

/// The receiver is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

impl<T: Weigh + Send> ByteSender<T> {
    /// Waits for room, then queues `item`.
    pub async fn send(&self, item: T) -> Result<(), Closed> {
        let n = item.weigh().clamp(1, self.cap) as u32;
        let permit = self
            .budget
            .clone()
            .acquire_many_owned(n)
            .await
            .map_err(|_| Closed)?;
        self.tx
            .send(Held {
                item,
                _permit: permit,
            })
            .await
            .map_err(|_| Closed)
    }

    /// Bytes queued or held by the consumer now.
    pub fn queued_bytes(&self) -> usize {
        self.cap - self.budget.available_permits()
    }

    pub fn cap(&self) -> usize {
        self.cap
    }
}

/// The receiving half of a [`byte_queue`].
#[derive(Debug)]
pub struct ByteReceiver<T> {
    rx: mpsc::Receiver<Held<T>>,
    budget: Arc<Semaphore>,
    cap: usize,
    _t: PhantomData<T>,
}

impl<T> ByteReceiver<T> {
    pub async fn recv(&mut self) -> Option<Held<T>> {
        self.rx.recv().await
    }

    /// Bytes queued or held by the consumer now.
    pub fn queued_bytes(&self) -> usize {
        self.cap - self.budget.available_permits()
    }
}

impl<T> Drop for ByteReceiver<T> {
    /// Senders waiting for room see the queue closed.
    fn drop(&mut self) {
        self.budget.close();
    }
}

/// An item taken from a [`byte_queue`], still holding its bytes until it's
/// dropped (or [`Held::into_inner`] lets them go).
#[derive(Debug)]
pub struct Held<T> {
    item: T,
    _permit: OwnedSemaphorePermit,
}

impl<T> Held<T> {
    pub fn get(&self) -> &T {
        &self.item
    }

    /// The item; its bytes leave the queue's count.
    pub fn into_inner(self) -> T {
        self.item
    }
}

impl<T> Borrow<T> for Held<T> {
    fn borrow(&self) -> &T {
        &self.item
    }
}

/// Where a stage reads its items from: a plain channel or a [`byte_queue`].
pub trait Inbox: Send {
    type Item: Send + 'static;
    fn recv(&mut self) -> impl Future<Output = Option<Self::Item>> + Send;
    /// `Err(true)` once closed and empty, `Err(false)` while just empty.
    fn try_recv(&mut self) -> Result<Self::Item, bool>;
}

impl<T: Send + 'static> Inbox for mpsc::Receiver<T> {
    type Item = T;
    fn recv(&mut self) -> impl Future<Output = Option<T>> + Send {
        mpsc::Receiver::recv(self)
    }
    fn try_recv(&mut self) -> Result<T, bool> {
        mpsc::Receiver::try_recv(self).map_err(|e| e == mpsc::error::TryRecvError::Disconnected)
    }
}

impl<T: Send + 'static> Inbox for ByteReceiver<T> {
    type Item = Held<T>;
    fn recv(&mut self) -> impl Future<Output = Option<Held<T>>> + Send {
        self.rx.recv()
    }
    fn try_recv(&mut self) -> Result<Held<T>, bool> {
        self.rx
            .try_recv()
            .map_err(|e| e == mpsc::error::TryRecvError::Disconnected)
    }
}

/// Where the prepare stage sends its [`Results`]: the writer's channel on
/// the coordinator, a [`byte_queue`] to the session on a remote worker.
/// Waiting here passes the receiver's backpressure on to the pool.
pub trait Outbox<T>: Send + Sync + 'static {
    fn send(&self, item: T) -> impl Future<Output = Result<(), Closed>> + Send;
}

impl<T: Send + 'static> Outbox<T> for mpsc::Sender<T> {
    fn send(&self, item: T) -> impl Future<Output = Result<(), Closed>> + Send {
        let send = mpsc::Sender::send(self, item);
        async move { send.await.map_err(|_| Closed) }
    }
}

impl<T: Weigh + Send + 'static> Outbox<T> for ByteSender<T> {
    fn send(&self, item: T) -> impl Future<Output = Result<(), Closed>> + Send {
        ByteSender::send(self, item)
    }
}

/// The queues between the pool and the prepare stage, and its batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Queues {
    /// Fetched info dicts waiting to be prepared: a channel of this many,
    /// or with `fetched_bytes`, at most one in the pool's channel and the
    /// rest in a [`byte_queue`] of this many items.
    pub fetched: usize,
    /// Caps fetched info dicts, waiting or being prepared, by bytes.
    pub fetched_bytes: Option<usize>,
    /// Info dicts prepared together, off the async threads.
    pub batch_max: usize,
    pub batch_wait: Duration,
}

impl Queues {
    /// The built-in worker's, as before WK-7.
    pub const fn local() -> Queues {
        Queues {
            fetched: 64,
            fetched_bytes: None,
            batch_max: 64,
            batch_wait: Duration::from_millis(20),
        }
    }

    /// A remote worker's: fetched info dicts capped at
    /// [`FETCHED_BYTES`] whether waiting or being prepared.
    pub const fn worker() -> Queues {
        Queues {
            fetched: 64,
            fetched_bytes: Some(FETCHED_BYTES),
            batch_max: 64,
            batch_wait: Duration::from_millis(20),
        }
    }
}

impl Default for Queues {
    fn default() -> Self {
        Queues::local()
    }
}

/// Everything the fetch pool, the prepare stage and the worker's funnel are
/// built from. No [`Store`](lodestar_store::Store): the coordinator's
/// local path and a remote worker build them the same way.
pub struct WorkerStages {
    /// Who the results come from ([`WorkerId::LOCAL`] for the built-in
    /// worker).
    pub worker: WorkerId,
    pub pool: PoolConfig,
    pub fetcher: Arc<dyn Fetcher>,
    /// Peer finders' request channels.
    pub peer_finders: Vec<mpsc::Sender<PeerRequest>>,
    pub weights: HashMap<SourceId, u32>,
    /// Keys to put back in the pool (the saved fetch queue); each must
    /// have its lease in `leases`.
    pub restore: Vec<QueuedKey>,
    pub preparer: Preparer,
    pub filter: Arc<Handle>,
    pub reports: Arc<dyn Reports>,
    /// The tokens of the leases this worker holds; whoever claims inserts.
    pub leases: Arc<WorkerLeases>,
    /// Progress stages the pool counts (the harvest was counted at grant).
    pub funnel: Arc<FunnelCounts>,
    pub gate: Arc<GateStats>,
    /// The prepare stage's `blocked` and `invalid` counters.
    pub stats: Arc<WriterStats>,
    pub adult: AdultSources,
    pub queues: Queues,
}

impl std::fmt::Debug for WorkerStages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerStages")
            .field("worker", &self.worker)
            .field("queues", &self.queues)
            .finish_non_exhaustive()
    }
}

/// The stages, running.
#[derive(Debug)]
pub struct Running {
    pub pool: PoolHandle,
    /// Ends with the pool's queue, to save.
    pub pool_task: JoinHandle<Vec<QueuedKey>>,
    /// Ends when the pool's output closes (and, with a byte cap, its
    /// forwarder) or `out` is closed.
    pub prepare: JoinHandle<()>,
}

impl WorkerStages {
    /// Starts the pool on `discoveries` and the prepare stage into `out`.
    /// Must be called in a Tokio runtime.
    ///
    /// `out` must live as long as the stages: once it's closed the prepare
    /// stage ends, and keys fetched after that keep their leases until the
    /// coordinator expires them. A remote worker keeps one outbound queue
    /// across session reconnects (or spools), never one per session.
    pub fn spawn<O: Outbox<Results>>(
        self,
        discoveries: mpsc::Receiver<Discovery>,
        shutdown: Shutdown,
        out: O,
    ) -> Running {
        let q = self.queues;
        let fetched = if q.fetched_bytes.is_some() {
            1
        } else {
            q.fetched
        };
        let (fetched_tx, fetched_rx) = mpsc::channel(fetched.max(1));
        let (pool, pool_task) = pool::spawn(
            self.pool,
            self.fetcher,
            PoolIo {
                discoveries,
                peer_finders: self.peer_finders,
                out: fetched_tx,
                funnel: Arc::new(WorkerFunnel {
                    counts: self.funnel,
                    reports: self.reports.clone(),
                    leases: self.leases.clone(),
                    gate: self.gate.clone(),
                }),
                shutdown,
            },
            self.weights,
            self.restore,
        );
        let stage = PrepareStage {
            worker: self.worker,
            preparer: self.preparer,
            filter: self.filter,
            reports: self.reports,
            leases: self.leases,
            stats: self.stats,
            gate: self.gate,
            adult: self.adult,
            batch_max: q.batch_max.max(1),
            batch_wait: q.batch_wait,
        };
        let prepare = match q.fetched_bytes {
            None => tokio::spawn(stage.run(fetched_rx, out)),
            Some(bytes) => {
                let (tx, rx) = byte_queue(bytes, q.fetched);
                let forward = tokio::spawn(forward(fetched_rx, tx));
                tokio::spawn(async move {
                    stage.run(rx, out).await;
                    // `out` closed: nothing more is taken from the pool.
                    forward.abort();
                })
            }
        };
        Running {
            pool,
            pool_task,
            prepare,
        }
    }
}

impl Results {
    /// The record WK-6's `Results` codec sends for this torrent
    /// ([`crate::wire::ResultsBatch`]). The coordinator re-judges at
    /// commit, so only the verdict's action and rule version travel.
    pub fn into_record(self) -> crate::wire::ResultRecord {
        crate::wire::ResultRecord {
            key: self.key,
            token: self.token,
            torrent: self.torrent,
            sources: self.sources,
            metadata_at: self.metadata_at,
            action: self.verdict.decision.map(|d| d.action),
            rule_version: self.version,
            fix: self.fix,
            private_source: self.private_source,
            source_adult: self.source_adult,
        }
    }
}

/// Moves the pool's fetched dicts into the byte-capped queue, one at a
/// time, so the pool waits while the queue is full.
async fn forward(mut from: mpsc::Receiver<FetchedKey>, to: ByteSender<FetchedKey>) {
    while let Some(fk) = from.recv().await {
        if to.send(fk).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct W(usize);
    impl Weigh for W {
        fn weigh(&self) -> usize {
            self.0
        }
    }

    /// A batch is sent full at once, or after the window from its first
    /// key, and never holds more than the limit (paused time).
    #[tokio::test(start_paused = true)]
    async fn claims_batch_at_512_or_250_ms() {
        let (tx, mut rx) = mpsc::channel(2048);
        for i in 0..600u32 {
            tx.send(i).await.unwrap();
        }
        let start = tokio::time::Instant::now();
        let full = next_claims(&mut rx).await.unwrap();
        assert_eq!(full.len(), CLAIM_BATCH);
        assert_eq!(start.elapsed(), Duration::ZERO, "a full batch doesn't wait");
        let rest = next_claims(&mut rx).await.unwrap();
        assert_eq!(rest.len(), 88);
        assert_eq!(rest[0], 512);
        assert_eq!(
            start.elapsed(),
            CLAIM_WAIT,
            "a short batch waits its window"
        );

        // Keys arriving inside the window join it; one after it starts the
        // next batch.
        let late = tx.clone();
        tokio::spawn(async move {
            late.send(1).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            late.send(2).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            late.send(3).await.unwrap();
        });
        let start = tokio::time::Instant::now();
        assert_eq!(next_claims(&mut rx).await.unwrap(), vec![1, 2]);
        assert_eq!(start.elapsed(), CLAIM_WAIT);
        assert_eq!(next_claims(&mut rx).await.unwrap(), vec![3]);
        drop(tx);
        assert_eq!(next_claims(&mut rx).await, None);
    }

    /// The queue never holds more than its cap; what the consumer holds
    /// still counts; an item over the cap passes alone once it's empty.
    #[tokio::test(start_paused = true)]
    async fn a_byte_queue_holds_its_cap() {
        let (tx, mut rx) = byte_queue::<W>(1000, 64);
        for _ in 0..4 {
            tx.send(W(250)).await.unwrap();
        }
        assert_eq!(tx.queued_bytes(), 1000);
        let blocked = tokio::spawn({
            let tx = tx.clone();
            async move { tx.send(W(1)).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!blocked.is_finished(), "a full queue makes the sender wait");
        let held = rx.recv().await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!blocked.is_finished(), "a held item still counts");
        drop(held);
        blocked.await.unwrap().unwrap();
        assert_eq!(rx.queued_bytes(), 751);

        let big = tokio::spawn({
            let tx = tx.clone();
            async move { tx.send(W(5000)).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!big.is_finished());
        for _ in 0..4 {
            drop(rx.recv().await.unwrap());
        }
        big.await.unwrap().unwrap();
        assert_eq!(rx.queued_bytes(), 1000, "alone, at the cap");
        let big = rx.recv().await.unwrap().into_inner();
        assert_eq!(big.0, 5000);
        assert_eq!(rx.queued_bytes(), 0);

        // A sender waiting when the receiver goes sees it closed.
        for _ in 0..4 {
            tx.send(W(250)).await.unwrap();
        }
        let waiting = tokio::spawn({
            let tx = tx.clone();
            async move { tx.send(W(1)).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        drop(rx);
        assert_eq!(waiting.await.unwrap(), Err(Closed));
        assert_eq!(tx.send(W(1)).await, Err(Closed));
    }
}
