//! A fake [`SourceCtx`] and a recorded-fixture replayer for plugin
//! contract tests (§18.9). Nothing here touches the network.
//!
//! # Fixture layout
//!
//! One directory per test case, one pair of files per HTTP exchange,
//! replayed in file-name order:
//!
//! ```text
//! fixtures/<plugin>/<case>/
//!   001.http        the request, then the response head
//!   001.body        the response body, raw bytes (optional; empty if absent)
//!   001.req.body    the expected request body (optional; POST only)
//!   002.http
//!   …
//! ```
//!
//! A `.http` file is two blocks separated by one blank line:
//!
//! ```text
//! GET https://indexer.example/api?t=search&q=ubuntu
//! Accept: application/rss+xml
//!
//! HTTP 200
//! Content-Type: application/rss+xml
//! ```
//!
//! - The first line is `METHOD URL` (GET, HEAD or POST). The URL must
//!   match exactly. Header lines after it must each appear in the request
//!   (name ignoring case, value exactly); the request may carry others.
//! - The response block starts with `HTTP <status>` and its headers, or is
//!   the single line `ERROR <message>` for a transport failure.
//! - Lines may end in `\r\n`. Bodies live in their own files so they can
//!   be binary and are stored byte for byte as recorded.
//! - Record against the real service once, strip anything personal
//!   (API keys, cookies, account names) from URLs, headers and bodies, and
//!   commit the result. A key in a URL is replaced by `REDACTED` in both
//!   the fixture and the test's config.
//! - Number the files with zero padding (`001`, `002`, …): they're
//!   sorted by name, so `10.http` would come before `2.http`.
//!
//! [`Replayer::load`] answers each request with the next exchange. For a
//! plugin that sends requests in parallel (one per indexer, say),
//! [`Replayer::load_unordered`] answers with the first unplayed exchange
//! that matches instead. Either way a request that matches nothing fails
//! with [`HttpError::Unrecorded`], and [`Replayer::finish`] reports any
//! mismatch or unplayed exchange, so a test can't pass by skipping
//! requests.
//!
//! # The plugin pattern (SRC-7 and later)
//!
//! - A plugin sends HTTP only through `ctx.kit.http`: each poll of a feed
//!   or indexer through [`Client::poll`](crate::Client::poll) with the
//!   unit's id, and follow-up requests (pages, `.torrent` files, redirects)
//!   through [`Client::send`](crate::Client::send).
//! - It waits on every timer and every send inside a `select!` with
//!   `shutdown.requested()`, so it returns within [`STOP_WITHIN`].
//! - Its contract test runs it on [`FakeCtx::with_fetch`] with a
//!   [`Replayer`], checks the discoveries, checks the poll spacing from
//!   [`Replayer::requests`] against its policy, calls
//!   [`Replayer::finish`], and ends with [`Running::stop_within`].
//!
//! # WebSocket transcripts (NOS-2b)
//!
//! A plugin that talks WebSocket is tested with a [`WsReplayer`] in place
//! of its [`WsConnect`]: one `.ws` file per connection, played in
//! file-name order (`001.ws`, `002.ws`, …), each a transcript of one
//! session:
//!
//! ```text
//! # A relay that answers one subscription, then closes.
//! CONNECT wss://relay.example/
//! > ["REQ","sub",{"kinds":[2003],"limit":500}]
//! < ["EVENT","sub",{"id":"…"}]
//! < ["EOSE","sub"]
//! CLOSE
//! ```
//!
//! - The first line is `CONNECT <url>`; the URL must match exactly.
//!   `REFUSED <message>` or `HANDSHAKE <status>` as the only line after it
//!   records a connection that failed or an upgrade the server refused.
//! - `> text` is a text message the plugin must send, exactly (`>b <hex>`
//!   for binary); `< text` is one the server sends (`<b <hex>`). The text
//!   is everything after the one space.
//! - The session ends with `CLOSE` (the server closed it), `ERROR <message>`
//!   (the connection failed), `IDLE` (the idle timeout) or `DOWN <reason>`
//!   (the kill switch). A transcript that ends without one stays open: the
//!   next receive waits for ever, as on a quiet connection.
//! - Blank lines and lines starting with `#` are ignored; lines may end in
//!   `\r\n`.
//! - The plugin's sends are matched in order. One may come before the
//!   server's messages recorded ahead of it have been received; a receive
//!   that reaches a send the plugin hasn't made waits, as a server would.
//! - The caps hold as on a real session: a message over the limit is
//!   refused when sent and ends the session when received.
//!
//! [`WsReplayer::finish`] reports a connect, a send or a receive that
//! didn't match, a transcript never connected and a step never played.
//!
//! # Time
//!
//! Contract tests run on tokio's paused clock, so a plugin's 15-minute
//! poll takes no real time. The harness's waits ([`FakeCtx::next`],
//! [`wait_for`], [`Running::stop`]) give up after [`DEADLINE`] of *real*
//! time, however far the paused clock runs meanwhile.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_store::SourceId;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};

use crate::http::{Fetch, HostLimiter, HttpError, Method, NoEgress, Request, Response};
use crate::manifest::{Policy, UserLimits};
use crate::plugin::{
    BoxFuture, Discovery, Emitter, Kit, PluginError, Shutdown, SourceCtx, SourcePlugin,
};
use crate::state::MemoryState;
use crate::ws::{self, WsConnect, WsError, WsLimits, WsMessage, WsStream};

/// How soon a plugin must return after shutdown (§18.9).
pub const STOP_WITHIN: Duration = Duration::from_secs(1);

/// How long the harness waits for anything before calling it a failure.
/// Generous so tests pass on the slowest CI runner.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// Discoveries buffered before `try_emit` starts dropping.
pub const CAPACITY: usize = 1024;

/// A [`SourceCtx`] wired to channels a test can read.
#[derive(Debug)]
pub struct FakeCtx {
    ctx: Option<SourceCtx>,
    pub discoveries: mpsc::Receiver<Discovery>,
    trigger: Arc<watch::Sender<bool>>,
    pub state: Arc<MemoryState>,
    pub limiter: HostLimiter,
}

impl FakeCtx {
    /// A context for `plugin`, paced by its own policy with no user
    /// settings and no egress. Jitter is seeded, so runs repeat.
    pub fn new(plugin: &dyn SourcePlugin, source: SourceId) -> FakeCtx {
        FakeCtx::with_fetch(plugin, source, Arc::new(NoEgress))
    }

    /// As [`new`](FakeCtx::new), with HTTP answered by `fetch` (usually a
    /// [`Replayer`]).
    pub fn with_fetch(
        plugin: &dyn SourcePlugin,
        source: SourceId,
        fetch: Arc<dyn Fetch>,
    ) -> FakeCtx {
        FakeCtx::with_limits(plugin.policy(), &UserLimits::default(), source, fetch)
    }

    pub fn with_limits(
        policy: &Policy,
        user: &UserLimits,
        source: SourceId,
        fetch: Arc<dyn Fetch>,
    ) -> FakeCtx {
        let (tx, discoveries) = mpsc::channel(CAPACITY);
        let (trigger, shutdown) = Shutdown::new();
        let state = Arc::new(MemoryState::new());
        let limiter = HostLimiter::with_seed(policy.limits(user), 0x5eed);
        let ctx = SourceCtx {
            source,
            emitter: Emitter::new(source, tx),
            peer_requests: None,
            peer_replies: None,
            shutdown,
            kit: Kit::with_limiter(state.clone(), fetch, limiter.clone()),
        };
        FakeCtx {
            ctx: Some(ctx),
            discoveries,
            trigger: Arc::new(trigger),
            state,
            limiter,
        }
    }

    /// The context to hand the plugin. Only once.
    pub fn take(&mut self) -> SourceCtx {
        self.ctx.take().expect("FakeCtx::take called twice")
    }

    /// Runs `plugin` on this context in a task.
    pub fn start<P: SourcePlugin + 'static>(&mut self, plugin: Arc<P>) -> Running {
        let ctx = self.take();
        Running {
            task: tokio::spawn(async move { plugin.run(ctx).await }),
            trigger: self.trigger.clone(),
        }
    }

    pub fn shutdown(&self) {
        let _ = self.trigger.send(true);
    }

    /// The next discovery, or `None` after [`DEADLINE`] of real time or
    /// once the plugin has dropped its emitter.
    pub async fn next(&mut self) -> Option<Discovery> {
        let start = std::time::Instant::now();
        let mut ticks = Ticks::new(DEADLINE);
        loop {
            match timeout(ticks.step(), self.discoveries.recv()).await {
                Ok(d) => return d,
                Err(_) if start.elapsed() >= DEADLINE => return None,
                Err(_) => {}
            }
        }
    }

    /// Discoveries already sent.
    pub fn drain(&mut self) -> Vec<Discovery> {
        let mut out = Vec::new();
        while let Ok(d) = self.discoveries.try_recv() {
            out.push(d);
        }
        out
    }
}

/// The harness's re-check interval: 50 ms at first, doubling to
/// `max`. On a paused clock each step lets the clock run on, and the
/// growing steps get through an hours-long back-off in a few thousand
/// cheap iterations rather than hundreds of thousands.
#[derive(Debug)]
struct Ticks {
    next: Duration,
    max: Duration,
}

impl Ticks {
    fn new(max: Duration) -> Ticks {
        Ticks {
            next: Duration::from_millis(50),
            max,
        }
    }

    fn step(&mut self) -> Duration {
        let t = self.next;
        self.next = (t * 2).min(self.max);
        t
    }
}

/// Waits until `cond` holds. Panics after [`DEADLINE`] of real time.
/// It re-checks at clock steps growing from 50 ms to 1 s, so on a paused
/// clock it may see the condition up to 1 s of clock time late.
pub async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    let mut ticks = Ticks::new(Duration::from_secs(1));
    while !cond() {
        assert!(
            start.elapsed() < DEADLINE,
            "still waiting for {what} after {DEADLINE:?}"
        );
        tokio::time::sleep(ticks.step()).await;
    }
}

/// A plugin running on a [`FakeCtx`].
#[derive(Debug)]
pub struct Running {
    task: JoinHandle<Result<(), PluginError>>,
    trigger: Arc<watch::Sender<bool>>,
}

/// How a plugin ended after [`Running::stop`].
#[derive(Debug)]
pub struct Stopped {
    pub result: Result<(), PluginError>,
    /// From the shutdown request to the plugin's return: the longer of
    /// the clock's time (paused or not) and real time, so neither a long
    /// timer nor blocking work passes unseen.
    pub took: Duration,
}

impl Running {
    /// Requests shutdown and waits up to [`DEADLINE`] for the plugin to
    /// return. `Err` if it didn't, or panicked.
    pub async fn stop(self) -> Result<Stopped, String> {
        let asked = Instant::now();
        let real = std::time::Instant::now();
        let _ = self.trigger.send(true);
        let mut task = self.task;
        let mut ticks = Ticks::new(DEADLINE);
        loop {
            match timeout(ticks.step(), &mut task).await {
                Ok(Ok(result)) => {
                    return Ok(Stopped {
                        result,
                        took: asked.elapsed().max(real.elapsed()),
                    });
                }
                Ok(Err(e)) => return Err(format!("plugin task failed: {e}")),
                Err(_) if real.elapsed() >= DEADLINE => {
                    task.abort();
                    return Err(format!("plugin still running {DEADLINE:?} after shutdown"));
                }
                Err(_) => {}
            }
        }
    }

    /// The contract check: the plugin returns `Ok` within [`STOP_WITHIN`]
    /// of shutdown.
    pub async fn stop_within(self) -> Result<(), String> {
        let stopped = self.stop().await?;
        if stopped.took > STOP_WITHIN {
            return Err(format!(
                "plugin took {:?} to stop (limit {STOP_WITHIN:?})",
                stopped.took
            ));
        }
        stopped.result.map_err(|e| format!("plugin failed: {e}"))
    }

    /// Whether the plugin has returned on its own.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// One recorded exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    /// The method and URL must match exactly; these headers must appear.
    pub request: Request,
    pub response: Result<Response, String>,
}

#[derive(Debug, Default)]
struct Log {
    /// Which exchanges have been played.
    played: Vec<bool>,
    /// The first unplayed exchange, in order.
    next: usize,
    seen: Vec<(Instant, Request)>,
    errors: Vec<String>,
}

/// Answers requests from recorded exchanges.
#[derive(Debug)]
pub struct Replayer {
    exchanges: Vec<Exchange>,
    ordered: bool,
    log: Mutex<Log>,
}

/// The largest fixture file read.
pub const MAX_FIXTURE: u64 = crate::http::MAX_BODY as u64;

impl Replayer {
    /// Plays `exchanges` in order.
    pub fn new(exchanges: Vec<Exchange>) -> Replayer {
        Replayer {
            log: Mutex::new(Log {
                played: vec![false; exchanges.len()],
                ..Log::default()
            }),
            exchanges,
            ordered: true,
        }
    }

    /// Plays `exchanges` in any order: each request takes the first
    /// unplayed exchange that matches it.
    pub fn unordered(exchanges: Vec<Exchange>) -> Replayer {
        Replayer {
            ordered: false,
            ..Replayer::new(exchanges)
        }
    }

    /// Loads a case directory to play in order (see the module docs).
    pub fn load(dir: &Path) -> Result<Replayer, String> {
        Ok(Replayer::new(load_exchanges(dir)?))
    }

    /// Loads a case directory to play in any order.
    pub fn load_unordered(dir: &Path) -> Result<Replayer, String> {
        Ok(Replayer::unordered(load_exchanges(dir)?))
    }

    /// Every request received, with when it arrived.
    pub fn requests(&self) -> Vec<(Instant, Request)> {
        self.lock().seen.clone()
    }

    /// `Ok` once every exchange has been played with no mismatch.
    pub fn finish(&self) -> Result<(), String> {
        let log = self.lock();
        if let Some(e) = log.errors.first() {
            return Err(e.clone());
        }
        let unplayed = log.played.iter().filter(|p| !**p).count();
        if let Some(i) = log.played.iter().position(|p| !*p) {
            return Err(format!(
                "{unplayed} of {} exchanges never requested; first: {} {}",
                self.exchanges.len(),
                self.exchanges[i].request.method.as_str(),
                self.exchanges[i].request.url,
            ));
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn answer(&self, req: &Request) -> Result<Response, HttpError> {
        let mut log = self.lock();
        log.seen.push((Instant::now(), req.clone()));
        let pick = if self.ordered {
            match self.exchanges.get(log.next) {
                None => Err(format!(
                    "unexpected {} {}: all exchanges played",
                    req.method.as_str(),
                    req.url
                )),
                Some(ex) => matches(&ex.request, req)
                    .map(|()| log.next)
                    .map_err(|why| format!("exchange {}: {why}", log.next + 1)),
            }
        } else {
            (0..self.exchanges.len())
                .find(|&i| !log.played[i] && matches(&self.exchanges[i].request, req).is_ok())
                .ok_or_else(|| {
                    format!(
                        "no unplayed exchange matches {} {}",
                        req.method.as_str(),
                        req.url
                    )
                })
        };
        let i = match pick {
            Ok(i) => i,
            Err(e) => {
                log.errors.push(e.clone());
                return Err(HttpError::Unrecorded(e));
            }
        };
        log.played[i] = true;
        while log.played.get(log.next) == Some(&true) {
            log.next += 1;
        }
        self.exchanges[i]
            .response
            .clone()
            .map_err(HttpError::Transport)
    }
}

impl Fetch for Replayer {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(async move { self.answer(req) })
    }
}

fn load_exchanges(dir: &Path) -> Result<Vec<Exchange>, String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "http"))
        .collect();
    names.sort();
    if names.is_empty() {
        return Err(format!("{}: no .http files", dir.display()));
    }
    let mut exchanges = Vec::with_capacity(names.len());
    for path in names {
        let head = read(&path)?;
        let head = String::from_utf8(head).map_err(|_| format!("{}: not UTF-8", path.display()))?;
        let body = read_opt(&path.with_extension("body"))?;
        let req_body = read_opt(&path.with_extension("req.body"))?;
        let ex = parse_exchange(&head, body, req_body)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        exchanges.push(ex);
    }
    Ok(exchanges)
}

fn matches(want: &Request, got: &Request) -> Result<(), String> {
    if want.method != got.method || want.url != got.url {
        return Err(format!(
            "expected {} {}, got {} {}",
            want.method.as_str(),
            want.url,
            got.method.as_str(),
            got.url
        ));
    }
    for (name, value) in &want.headers {
        let found = got
            .headers
            .iter()
            .any(|(n, v)| n.eq_ignore_ascii_case(name) && v == value);
        if !found {
            return Err(format!("missing header {name}: {value}"));
        }
    }
    if want.body != got.body {
        return Err("request body differs".into());
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_FIXTURE {
        return Err(format!("{}: over {MAX_FIXTURE} bytes", path.display()));
    }
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_opt(path: &Path) -> Result<Vec<u8>, String> {
    if path.exists() {
        read(path)
    } else {
        Ok(Vec::new())
    }
}

/// Parses a `.http` file's text (see the module docs).
pub fn parse_exchange(head: &str, body: Vec<u8>, req_body: Vec<u8>) -> Result<Exchange, String> {
    let mut lines = head.lines().map(|l| l.strip_suffix('\r').unwrap_or(l));
    let first = lines.next().ok_or("empty file")?;
    let (method, url) = first
        .split_once(' ')
        .ok_or("first line is not `METHOD URL`")?;
    let method = Method::parse(method).ok_or_else(|| format!("unknown method {method}"))?;
    let mut request = Request {
        method,
        url: url.trim().to_owned(),
        headers: Vec::new(),
        body: req_body,
    };
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        request.headers.push(header(line)?);
    }
    let status = lines.next().ok_or("no response block")?;
    let response = if let Some(msg) = status.strip_prefix("ERROR ") {
        Err(msg.to_owned())
    } else {
        let code = status
            .strip_prefix("HTTP ")
            .and_then(|c| c.trim().parse::<u16>().ok())
            .filter(|c| (100..=599).contains(c))
            .ok_or_else(|| format!("bad response line {status:?}"))?;
        let mut headers = Vec::new();
        for line in lines.by_ref() {
            if line.is_empty() {
                break;
            }
            headers.push(header(line)?);
        }
        Ok(Response {
            status: code,
            headers,
            body,
        })
    };
    if lines.any(|l| !l.trim().is_empty()) {
        return Err("text after the response head; bodies go in a .body file".into());
    }
    Ok(Exchange { request, response })
}

/// One recorded WebSocket session (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsTranscript {
    /// The URL the connect must name, exactly.
    pub url: String,
    /// The session's steps, or why it didn't open.
    pub open: Result<Vec<WsStep>, WsError>,
}

/// One step of a [`WsTranscript`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsStep {
    /// The plugin must send this.
    Send(WsMessage),
    /// The server sends this.
    Recv(WsMessage),
    /// The server closes the session.
    Close,
    /// The connection fails ([`WsError::Transport`]).
    Error(String),
    /// The idle timeout ends the session ([`WsError::Idle`]).
    Idle,
    /// The kill switch ends the session ([`WsError::EgressDown`]).
    Down(String),
}

impl WsStep {
    fn ends(&self) -> bool {
        matches!(
            self,
            WsStep::Close | WsStep::Error(_) | WsStep::Idle | WsStep::Down(_)
        )
    }
}

#[derive(Debug, Default)]
struct WsLog {
    /// The next transcript to connect.
    next: usize,
    /// Per transcript: which steps have been played.
    played: Vec<Vec<bool>>,
    connects: Vec<(Instant, String)>,
    sent: Vec<(Instant, usize, WsMessage)>,
    errors: Vec<String>,
}

/// Opens WebSocket sessions from recorded transcripts, one per connect,
/// in order. Nothing touches the network.
#[derive(Debug, Clone)]
pub struct WsReplayer {
    transcripts: Arc<Vec<WsTranscript>>,
    log: Arc<Mutex<WsLog>>,
}

impl WsReplayer {
    pub fn new(transcripts: Vec<WsTranscript>) -> WsReplayer {
        let played = transcripts
            .iter()
            .map(|t| vec![false; t.open.as_ref().map_or(0, Vec::len)])
            .collect();
        WsReplayer {
            transcripts: Arc::new(transcripts),
            log: Arc::new(Mutex::new(WsLog {
                played,
                ..WsLog::default()
            })),
        }
    }

    /// Loads a case directory's `.ws` files (see the module docs).
    pub fn load(dir: &Path) -> Result<WsReplayer, String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "ws"))
            .collect();
        names.sort();
        if names.is_empty() {
            return Err(format!("{}: no .ws files", dir.display()));
        }
        let mut transcripts = Vec::with_capacity(names.len());
        for path in names {
            let text = String::from_utf8(read(&path)?)
                .map_err(|_| format!("{}: not UTF-8", path.display()))?;
            transcripts
                .push(parse_transcript(&text).map_err(|e| format!("{}: {e}", path.display()))?);
        }
        Ok(WsReplayer::new(transcripts))
    }

    /// Every connect, with when it came and the URL it named.
    pub fn connects(&self) -> Vec<(Instant, String)> {
        self.lock().connects.clone()
    }

    /// Every message the plugin sent, with when and on which connection
    /// (counted from 0), matched or not.
    pub fn sent(&self) -> Vec<(Instant, usize, WsMessage)> {
        self.lock().sent.clone()
    }

    /// `Ok` once every transcript has been connected and every step played
    /// with no mismatch.
    pub fn finish(&self) -> Result<(), String> {
        let log = self.lock();
        if let Some(e) = log.errors.first() {
            return Err(e.clone());
        }
        if log.next < self.transcripts.len() {
            return Err(format!(
                "{} of {} transcripts never connected; first: {}",
                self.transcripts.len() - log.next,
                self.transcripts.len(),
                self.transcripts[log.next].url
            ));
        }
        for (i, played) in log.played.iter().enumerate() {
            if let Some(step) = played.iter().position(|p| !*p) {
                return Err(format!(
                    "transcript {}: step {} never played",
                    i + 1,
                    step + 1
                ));
            }
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WsLog> {
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn open(&self, url: &str, limits: WsLimits) -> Result<Box<dyn WsStream>, WsError> {
        let mut log = self.lock();
        log.connects.push((Instant::now(), url.to_owned()));
        if let Err(why) = ws::parse_url(url) {
            return Err(WsError::Connect(HttpError::Transport(format!(
                "refused by the endpoint policy: invalid URL: {why}"
            ))));
        }
        let i = log.next;
        let fail = |log: &mut WsLog, e: String| {
            log.errors.push(e.clone());
            Err(WsError::Unrecorded(e))
        };
        let Some(t) = self.transcripts.get(i) else {
            return fail(
                &mut log,
                format!("unexpected connect to {url}: all transcripts played"),
            );
        };
        if t.url != url {
            return fail(
                &mut log,
                format!("transcript {}: expected {}, got {url}", i + 1, t.url),
            );
        }
        log.next += 1;
        match &t.open {
            Err(e) => Err(e.clone()),
            Ok(_) => Ok(Box::new(WsReplay {
                replayer: self.clone(),
                session: i,
                limits: limits.clamped(),
                recv_at: 0,
                ended: None,
            })),
        }
    }
}

impl WsConnect for WsReplayer {
    fn connect<'a>(
        &'a self,
        url: &'a str,
        limits: WsLimits,
    ) -> BoxFuture<'a, Result<Box<dyn WsStream>, WsError>> {
        Box::pin(async move { self.open(url, limits) })
    }
}

/// A session played from a transcript.
struct WsReplay {
    replayer: WsReplayer,
    session: usize,
    limits: WsLimits,
    /// The next step a receive looks at.
    recv_at: usize,
    /// Set once the session ended: `None` for a close, the error
    /// otherwise.
    ended: Option<Option<WsError>>,
}

impl WsReplay {
    fn send_msg(&mut self, msg: WsMessage) -> Result<(), WsError> {
        if self.ended.is_some() {
            return Err(WsError::Closed);
        }
        if msg.len() > self.limits.max_message {
            return Err(WsError::TooLarge);
        }
        let replayer = self.replayer.clone();
        let steps = replayer.transcripts[self.session]
            .open
            .as_deref()
            .unwrap_or_default();
        let mut log = replayer.lock();
        log.sent.push((Instant::now(), self.session, msg.clone()));
        let played = &log.played[self.session];
        let next = steps
            .iter()
            .enumerate()
            .find(|(i, s)| matches!(s, WsStep::Send(_)) && !played[*i]);
        let why = match next {
            Some((i, WsStep::Send(want))) if *want == msg => {
                log.played[self.session][i] = true;
                return Ok(());
            }
            Some((i, WsStep::Send(want))) => {
                format!("step {}: expected to send {want:?}, sent {msg:?}", i + 1)
            }
            _ => format!("unexpected send {msg:?}: no send left"),
        };
        let e = format!("transcript {}: {why}", self.session + 1);
        log.errors.push(e.clone());
        drop(log);
        self.ended = Some(Some(WsError::Unrecorded(e.clone())));
        Err(WsError::Unrecorded(e))
    }

    /// The next step a receive plays, `None` to wait for ever.
    fn next_recv(&mut self) -> Option<Result<Option<WsMessage>, WsError>> {
        match &self.ended {
            Some(None) => return Some(Ok(None)),
            Some(Some(_)) => return Some(Err(WsError::Closed)),
            None => {}
        }
        let replayer = self.replayer.clone();
        let steps = replayer.transcripts[self.session]
            .open
            .as_deref()
            .unwrap_or_default();
        let mut log = replayer.lock();
        while let Some(step) = steps.get(self.recv_at) {
            let i = self.recv_at;
            if let WsStep::Send(_) = step {
                if !log.played[self.session][i] {
                    // The server waits for the plugin's message.
                    return None;
                }
                self.recv_at += 1;
                continue;
            }
            log.played[self.session][i] = true;
            self.recv_at += 1;
            let result = match step {
                WsStep::Recv(m) if m.len() > self.limits.max_message => Err(WsError::TooLarge),
                WsStep::Recv(m) => return Some(Ok(Some(m.clone()))),
                WsStep::Close => Ok(None),
                WsStep::Error(e) => Err(WsError::Transport(e.clone())),
                WsStep::Idle => Err(WsError::Idle),
                WsStep::Down(r) => Err(WsError::EgressDown(r.clone())),
                WsStep::Send(_) => unreachable!("handled above"),
            };
            drop(log);
            self.ended = Some(result.as_ref().err().cloned());
            return Some(result);
        }
        None
    }

    async fn recv_msg(&mut self) -> Result<Option<WsMessage>, WsError> {
        match self.next_recv() {
            Some(r) => r,
            None => std::future::pending().await,
        }
    }
}

impl WsStream for WsReplay {
    fn send(&mut self, msg: WsMessage) -> BoxFuture<'_, Result<(), WsError>> {
        let r = self.send_msg(msg);
        Box::pin(async move { r })
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Option<WsMessage>, WsError>> {
        Box::pin(self.recv_msg())
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        if self.ended.is_none() {
            self.ended = Some(None);
        }
        Box::pin(async {})
    }
}

/// Parses a `.ws` transcript's text (see the module docs).
pub fn parse_transcript(text: &str) -> Result<WsTranscript, String> {
    let mut lines = text
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.starts_with('#'));
    let (_, first) = lines.next().ok_or("empty transcript")?;
    let url = first
        .strip_prefix("CONNECT ")
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("the first line is not `CONNECT <url>`")?
        .to_owned();
    let mut steps = Vec::new();
    let mut open = None;
    for (n, line) in lines {
        let at = |why: String| format!("line {}: {why}", n + 1);
        if open.is_some() || steps.last().is_some_and(WsStep::ends) {
            return Err(at("nothing may follow the end of a session".into()));
        }
        if let Some(msg) = line.strip_prefix("REFUSED ") {
            if !steps.is_empty() {
                return Err(at("REFUSED must follow CONNECT".into()));
            }
            open = Some(WsError::Connect(HttpError::Transport(msg.to_owned())));
            continue;
        }
        if let Some(status) = line.strip_prefix("HANDSHAKE ") {
            if !steps.is_empty() {
                return Err(at("HANDSHAKE must follow CONNECT".into()));
            }
            let status = status
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|s| (100..=599).contains(s) && *s != 101)
                .ok_or_else(|| at(format!("bad status {status:?}")))?;
            open = Some(WsError::Handshake(Some(status)));
            continue;
        }
        let step = match line {
            "CLOSE" => WsStep::Close,
            "IDLE" => WsStep::Idle,
            ">" => WsStep::Send(WsMessage::Text(String::new())),
            "<" => WsStep::Recv(WsMessage::Text(String::new())),
            ">b" => WsStep::Send(WsMessage::Binary(Vec::new())),
            "<b" => WsStep::Recv(WsMessage::Binary(Vec::new())),
            _ => {
                if let Some(e) = line.strip_prefix("ERROR ") {
                    WsStep::Error(e.to_owned())
                } else if let Some(r) = line.strip_prefix("DOWN ") {
                    WsStep::Down(r.to_owned())
                } else if let Some(h) = line.strip_prefix(">b ") {
                    WsStep::Send(WsMessage::Binary(hex(h).map_err(at)?))
                } else if let Some(h) = line.strip_prefix("<b ") {
                    WsStep::Recv(WsMessage::Binary(hex(h).map_err(at)?))
                } else if let Some(t) = line.strip_prefix("> ") {
                    WsStep::Send(WsMessage::Text(t.to_owned()))
                } else if let Some(t) = line.strip_prefix("< ") {
                    WsStep::Recv(WsMessage::Text(t.to_owned()))
                } else {
                    return Err(at(format!("unknown step {line:?}")));
                }
            }
        };
        steps.push(step);
    }
    Ok(WsTranscript {
        url,
        open: open.map_or(Ok(steps), Err),
    })
}

fn hex(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return Err("odd number of hex digits".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
                .ok_or_else(|| "bad hex".to_owned())
        })
        .collect()
}

fn header(line: &str) -> Result<(String, String), String> {
    let (n, v) = line
        .split_once(':')
        .ok_or_else(|| format!("bad header line {line:?}"))?;
    Ok((n.trim().to_owned(), v.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_exchange() {
        let ex = parse_exchange(
            "GET https://h/api?t=caps\r\nAccept: text/xml\r\n\r\nHTTP 200\r\nContent-Type: text/xml\r\n",
            b"<caps/>".to_vec(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(ex.request.url, "https://h/api?t=caps");
        assert_eq!(
            ex.request.headers,
            vec![("Accept".into(), "text/xml".into())]
        );
        let res = ex.response.unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.header("content-type"), Some("text/xml"));
        assert_eq!(res.body, b"<caps/>");
    }

    #[test]
    fn parses_transport_errors_and_rejects_junk() {
        let ex = parse_exchange(
            "GET https://h/\n\nERROR connection refused\n",
            vec![],
            vec![],
        )
        .unwrap();
        assert_eq!(ex.response, Err("connection refused".into()));
        assert!(parse_exchange("FETCH https://h/\n\nHTTP 200\n", vec![], vec![]).is_err());
        assert!(parse_exchange("GET https://h/\n\nHTTP 999\n", vec![], vec![]).is_err());
        assert!(parse_exchange("GET https://h/\n", vec![], vec![]).is_err());
        assert!(parse_exchange("GET https://h/\n\nHTTP 200\n\n<body>\n", vec![], vec![]).is_err());
    }

    #[tokio::test]
    async fn replays_in_order_and_reports_mismatches() {
        let ex = |url: &str| Exchange {
            request: Request::get(url),
            response: Ok(Response {
                status: 200,
                headers: vec![],
                body: url.as_bytes().to_vec(),
            }),
        };
        let r = Replayer::new(vec![ex("https://h/1"), ex("https://h/2")]);
        assert_eq!(
            r.fetch(&Request::get("https://h/1")).await.unwrap().body,
            b"https://h/1"
        );
        assert!(r.finish().is_err());
        assert!(matches!(
            r.fetch(&Request::get("https://h/3")).await,
            Err(HttpError::Unrecorded(_))
        ));
        assert!(r.fetch(&Request::get("https://h/2")).await.is_ok());
        // The mismatch is still reported.
        assert!(r.finish().unwrap_err().contains("expected GET https://h/2"));
        assert_eq!(r.requests().len(), 3);
    }
    const RELAY: &str = "\
# a relay that answers one subscription
CONNECT wss://relay.example/\r
> [\"REQ\",\"s\",{}]
< [\"EVENT\",\"s\",{}]

<b 00ff
< [\"EOSE\",\"s\"]
CLOSE
";

    #[test]
    fn parses_transcripts_and_rejects_junk() {
        let t = parse_transcript(RELAY).unwrap();
        assert_eq!(t.url, "wss://relay.example/");
        assert_eq!(
            t.open.unwrap(),
            vec![
                WsStep::Send(WsMessage::Text(r#"["REQ","s",{}]"#.into())),
                WsStep::Recv(WsMessage::Text(r#"["EVENT","s",{}]"#.into())),
                WsStep::Recv(WsMessage::Binary(vec![0, 0xff])),
                WsStep::Recv(WsMessage::Text(r#"["EOSE","s"]"#.into())),
                WsStep::Close,
            ]
        );
        let t = parse_transcript("CONNECT wss://r/\nHANDSHAKE 403\n").unwrap();
        assert_eq!(t.open, Err(WsError::Handshake(Some(403))));
        let t = parse_transcript("CONNECT wss://r/\nREFUSED no route\n").unwrap();
        assert_eq!(
            t.open,
            Err(WsError::Connect(HttpError::Transport("no route".into())))
        );
        let t = parse_transcript("CONNECT wss://r/\n>\n<b\nIDLE\n").unwrap();
        assert_eq!(t.open.unwrap().len(), 3);
        for bad in [
            "",
            "> x\n",
            "CONNECT wss://r/\nCLOSE\n< late\n",
            "CONNECT wss://r/\n> x\nHANDSHAKE 403\n",
            "CONNECT wss://r/\nHANDSHAKE 101\n",
            "CONNECT wss://r/\nHANDSHAKE 403\n> x\n",
            "CONNECT wss://r/\n<b 0g\n",
            "CONNECT wss://r/\n<b 012\n",
            "CONNECT wss://r/\nPING\n",
        ] {
            assert!(parse_transcript(bad).is_err(), "{bad:?}");
        }
    }

    fn text(t: &str) -> WsMessage {
        WsMessage::Text(t.into())
    }

    #[tokio::test]
    async fn replays_a_session_and_checks_every_step() {
        let r = WsReplayer::new(vec![parse_transcript(RELAY).unwrap()]);
        let mut s = r
            .connect("wss://relay.example/", WsLimits::default())
            .await
            .unwrap();
        // The server waits for the plugin's request.
        assert!(
            tokio::time::timeout(Duration::from_millis(10), s.recv())
                .await
                .is_err()
        );
        s.send(text(r#"["REQ","s",{}]"#)).await.unwrap();
        assert_eq!(s.recv().await, Ok(Some(text(r#"["EVENT","s",{}]"#))));
        assert!(r.finish().unwrap_err().contains("step 3 never played"));
        assert_eq!(s.recv().await, Ok(Some(WsMessage::Binary(vec![0, 0xff]))));
        assert_eq!(s.recv().await, Ok(Some(text(r#"["EOSE","s"]"#))));
        assert_eq!(s.recv().await, Ok(None));
        assert_eq!(s.recv().await, Ok(None));
        assert_eq!(s.send(text("late")).await, Err(WsError::Closed));
        r.finish().unwrap();
        assert_eq!(r.connects().len(), 1);
        assert_eq!(r.sent().len(), 1);
        // A further connect has nothing recorded.
        assert!(matches!(
            r.connect("wss://relay.example/", WsLimits::default()).await,
            Err(WsError::Unrecorded(_))
        ));
        assert!(r.finish().is_err());
    }

    #[tokio::test]
    async fn replays_mismatches_failures_and_caps() {
        let r = WsReplayer::new(vec![
            parse_transcript("CONNECT wss://a/\nHANDSHAKE 429\n").unwrap(),
            parse_transcript("CONNECT wss://a/\n> one\n< big\nDOWN the VPN dropped\n").unwrap(),
        ]);
        // Not wss: refused before anything is matched.
        assert!(matches!(
            r.connect("ws://a/", WsLimits::default()).await,
            Err(WsError::Connect(_))
        ));
        assert_eq!(
            r.connect("wss://a/", WsLimits::default()).await.err(),
            Some(WsError::Handshake(Some(429)))
        );
        let small = WsLimits {
            max_message: 2,
            ..WsLimits::default()
        };
        let mut s = r.connect("wss://a/", small).await.unwrap();
        assert_eq!(s.send(text("three")).await, Err(WsError::TooLarge));
        s.send(text("on")).await.unwrap_err();
        assert!(r.finish().unwrap_err().contains("expected to send"));
        let mut s = WsReplayer::new(vec![
            parse_transcript("CONNECT wss://a/\n< big\nDOWN the VPN dropped\n").unwrap(),
        ]);
        let mut session = s.connect("wss://a/", small).await.unwrap();
        assert_eq!(session.recv().await, Err(WsError::TooLarge));
        assert_eq!(session.recv().await, Err(WsError::Closed));
        assert!(s.finish().unwrap_err().contains("step 2 never played"));
        s = WsReplayer::new(vec![
            parse_transcript("CONNECT wss://a/\n< ok\nDOWN the VPN dropped\n").unwrap(),
        ]);
        let mut session = s.connect("wss://a/", small).await.unwrap();
        assert_eq!(session.recv().await, Ok(Some(text("ok"))));
        assert_eq!(
            session.recv().await,
            Err(WsError::EgressDown("the VPN dropped".into()))
        );
        s.finish().unwrap();
    }
}
