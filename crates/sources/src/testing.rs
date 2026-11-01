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
}
