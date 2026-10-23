//! `GET /events` (§9): the server-sent event stream. New torrents (from
//! the live indexer's commit hook), crawler stats and rebuild progress
//! now; alerts (B9) and Insights deltas (B11a) are typed and reserved.
//!
//! **One hub per process.** [`EventHub::publish`] gives each event the id
//! `<epoch>-<seq>` (a random per-process epoch, a monotonic sequence),
//! renders its frame once, keeps it in a ring of the last
//! [`EventsConfig::ring`] events for `Last-Event-ID` reconnects, and
//! broadcasts it.
//!
//! **Per client,** a bounded broadcast receiver (shared buffer of
//! [`EventsConfig::lag`] events) and a small queue to the socket. A client
//! that falls behind gets one `resync` and carries on from live: it's
//! never disconnected for it, and nothing grows per client.
//!
//! **Caps:** [`EventsConfig::max_streams`] in all and
//! [`EventsConfig::max_per_session`] per session; past either, a 429
//! with `Retry-After`. A slot is released when the response body is
//! dropped (the client went away).
//!
//! **Transport:** `text/event-stream`, `Cache-Control: no-store`,
//! `X-Accel-Buffering: no`, `retry:` first, an SSE comment as a heartbeat.
//! The router has no response-compression layer (static assets are
//! precompressed at build time, `web.rs`), so each event is written as it
//! comes; a compression layer added later must skip `text/event-stream`.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Extension, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use lodestar_search::{CommitHook, Committed, RebuildProgress, SearchHandle};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use utoipa::OpenApi;

use crate::dto::error::ErrorEnvelope;
use crate::dto::events::{
    CrawlerEvent, Event, EventsQuery, RebuildEvent, RebuildPhase, ResyncEvent, ResyncReason, Topic,
    TorrentsEvent,
};
use crate::error::{ApiError, ApiResult};
use crate::search::AdultVisibility;
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(events), components(schemas(Event, Topic)))]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    auth::protected(Router::new().route("/events", get(events)))
}

/// The hub's sizes and intervals.
#[derive(Debug, Clone)]
pub struct EventsConfig {
    /// Events kept for `Last-Event-ID` replay.
    pub ring: usize,
    /// How far a client may fall behind live before it gets a `resync`:
    /// the broadcast buffer, shared by every client.
    pub lag: usize,
    /// Frames queued to one client's socket.
    pub queue: usize,
    /// An SSE comment this often.
    pub heartbeat: Duration,
    /// The `retry:` hint.
    pub retry: Duration,
    /// A `crawler` snapshot this often, while anyone subscribes.
    pub crawler_every: Duration,
    /// Rebuild progress is checked this often, while anyone subscribes.
    pub rebuild_every: Duration,
    /// Concurrent streams in all...
    pub max_streams: usize,
    /// ...and per session (the UI opens one per tab).
    pub max_per_session: usize,
}

impl Default for EventsConfig {
    fn default() -> Self {
        EventsConfig {
            ring: 512,
            lag: 256,
            queue: 16,
            heartbeat: Duration::from_secs(15),
            retry: Duration::from_secs(5),
            crawler_every: Duration::from_secs(5),
            rebuild_every: Duration::from_secs(5),
            max_streams: 64,
            max_per_session: 4,
        }
    }
}

/// The crawler's counters, read the way the status page and `/metrics`
/// read them. A closure returning a [`CrawlerEvent`] is one.
pub trait CrawlerSource: Send + Sync {
    fn snapshot(&self) -> CrawlerEvent;
}

impl<F: Fn() -> CrawlerEvent + Send + Sync> CrawlerSource for F {
    fn snapshot(&self) -> CrawlerEvent {
        self()
    }
}

/// What the hub publishes from. Each is optional (tests, search off).
#[derive(Default, Clone)]
pub struct EventSources {
    /// The serving index's commit hook: `torrents`.
    pub commits: Option<Arc<CommitHook>>,
    /// `crawler`.
    pub crawler: Option<Arc<dyn CrawlerSource>>,
    /// The serving handle's rebuild progress: `rebuild`.
    pub rebuild: Option<SearchHandle>,
    /// "Show adult content", read live when a `torrents` event is made:
    /// the API state's own [`AdultVisibility`]. The default hides Adult.
    pub adult: AdultVisibility,
}

impl std::fmt::Debug for EventSources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSources")
            .field("commits", &self.commits)
            .field("crawler", &self.crawler.is_some())
            .field("rebuild", &self.rebuild)
            .field("adult", &self.adult)
            .finish()
    }
}

/// One published event, its frame rendered once for every client.
#[derive(Debug)]
struct Stored {
    seq: u64,
    topic: Topic,
    frame: Bytes,
    /// A `torrents` event made while Adult was shown: its count may
    /// include Adult and it carries a real `max_id`. While Adult is
    /// hidden it's withheld from clients, replay included.
    adult_shown: bool,
}

#[derive(Debug, Default)]
struct Ring {
    /// The last sequence number given out (0: none yet).
    seq: u64,
    events: VecDeque<Arc<Stored>>,
}

#[derive(Debug, Default)]
struct Slots {
    total: usize,
    per_session: HashMap<u64, usize>,
}

/// Commits reported by the indexer thread, coalesced: the sink adds to
/// counters under a lock held for a few additions (never across a wait
/// or an allocation) and wakes the hub, so it never blocks on a client,
/// and commits close together become one `torrents` event without
/// losing any count.
#[derive(Debug, Default)]
struct CommitFeed {
    pending: Mutex<Pending>,
    wake: Notify,
}

/// What's been reported since the last [`CommitFeed::take`]. One lock
/// keeps the counts and the watermarks consistent: a take never sees a
/// commit's Adult count without its total, or the reverse.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Pending {
    /// Torrents new to the index.
    added: u64,
    /// How many of `added` are Adult.
    adult: u64,
    /// The highest torrent id searchable; never reset.
    max_id: i64,
}

impl CommitFeed {
    fn report(&self, c: Committed) {
        {
            let mut p = lock(&self.pending);
            p.added += c.docs_added;
            p.adult += c.adult_added;
            p.max_id = p.max_id.max(c.max_torrent_id);
        }
        self.wake.notify_one();
    }

    /// What's been reported since the last take, as one event, made
    /// under the visibility `adult_shown` has now. While Adult is hidden,
    /// its torrents are left out of `added` (§20.9) and dropped, so
    /// turning it on later doesn't announce them, and `max_id` is null:
    /// ids are sequential, so any real one would tell, in its gap from
    /// the last, how many hidden torrents arrived. When nothing visible
    /// is new (nothing at all, or only hidden Adult), there's no event.
    fn take(&self, adult_shown: bool) -> Option<TorrentsEvent> {
        let mut p = lock(&self.pending);
        let (added, max_id) = if adult_shown {
            (p.added, Some(p.max_id))
        } else {
            (p.added.saturating_sub(p.adult), None)
        };
        (p.added, p.adult) = (0, 0);
        (added > 0).then_some(TorrentsEvent { added, max_id })
    }
}

/// The SSE broadcast hub. Build it with [`EventHub::start`] inside a
/// tokio runtime; [`EventHub::close`] ends every stream (graceful
/// shutdown) and stops its tasks.
pub struct EventHub {
    config: EventsConfig,
    epoch: String,
    ring: Mutex<Ring>,
    tx: broadcast::Sender<Arc<Stored>>,
    /// Open streams per topic, which gates the periodic topics.
    listeners: [AtomicUsize; Topic::ALL.len()],
    slots: Mutex<Slots>,
    closed: watch::Sender<bool>,
    commits: Option<(Arc<CommitHook>, Arc<CommitFeed>)>,
    /// "Show adult content", for the `torrents` events and for what's
    /// replayed or delivered of them.
    adult: AdultVisibility,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for EventHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventHub")
            .field("epoch", &self.epoch)
            .field("streams", &self.streams())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a `Last-Event-ID` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastId {
    Ours(u64),
    OtherEpoch,
    Malformed,
}

/// One client's place in the stream, apart from the socket: what to
/// replay, then the live broadcast filtered to its topics.
pub(crate) struct Subscription {
    hub: Arc<EventHub>,
    topics: u8,
    pending: VecDeque<Item>,
    rx: broadcast::Receiver<Arc<Stored>>,
    /// The last sequence handed out, so nothing is delivered twice.
    last: u64,
    /// The last item was a `resync` for a withheld event, so the next
    /// withheld one needs none.
    withheld: bool,
}

/// What a subscription yields.
#[derive(Debug, Clone)]
enum Item {
    Event(Arc<Stored>),
    Resync(ResyncReason),
}

impl Drop for Subscription {
    fn drop(&mut self) {
        for t in Topic::ALL {
            if self.topics & t.bit() != 0 {
                self.hub.listeners[t as usize].fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

impl Subscription {
    /// `s`, unless it's a duplicate or on another topic.
    fn accept(&mut self, s: Arc<Stored>) -> Option<Item> {
        if s.seq <= self.last {
            return None;
        }
        self.last = s.seq;
        (self.topics & s.topic.bit() != 0).then_some(Item::Event(s))
    }

    /// Checks `item` as it's handed to the client, replayed or live, so
    /// the visibility of that moment applies: while Adult is hidden, an
    /// event made while it was shown becomes a `resync` (one for a run
    /// of them).
    fn screen(&mut self, item: Item) -> Option<Item> {
        if let Item::Event(s) = &item
            && s.adult_shown
            && !self.hub.adult.shown()
        {
            let first = !std::mem::replace(&mut self.withheld, true);
            return first.then_some(Item::Resync(ResyncReason::Visibility));
        }
        self.withheld = false;
        Some(item)
    }

    /// The next item if one is ready; `None` when there's nothing yet or
    /// the hub is gone.
    #[cfg(test)]
    fn try_next(&mut self) -> Option<Item> {
        use broadcast::error::TryRecvError;
        loop {
            let item = match self.pending.pop_front() {
                Some(item) => item,
                None => match self.rx.try_recv() {
                    Ok(s) => match self.accept(s) {
                        Some(item) => item,
                        None => continue,
                    },
                    Err(TryRecvError::Lagged(_)) => Item::Resync(ResyncReason::Lagged),
                    Err(TryRecvError::Empty | TryRecvError::Closed) => return None,
                },
            };
            if let Some(item) = self.screen(item) {
                return Some(item);
            }
        }
    }

    /// The next item; `None` once the hub is dropped.
    async fn next(&mut self) -> Option<Item> {
        use broadcast::error::RecvError;
        loop {
            let item = match self.pending.pop_front() {
                Some(item) => item,
                None => match self.rx.recv().await {
                    Ok(s) => match self.accept(s) {
                        Some(item) => item,
                        None => continue,
                    },
                    Err(RecvError::Lagged(_)) => Item::Resync(ResyncReason::Lagged),
                    Err(RecvError::Closed) => return None,
                },
            };
            if let Some(item) = self.screen(item) {
                return Some(item);
            }
        }
    }
}

/// Why a stream wasn't opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Past [`EventsConfig::max_streams`].
    Full,
    /// Past [`EventsConfig::max_per_session`].
    SessionFull,
    /// Shutting down.
    Closed,
}

/// A held stream slot, released on drop.
#[derive(Debug)]
pub struct Slot {
    hub: Arc<EventHub>,
    session: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut slots = lock(&self.hub.slots);
        slots.total -= 1;
        if let Some(n) = slots.per_session.get_mut(&self.session) {
            *n -= 1;
            if *n == 0 {
                slots.per_session.remove(&self.session);
            }
        }
    }
}

impl EventHub {
    /// Starts the hub and the tasks that publish from `sources`.
    pub fn start(config: EventsConfig, sources: EventSources) -> Arc<EventHub> {
        let (tx, _) = broadcast::channel(config.lag.max(1));
        let epoch: u64 = rand::random();
        let commits = sources
            .commits
            .clone()
            .map(|hook| (hook, Arc::new(CommitFeed::default())));
        let hub = Arc::new(EventHub {
            config,
            epoch: format!("{epoch:016x}"),
            ring: Mutex::default(),
            tx,
            listeners: Default::default(),
            slots: Mutex::default(),
            closed: watch::channel(false).0,
            commits,
            adult: sources.adult.clone(),
            tasks: Mutex::default(),
        });
        let mut tasks = Vec::new();
        if let Some((hook, feed)) = &hub.commits {
            let f = feed.clone();
            hook.set(move |c| f.report(c));
            tasks.push(tokio::spawn(forward_commits(
                Arc::downgrade(&hub),
                feed.clone(),
                hub.closed.subscribe(),
            )));
        }
        if let Some(crawler) = sources.crawler {
            tasks.push(tokio::spawn(poll_crawler(
                Arc::downgrade(&hub),
                crawler,
                hub.closed.subscribe(),
            )));
        }
        if let Some(handle) = sources.rebuild {
            tasks.push(tokio::spawn(poll_rebuild(
                Arc::downgrade(&hub),
                handle,
                hub.closed.subscribe(),
            )));
        }
        *lock(&hub.tasks) = tasks;
        hub
    }

    /// Ends every open stream, refuses new ones and stops publishing.
    /// Later calls do nothing.
    pub fn close(&self) {
        self.closed.send_replace(true);
        if let Some((hook, _)) = &self.commits {
            hook.clear();
        }
        for t in lock(&self.tasks).drain(..) {
            t.abort();
        }
    }

    fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// This process's epoch, the first half of every id.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Open streams.
    pub fn streams(&self) -> usize {
        lock(&self.slots).total
    }

    /// Open streams subscribed to `topic`.
    pub fn listeners(&self, topic: Topic) -> usize {
        self.listeners[topic as usize].load(Ordering::Relaxed)
    }

    /// Publishes `event` on its topic and returns its id. A `resync` is
    /// per client and isn't published.
    pub fn publish(&self, event: &Event) -> Option<String> {
        self.publish_tagged(event, false)
    }

    /// [`EventHub::publish`], marking whether the event was made while
    /// Adult was shown ([`Stored::adult_shown`]).
    fn publish_tagged(&self, event: &Event, adult_shown: bool) -> Option<String> {
        let topic = event.topic()?;
        let mut ring = lock(&self.ring);
        ring.seq += 1;
        let seq = ring.seq;
        let id = format!("{}-{seq}", self.epoch);
        let stored = Arc::new(Stored {
            seq,
            topic,
            frame: frame(Some(&id), event),
            adult_shown,
        });
        ring.events.push_back(stored.clone());
        while ring.events.len() > self.config.ring {
            ring.events.pop_front();
        }
        // Under the lock, so the ring and the broadcast agree on order and
        // a subscriber sees each event exactly once. It never waits: a
        // full buffer overwrites its oldest slot (those clients lag).
        let _ = self.tx.send(stored);
        Some(id)
    }

    /// Takes a stream slot for `session`.
    pub fn acquire(self: &Arc<Self>, session: u64) -> Result<Slot, Refused> {
        if self.is_closed() {
            return Err(Refused::Closed);
        }
        let mut slots = lock(&self.slots);
        if slots.total >= self.config.max_streams {
            return Err(Refused::Full);
        }
        let mine = slots.per_session.entry(session).or_insert(0);
        if *mine >= self.config.max_per_session {
            return Err(Refused::SessionFull);
        }
        *mine += 1;
        slots.total += 1;
        Ok(Slot {
            hub: self.clone(),
            session,
        })
    }

    fn parse_last(&self, last: &str) -> LastId {
        let Some((epoch, seq)) = last.split_once('-') else {
            return LastId::Malformed;
        };
        let wellformed = epoch.len() == 16
            && epoch.bytes().all(|b| b.is_ascii_hexdigit())
            && !seq.is_empty()
            && seq.bytes().all(|b| b.is_ascii_digit());
        if !wellformed {
            return LastId::Malformed;
        }
        if epoch != self.epoch {
            return LastId::OtherEpoch;
        }
        seq.parse().map_or(LastId::Malformed, LastId::Ours)
    }

    /// Subscribes to `topics`, resuming after `last` (a `Last-Event-ID`)
    /// when it can: the events after it in the ring, then live. When it
    /// can't, a `resync` first.
    fn subscribe(self: &Arc<Self>, topics: u8, last: Option<&str>) -> Subscription {
        let last = last.map(|l| self.parse_last(l));
        let ring = lock(&self.ring);
        let rx = self.tx.subscribe();
        let mut pending = VecDeque::new();
        let oldest = ring.events.front().map_or(ring.seq + 1, |s| s.seq);
        match last {
            None => {}
            Some(LastId::Ours(seq)) if seq > ring.seq => {
                pending.push_back(Item::Resync(ResyncReason::Malformed));
            }
            // Something after it has left the ring.
            Some(LastId::Ours(seq)) if seq + 1 < oldest => {
                pending.push_back(Item::Resync(ResyncReason::Expired));
            }
            Some(LastId::Ours(seq)) => pending.extend(
                ring.events
                    .iter()
                    .filter(|s| s.seq > seq && topics & s.topic.bit() != 0)
                    .map(|s| Item::Event(s.clone())),
            ),
            Some(LastId::OtherEpoch) => pending.push_back(Item::Resync(ResyncReason::Restarted)),
            Some(LastId::Malformed) => pending.push_back(Item::Resync(ResyncReason::Malformed)),
        }
        // Everything up to here is in the ring snapshot or before it; the
        // receiver gets only later events.
        let last = ring.seq;
        drop(ring);
        for t in Topic::ALL {
            if topics & t.bit() != 0 {
                self.listeners[t as usize].fetch_add(1, Ordering::Relaxed);
            }
        }
        Subscription {
            hub: self.clone(),
            topics,
            pending,
            rx,
            last,
            withheld: false,
        }
    }

    /// The response body for one client: `retry:`, then its subscription's
    /// frames with heartbeats between, until it goes away, the hub closes
    /// or `signed_in` (asked at each heartbeat) says its session has
    /// ended. `slot` is released when the body is dropped.
    fn stream(
        self: &Arc<Self>,
        slot: Slot,
        topics: u8,
        last: Option<&str>,
        signed_in: impl Fn() -> bool + Send + 'static,
    ) -> Body {
        let mut sub = self.subscribe(topics, last);
        let (out, rx) = mpsc::channel::<Bytes>(self.config.queue.max(1));
        let mut closed = self.closed.subscribe();
        let heartbeat = self.config.heartbeat;
        let retry = Bytes::from(format!("retry: {}\n\n", self.config.retry.as_millis()));
        tokio::spawn(async move {
            // Gives up when the hub closes while the socket is full.
            let send = |frame: Bytes, closed: &watch::Receiver<bool>| {
                let out = out.clone();
                let mut closed = closed.clone();
                async move {
                    tokio::select! {
                        biased;
                        _ = closed.wait_for(|c| *c) => false,
                        sent = out.send(frame) => sent.is_ok(),
                    }
                }
            };
            if !send(retry, &closed).await {
                return;
            }
            let start = tokio::time::Instant::now() + heartbeat;
            let mut beat = tokio::time::interval_at(start, heartbeat);
            beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let frame = tokio::select! {
                    biased;
                    _ = closed.wait_for(|c| *c) => break,
                    () = out.closed() => break,
                    item = sub.next() => match item {
                        Some(Item::Event(s)) => s.frame.clone(),
                        Some(Item::Resync(reason)) => {
                            frame(None, &Event::Resync(ResyncEvent { reason }))
                        }
                        None => break,
                    },
                    _ = beat.tick() => {
                        // Expired, or the password changed (which ends
                        // every other session): the stream ends with it.
                        if !signed_in() {
                            break;
                        }
                        Bytes::from_static(b": keep-alive\n\n")
                    }
                };
                if !send(frame, &closed).await {
                    break;
                }
            }
        });
        Body::from_stream(Frames { rx, _slot: slot })
    }
}

impl Drop for EventHub {
    fn drop(&mut self) {
        self.close();
    }
}

/// An SSE frame: `id:` (none for `resync`), `event:`, `data:`.
fn frame(id: Option<&str>, event: &Event) -> Bytes {
    let mut s = String::new();
    if let Some(id) = id {
        s.push_str("id: ");
        s.push_str(id);
        s.push('\n');
    }
    s.push_str("event: ");
    s.push_str(event.name());
    // Compact JSON has no newlines, so it's one `data:` line.
    s.push_str("\ndata: ");
    s.push_str(&event.data());
    s.push_str("\n\n");
    Bytes::from(s)
}

/// The response body: frames from the client's task, holding its slot.
struct Frames {
    rx: mpsc::Receiver<Bytes>,
    _slot: Slot,
}

impl futures_core::Stream for Frames {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|f| f.map(Ok))
    }
}

/// Turns reported commits into `torrents` events, under the visibility
/// of the moment each is made.
async fn forward_commits(
    hub: std::sync::Weak<EventHub>,
    feed: Arc<CommitFeed>,
    mut closed: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            _ = closed.wait_for(|c| *c) => return,
            () = feed.wake.notified() => {}
        }
        let Some(hub) = hub.upgrade() else { return };
        // Read outside the feed's lock: it may take the settings' own.
        let shown = hub.adult.shown();
        if let Some(event) = feed.take(shown) {
            hub.publish_tagged(&Event::Torrents(event), shown);
        }
    }
}

/// Publishes a crawler snapshot every `crawler_every` while anyone
/// listens.
async fn poll_crawler(
    hub: std::sync::Weak<EventHub>,
    crawler: Arc<dyn CrawlerSource>,
    mut closed: watch::Receiver<bool>,
) {
    let every = match hub.upgrade() {
        Some(h) => h.config.crawler_every,
        None => return,
    };
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = closed.wait_for(|c| *c) => return,
            _ = tick.tick() => {}
        }
        let Some(hub) = hub.upgrade() else { return };
        if hub.listeners(Topic::Crawler) > 0 {
            hub.publish(&Event::Crawler(crawler.snapshot()));
        }
    }
}

fn rebuild_event(p: &RebuildProgress) -> RebuildEvent {
    use lodestar_search::RebuildPhase as P;
    RebuildEvent {
        done: p.done,
        error: p.error.clone(),
        from: p.from.to_string(),
        phase: match p.phase {
            P::Building => RebuildPhase::Building,
            P::Finishing => RebuildPhase::Finishing,
            P::Swapping => RebuildPhase::Swapping,
            P::Failed => RebuildPhase::Failed,
        },
        to: p.to.to_string(),
        total: p.total,
        unavailable: p.unavailable.clone(),
    }
}

/// Publishes rebuild progress when it changes, and `done` once the
/// rebuild is over, while anyone listens.
async fn poll_rebuild(
    hub: std::sync::Weak<EventHub>,
    handle: SearchHandle,
    mut closed: watch::Receiver<bool>,
) {
    let every = match hub.upgrade() {
        Some(h) => h.config.rebuild_every,
        None => return,
    };
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The last state published.
    let mut last: Option<RebuildEvent> = None;
    loop {
        tokio::select! {
            biased;
            _ = closed.wait_for(|c| *c) => return,
            _ = tick.tick() => {}
        }
        let Some(hub) = hub.upgrade() else { return };
        if hub.listeners(Topic::Rebuild) == 0 {
            last = None;
            continue;
        }
        let now = match handle.progress() {
            Some(p) => Some(rebuild_event(&p)),
            None => last
                .take()
                .filter(|l| l.phase != RebuildPhase::Done)
                .map(|l| RebuildEvent {
                    phase: RebuildPhase::Done,
                    done: l.done.max(l.total),
                    error: None,
                    ..l
                }),
        };
        if let Some(now) = now
            && last.as_ref() != Some(&now)
        {
            hub.publish(&Event::Rebuild(now.clone()));
            last = Some(now);
        }
    }
}

/// The topics in `?topics=`, as a bit set; all when absent.
fn parse_topics(raw: Option<&str>) -> ApiResult<u8> {
    let Some(raw) = raw else {
        return Ok(Topic::ALL.iter().fold(0, |acc, t| acc | t.bit()));
    };
    let mut set = 0;
    for name in raw.split(',') {
        let topic = Topic::parse(name).ok_or_else(|| {
            let known: Vec<&str> = Topic::ALL.iter().map(|t| t.name()).collect();
            // Echo at most 64 characters of what was sent.
            let shown: String = name.chars().take(64).collect();
            ApiError::bad_request(
                "unknown_topic",
                format!(
                    "Unknown topic {shown:?}; use a comma-separated list of {}.",
                    known.join(", ")
                ),
            )
            .field("topics")
        })?;
        set |= topic.bit();
    }
    Ok(set)
}

/// `?topics=`, given at most once.
fn topics_param(query: Option<&str>) -> ApiResult<Option<String>> {
    let mut values = form_urlencoded::parse(query.unwrap_or("").as_bytes())
        .filter(|(k, _)| k == "topics")
        .map(|(_, v)| v.into_owned());
    let first = values.next();
    if values.next().is_some() {
        return Err(ApiError::bad_request(
            "bad_parameter",
            "Give `topics` once, as a comma-separated list.",
        )
        .field("topics"));
    }
    Ok(first)
}

/// The session, for the per-session cap. The guard has already checked
/// it, so a request here always has one.
fn session_of(headers: &HeaderMap) -> u64 {
    let session = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| auth::cookie_value(c, auth::COOKIE))
        .unwrap_or("");
    let d = Sha256::digest(session.as_bytes());
    u64::from_le_bytes(d[..8].try_into().expect("8 bytes"))
}

/// The live event stream.
///
/// Each frame is `id: <epoch>-<seq>` (none on `resync`), `event: <name>`
/// and `data: <json>`, the payload of the `Event` variant of that name.
/// Send the last id back as `Last-Event-ID` (`EventSource` does) to
/// replay what was missed; when that can't be done exactly (a restart,
/// too long ago, or an id that isn't ours), a `resync` comes first. A
/// client that falls behind gets a `resync` and carries on from live. A
/// comment every 15 s keeps the connection open; `retry:` is 5 s.
#[utoipa::path(
    operation_id = "getEvents",
    get,
    path = "/events",
    tag = "events",
    params(
        EventsQuery,
        ("Last-Event-ID" = Option<String>, Header, description = "The last `id:` received, to resume after it."),
    ),
    responses(
        (
            status = 200,
            description = "The stream: one `Event` per frame (`event:` names the variant, `data:` holds its payload).",
            content_type = "text/event-stream",
            body = Event,
            headers(
                ("Cache-Control" = String, description = "`no-store`."),
                ("X-Accel-Buffering" = String, description = "`no`, so a proxy doesn't buffer it."),
            )
        ),
        (status = 400, description = "An unknown topic (`unknown_topic`, `field: \"topics\"`).", body = ErrorEnvelope),
        (status = 401, description = "No session (`login_required`).", body = ErrorEnvelope),
        (
            status = 429,
            description = "Too many open streams, in all or for this session (`too_many_streams`).",
            body = ErrorEnvelope,
            headers(("Retry-After" = u32, description = "Seconds to wait."))
        ),
        (status = 503, description = "Not served here, or shutting down (`events_unavailable`).", body = ErrorEnvelope),
    )
)]
pub(crate) async fn events(
    State(s): State<ApiState>,
    hub: Option<Extension<Arc<EventHub>>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let topics = parse_topics(topics_param(query.as_deref())?.as_deref())?;
    let unavailable = || {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "events_unavailable",
            "Live updates aren't available right now.",
        )
    };
    let Some(Extension(hub)) = hub else {
        return Err(unavailable());
    };
    let slot = match hub.acquire(session_of(&headers)) {
        Ok(slot) => slot,
        Err(Refused::Closed) => return Err(unavailable()),
        Err(Refused::Full | Refused::SessionFull) => {
            let retry = hub.config.retry.as_secs().max(1);
            let mut res = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_streams",
                "Too many live connections are open; close a tab or try again shortly.",
            )
            .into_response();
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(retry));
            return Ok(res);
        }
    };
    let last = headers.get("last-event-id").and_then(|v| v.to_str().ok());
    // The guard's check, repeated at each heartbeat on the cookie it let
    // in.
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let auth = s.auth.clone();
    let body = hub.stream(slot, topics, last, move || auth.verify(&cookie));
    Ok((
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-store"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn torrents(added: u64) -> Event {
        Event::Torrents(TorrentsEvent {
            added,
            max_id: Some(added as i64),
        })
    }

    fn crawler() -> Event {
        Event::Crawler(CrawlerEvent::default())
    }

    fn hub(config: EventsConfig) -> Arc<EventHub> {
        EventHub::start(config, EventSources::default())
    }

    fn all() -> u8 {
        parse_topics(None).unwrap()
    }

    fn seqs(sub: &mut Subscription) -> Vec<Result<u64, ResyncReason>> {
        std::iter::from_fn(|| sub.try_next())
            .map(|i| match i {
                Item::Event(s) => Ok(s.seq),
                Item::Resync(r) => Err(r),
            })
            .collect()
    }

    #[tokio::test]
    async fn ids_are_epoch_and_sequence() {
        let h = hub(EventsConfig::default());
        let a = h.publish(&torrents(1)).unwrap();
        let b = h.publish(&crawler()).unwrap();
        assert_eq!(a, format!("{}-1", h.epoch()));
        assert_eq!(b, format!("{}-2", h.epoch()));
        assert_eq!(h.epoch().len(), 16);
        assert!(
            h.publish(&Event::Resync(ResyncEvent {
                reason: ResyncReason::Lagged
            }))
            .is_none()
        );
        // Another process's epoch.
        assert_ne!(hub(EventsConfig::default()).epoch(), h.epoch());
    }

    #[tokio::test]
    async fn replay_is_exact_and_filtered() {
        let h = hub(EventsConfig::default());
        for i in 1..=6 {
            h.publish(&if i % 2 == 0 { crawler() } else { torrents(i) });
        }
        let id = |n: u64| format!("{}-{n}", h.epoch());
        let mut sub = h.subscribe(Topic::Torrents.bit(), Some(&id(2)));
        h.publish(&torrents(7));
        assert_eq!(seqs(&mut sub), [Ok(3), Ok(5), Ok(7)]);
        // From the newest: nothing to replay.
        let mut sub = h.subscribe(all(), Some(&id(7)));
        assert!(seqs(&mut sub).is_empty());
        // From before the first event.
        let mut sub = h.subscribe(all(), Some(&id(0)));
        assert_eq!(seqs(&mut sub).len(), 7);
    }

    #[tokio::test]
    async fn unusable_ids_resync_then_go_live() {
        let h = hub(EventsConfig {
            ring: 4,
            ..EventsConfig::default()
        });
        for i in 1..=10 {
            h.publish(&torrents(i));
        }
        let cases = [
            (format!("{}-5", h.epoch()), ResyncReason::Expired),
            (format!("{}-99", h.epoch()), ResyncReason::Malformed),
            (format!("{:016x}-5", 7u64), ResyncReason::Restarted),
            ("garbage".to_string(), ResyncReason::Malformed),
            ("-".to_string(), ResyncReason::Malformed),
            (format!("{}-", h.epoch()), ResyncReason::Malformed),
            (format!("{}-+5", h.epoch()), ResyncReason::Malformed),
            (
                format!("{}-99999999999999999999999", h.epoch()),
                ResyncReason::Malformed,
            ),
        ];
        for (last, reason) in cases {
            let mut sub = h.subscribe(all(), Some(&last));
            let id = h.publish(&torrents(0)).unwrap();
            let seq: u64 = id.rsplit('-').next().unwrap().parse().unwrap();
            assert_eq!(seqs(&mut sub), [Err(reason), Ok(seq)], "{last}");
        }
        // The oldest kept event's predecessor is still exact.
        let oldest = lock(&h.ring).events.front().unwrap().seq;
        let mut sub = h.subscribe(all(), Some(&format!("{}-{}", h.epoch(), oldest - 1)));
        assert_eq!(seqs(&mut sub).len(), 4);
    }

    #[tokio::test]
    async fn a_lagging_client_resyncs_and_stays_bounded() {
        let h = hub(EventsConfig {
            lag: 8,
            ..EventsConfig::default()
        });
        let mut sub = h.subscribe(all(), None);
        for i in 1..=100 {
            h.publish(&torrents(i));
        }
        assert_eq!(h.tx.len(), 8, "the buffer holds only the newest");
        let got = seqs(&mut sub);
        assert_eq!(got[0], Err(ResyncReason::Lagged));
        assert_eq!(got[1..], (93..=100).map(Ok).collect::<Vec<_>>());
        // It carries on live.
        h.publish(&torrents(101));
        assert_eq!(seqs(&mut sub), [Ok(101)]);
    }

    #[tokio::test]
    async fn caps_hold_and_slots_come_back() {
        let h = hub(EventsConfig {
            max_streams: 3,
            max_per_session: 2,
            ..EventsConfig::default()
        });
        let a1 = h.acquire(1).unwrap();
        let _a2 = h.acquire(1).unwrap();
        assert_eq!(h.acquire(1).err(), Some(Refused::SessionFull));
        let _b = h.acquire(2).unwrap();
        assert_eq!(h.acquire(3).err(), Some(Refused::Full));
        drop(a1);
        let _a = h.acquire(1).unwrap();
        assert_eq!(h.streams(), 3);
        h.close();
        assert_eq!(h.acquire(4).err(), Some(Refused::Closed));
    }

    #[tokio::test]
    async fn a_thousand_cycles_leave_nothing_held() {
        let h = hub(EventsConfig::default());
        for i in 0..1000 {
            let slot = h.acquire(i % 7).unwrap();
            let sub = h.subscribe(all(), None);
            drop((slot, sub));
        }
        assert_eq!(h.streams(), 0);
        assert!(lock(&h.slots).per_session.is_empty());
        for t in Topic::ALL {
            assert_eq!(h.listeners(t), 0);
        }
    }

    #[tokio::test]
    async fn the_commit_sink_never_waits_and_coalesces() {
        let hook = Arc::new(CommitHook::default());
        let h = EventHub::start(
            EventsConfig::default(),
            EventSources {
                commits: Some(hook.clone()),
                // Shown, so each event carries the watermark.
                adult: AdultVisibility::follow(|| true),
                ..EventSources::default()
            },
        );
        let feed = h.commits.as_ref().unwrap().1.clone();
        // Nobody reads the hub's side meanwhile: the reports still return
        // (none waits on a reader) and take no memory.
        for i in 1..=100_000 {
            feed.report(commit(1, 0, i));
        }
        let mut sub = h.subscribe(all(), None);
        let mut added = 0;
        while added < 100_000 {
            match tokio::time::timeout(Duration::from_secs(5), sub.next()).await {
                Ok(Some(Item::Event(s))) => {
                    let text = std::str::from_utf8(&s.frame).unwrap();
                    let data = text.split("data: ").nth(1).unwrap().trim();
                    let e: TorrentsEvent = serde_json::from_str(data).unwrap();
                    assert_eq!(e.max_id, Some(100_000));
                    added += e.added;
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(added, 100_000);
        // Close clears the sink, so a hook outliving the hub reports
        // nowhere.
        h.close();
        assert!(format!("{hook:?}").contains("set: false"));
    }

    #[test]
    fn topics_parse_strictly() {
        assert_eq!(
            parse_topics(Some("torrents,crawler")).unwrap(),
            Topic::Torrents.bit() | Topic::Crawler.bit()
        );
        for bad in ["", "torrents,", "nope", "torrents, crawler", "TORRENTS"] {
            let e = parse_topics(Some(bad)).unwrap_err();
            assert_eq!(
                (e.code(), e.status()),
                ("unknown_topic", StatusCode::BAD_REQUEST)
            );
            assert_eq!(e.error_body().field.as_deref(), Some("topics"), "{bad}");
        }
    }

    #[test]
    fn topics_come_once_and_echo_at_most_64_chars() {
        assert_eq!(topics_param(None).unwrap(), None);
        assert_eq!(
            topics_param(Some("x=1&topics=torrents%2Ccrawler")).unwrap(),
            Some("torrents,crawler".to_string())
        );
        for twice in ["topics=torrents&topics=crawler", "topics=a&topics=a"] {
            let e = topics_param(Some(twice)).unwrap_err();
            assert_eq!(
                (e.code(), e.status()),
                ("bad_parameter", StatusCode::BAD_REQUEST)
            );
            assert_eq!(e.error_body().field.as_deref(), Some("topics"));
        }
        let long = "é".repeat(500);
        let e = parse_topics(Some(&long)).unwrap_err();
        let message = e.error_body().message.clone();
        assert!(message.contains(&"é".repeat(64)), "{message}");
        assert!(!message.contains(&"é".repeat(65)), "{message}");
    }

    fn commit(docs_added: u64, adult_added: u64, max_torrent_id: i64) -> Committed {
        Committed {
            docs_added,
            adult_added,
            max_torrent_id,
            at: 0,
        }
    }

    fn take(feed: &CommitFeed, shown: bool) -> Option<(u64, Option<i64>)> {
        feed.take(shown).map(|e| (e.added, e.max_id))
    }

    /// A take counts under the visibility it's given, never the one at
    /// the report, and drops what it leaves out. `max_id` goes out only
    /// while Adult is shown.
    #[test]
    fn a_take_counts_under_the_visibility_of_its_moment() {
        let feed = CommitFeed::default();
        assert_eq!(take(&feed, false), None);
        // Reported while hidden, taken while shown: all of it.
        feed.report(commit(5, 2, 5));
        assert_eq!(take(&feed, true), Some((5, Some(5))));
        // Taken while hidden: Adult left out, and not sent later either.
        feed.report(commit(5, 2, 10));
        assert_eq!(take(&feed, false), Some((3, None)));
        assert_eq!(take(&feed, true), None);
        // Coalesced commits add up, each with its own Adult count.
        feed.report(commit(4, 1, 14));
        feed.report(commit(2, 2, 12));
        assert_eq!(take(&feed, false), Some((3, None)));
        // Shown again: the newest id, Adult included.
        feed.report(commit(1, 0, 15));
        assert_eq!(take(&feed, true), Some((1, Some(15))));
    }

    /// Only hidden Adult is new: no event.
    #[test]
    fn hidden_adult_alone_sends_nothing() {
        let feed = CommitFeed::default();
        feed.report(commit(1, 0, 1));
        assert_eq!(take(&feed, false), Some((1, None)));
        feed.report(commit(2, 2, 3));
        assert_eq!(take(&feed, false), None);
        // Dropped, not held back for a later toggle.
        assert_eq!(take(&feed, true), None);
        feed.report(commit(1, 0, 4));
        assert_eq!(take(&feed, false), Some((1, None)));
    }

    /// The next item a subscription yields, or `None` if none comes
    /// within `wait`.
    async fn next_item(sub: &mut Subscription, wait: Duration) -> Option<Item> {
        tokio::time::timeout(wait, sub.next()).await.ok().flatten()
    }

    fn torrents_of(item: Option<Item>) -> Option<TorrentsEvent> {
        match item? {
            Item::Event(s) => {
                let text = std::str::from_utf8(&s.frame).unwrap();
                let data = text.split("data: ").nth(1).unwrap().trim();
                Some(serde_json::from_str(data).unwrap())
            }
            other => panic!("{other:?}"),
        }
    }

    /// A hub fed by a hook, following `show`.
    fn adult_hub(show: &watch::Sender<bool>) -> (Arc<EventHub>, Arc<CommitFeed>) {
        let rx = show.subscribe();
        let h = EventHub::start(
            EventsConfig::default(),
            EventSources {
                commits: Some(Arc::new(CommitHook::default())),
                adult: AdultVisibility::follow(move || *rx.borrow()),
                ..EventSources::default()
            },
        );
        let feed = h.commits.as_ref().unwrap().1.clone();
        (h, feed)
    }

    /// Through the hub's own task, the setting read live from the same
    /// kind of reader the API state holds.
    #[tokio::test]
    async fn the_hub_leaves_hidden_adult_out_of_torrents_events() {
        let (show, _) = watch::channel(false);
        let (h, feed) = adult_hub(&show);
        let mut sub = h.subscribe(all(), None);
        let long = Duration::from_secs(5);
        let short = Duration::from_millis(200);
        // Hidden (the default): 3 non-adult and 2 adult send 3, no id.
        feed.report(commit(5, 2, 5));
        let e = torrents_of(next_item(&mut sub, long).await).unwrap();
        assert_eq!((e.added, e.max_id), (3, None));
        let frame = match next_item(
            &mut h.subscribe(all(), Some(&format!("{}-0", h.epoch()))),
            long,
        )
        .await
        {
            Some(Item::Event(s)) => String::from_utf8(s.frame.to_vec()).unwrap(),
            other => panic!("{other:?}"),
        };
        assert!(
            frame.ends_with("data: {\"added\":3,\"max_id\":null}\n\n"),
            "{frame}"
        );
        // Shown: the same commit sends 5 and the newest id.
        show.send_replace(true);
        feed.report(commit(5, 2, 10));
        let e = torrents_of(next_item(&mut sub, long).await).unwrap();
        assert_eq!((e.added, e.max_id), (5, Some(10)));
        // Hidden again: an all-Adult commit sends nothing...
        show.send_replace(false);
        feed.report(commit(2, 2, 12));
        assert!(next_item(&mut sub, short).await.is_none());
        // ...and the next visible one carries no id.
        feed.report(commit(1, 0, 13));
        let e = torrents_of(next_item(&mut sub, long).await).unwrap();
        assert_eq!((e.added, e.max_id), (1, None));
        h.close();
    }

    /// While Adult is hidden, no client is handed an event made while it
    /// was shown (each carries a real `max_id`), replayed or live: a
    /// `resync` (one per run) takes their place. Shown again, replay is
    /// exact.
    #[tokio::test]
    async fn events_made_while_adult_was_shown_are_withheld_once_hidden() {
        let (show, _) = watch::channel(true);
        let (h, feed) = adult_hub(&show);
        let mut live = h.subscribe(all(), None);
        let long = Duration::from_secs(5);
        let mut ids = Vec::new();
        let mut publish = async |c, ids: &mut Vec<String>| {
            feed.report(c);
            let item = next_item(&mut live, long).await.unwrap();
            let Item::Event(s) = &item else {
                panic!("{item:?}")
            };
            ids.push(format!("{}-{}", h.epoch(), s.seq));
        };
        // Shown: three events, none with Adult in its count.
        for i in 1..=3 {
            publish(commit(1, 0, i), &mut ids).await;
        }
        // Hidden: one more.
        show.send_replace(false);
        publish(commit(2, 1, 5), &mut ids).await;
        // A tab reconnects from the first id: one `resync` for the shown
        // run, then the hidden event, and no frame carries an id.
        let mut back = h.subscribe(all(), Some(&ids[0]));
        let got: Vec<_> = std::iter::from_fn(|| back.try_next()).collect();
        assert!(
            matches!(
                got.as_slice(),
                [Item::Resync(ResyncReason::Visibility), Item::Event(_)]
            ),
            "{got:?}"
        );
        let e = torrents_of(got.into_iter().nth(1)).unwrap();
        assert_eq!((e.added, e.max_id), (1, None));
        // Live: a frame still buffered for a client is checked when it
        // takes it.
        show.send_replace(true);
        let mut idle = h.subscribe(all(), Some(&ids[3]));
        publish(commit(1, 0, 6), &mut ids).await;
        show.send_replace(false);
        assert!(matches!(
            idle.try_next(),
            Some(Item::Resync(ResyncReason::Visibility))
        ));
        assert!(idle.try_next().is_none());
        // Shown again: replay is exact, ids included.
        show.send_replace(true);
        let mut again = h.subscribe(all(), Some(&ids[0]));
        let got: Vec<_> = std::iter::from_fn(|| again.try_next()).collect();
        let ids_sent: Vec<_> = got
            .into_iter()
            .map(|i| torrents_of(Some(i)).unwrap().max_id)
            .collect();
        assert_eq!(ids_sent, [Some(2), Some(3), None, Some(6)]);
        h.close();
    }

    #[test]
    fn hub_sources_default_to_adult_hidden() {
        assert!(!EventSources::default().adult.shown());
    }

    /// A step of the property test.
    #[derive(Debug, Clone)]
    enum Op {
        /// Publish on topic `n % 2` (torrents or crawler).
        Publish(u8),
        /// Client `c` reads up to `n` items.
        Read(usize, u8),
        /// Client `c` drops its connection and reconnects with the last
        /// id it saw.
        Reconnect(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => any::<u8>().prop_map(Op::Publish),
            3 => (0..3usize, 0..12u8).prop_map(|(c, n)| Op::Read(c, n)),
            1 => (0..3usize).prop_map(Op::Reconnect),
        ]
    }

    /// One simulated client: what it subscribed to and what it has seen.
    struct Client {
        topics: u8,
        sub: Subscription,
        /// The last id (sequence) received, as `EventSource` keeps it.
        last: Option<u64>,
        /// Items seen since a gap could last have been announced: each
        /// must be the next event on its topics.
        expect_from: u64,
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Whatever the interleaving of publishes, reads and reconnects
        /// (with small buffers, so lagging and expiry happen), no client
        /// gets an event twice or out of order, and every event it misses
        /// on its topics is announced by a `resync` before the next one.
        #[test]
        fn delivery_is_ordered_unique_and_gaps_are_announced(
            ops in prop::collection::vec(op(), 1..200),
            ring in 1usize..12,
            lag in 1usize..8,
        ) {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let _guard = rt.enter();
            let h = hub(EventsConfig { ring, lag, ..EventsConfig::default() });
            let sets = [Topic::Torrents.bit(), Topic::Crawler.bit(), all()];
            // Every published event's topic, by sequence.
            let mut published: Vec<Topic> = vec![Topic::Alerts];
            let mut clients: Vec<Client> = sets.iter().map(|&topics| Client {
                topics,
                sub: h.subscribe(topics, None),
                last: None,
                expect_from: 1,
            }).collect();
            let epoch = h.epoch().to_string();
            for op in ops {
                match op {
                    Op::Publish(n) => {
                        let e = if n % 2 == 0 { torrents(1) } else { crawler() };
                        published.push(e.topic().unwrap());
                        h.publish(&e);
                    }
                    Op::Read(c, n) => {
                        let cl = &mut clients[c];
                        for _ in 0..n {
                            let Some(item) = cl.sub.try_next() else { break };
                            match item {
                                Item::Resync(_) => {
                                    // A gap is announced: anything before
                                    // what comes next may be missing.
                                    cl.expect_from = u64::MAX;
                                }
                                Item::Event(s) => {
                                    prop_assert!(cl.topics & s.topic.bit() != 0);
                                    if let Some(last) = cl.last {
                                        prop_assert!(s.seq > last, "duplicate or out of order");
                                    }
                                    if cl.expect_from != u64::MAX {
                                        // No event on its topics was skipped.
                                        for missed in cl.expect_from..s.seq {
                                            prop_assert!(
                                                cl.topics & published[missed as usize].bit() == 0,
                                                "event {missed} skipped without a resync"
                                            );
                                        }
                                    }
                                    cl.last = Some(s.seq);
                                    cl.expect_from = s.seq + 1;
                                }
                            }
                        }
                    }
                    Op::Reconnect(c) => {
                        let cl = &mut clients[c];
                        let last = cl.last.map(|l| format!("{epoch}-{l}"));
                        cl.sub = h.subscribe(cl.topics, last.as_deref());
                        // A new connection without an id starts live.
                        if cl.last.is_none() {
                            cl.expect_from = published.len() as u64;
                        }
                    }
                }
            }
        }
    }
}
