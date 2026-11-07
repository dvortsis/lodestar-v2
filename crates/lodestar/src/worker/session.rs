//! The worker's connection to the coordinator, kept up for as long as it
//! runs (architecture §19.6, workers plan WK-2).
//!
//! - **Connect** over the cluster library's mutual-TLS WebSocket
//!   ([`Tcp`]), say `Hello`, and hand everything the session doesn't
//!   handle itself to the runtime ([`Work`]).
//! - **Reconnect** with capped exponential back-off and equal jitter
//!   ([`Backoff`]): 1 s, 2 s, 4 s … up to 2 minutes, each wait between half
//!   and all of that step. It resets once a session has lasted a minute. The wait itself
//!   is injected ([`Wait`]), so a test sees each one without waiting (the
//!   binary may not depend on tokio's `test-util`, which CI checks).
//! - **Renew** the certificate at half-life (or at once when `Welcome`
//!   says a CA is being rotated out), save the new credentials, then
//!   reconnect at once on them: the coordinator checks the certificate a
//!   session was opened with, so a session left on the old one would be
//!   stopped when that one expires.
//! - **Stop** on `Drain` (finish the runtime's leases, report them, say
//!   `Drained`), on `Stop(Revoked)` or an expired certificate (at once:
//!   the user must join again), on another major version, and on a local
//!   shutdown. Anything else ends in a reconnect.

use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::time::Duration;

use lodestar_cluster::proto::{
    Capabilities, Hello, MAX_OUTCOME_BATCH, OutcomeItem, RenewRefusal, StopReason,
};
use lodestar_cluster::{
    ClientError, Clock, Credentials, RENEW_MIN_INTERVAL_SECS, WorkerConfig, WorkerEvent,
    WorkerLink, WorkerSession,
};

use super::Layout;

/// Where the worker's status lines go: stderr in the binary, a buffer in
/// tests. Every line is fixed text plus names, addresses and numbers;
/// never a token or a key.
pub type Report = Arc<dyn Fn(&str) + Send + Sync>;

/// How the worker dials the coordinator.
pub trait Connect: Send + Sync {
    fn link(
        &self,
        creds: &Credentials,
    ) -> impl Future<Output = Result<WorkerLink, ClientError>> + Send;
}

/// The cluster port over TCP, mutual TLS and the WebSocket upgrade.
#[derive(Debug, Clone)]
pub struct Tcp {
    /// `host:port`.
    pub addr: String,
    pub clock: Arc<dyn Clock>,
    pub within: Duration,
}

impl Connect for Tcp {
    fn link(
        &self,
        creds: &Credentials,
    ) -> impl Future<Output = Result<WorkerLink, ClientError>> + Send {
        lodestar_cluster::connect(&self.addr, creds, self.clock.clone(), self.within)
    }
}

/// What runs on the worker. WK-2's is [`Idle`]; WK-10 (`worker/runtime.rs`)
/// puts the DHT and the fetch pool behind it.
pub trait Work: Send {
    /// Something the coordinator sent that the session doesn't handle
    /// itself: `Assign`, re-granted leases, hints, snapshots, …
    fn event(&mut self, event: WorkerEvent);
    /// A drain: take no new work, finish what's under way and return the
    /// outcomes to report. Leases left out go back to the coordinator with
    /// `Drained`, which re-grants them.
    fn drain(&mut self) -> impl Future<Output = Vec<OutcomeItem>> + Send;
}

/// The idle runtime: connected, doing no work.
#[derive(Debug, Default)]
pub struct Idle;

impl Work for Idle {
    fn event(&mut self, _event: WorkerEvent) {}

    async fn drain(&mut self) -> Vec<OutcomeItem> {
        Vec::new()
    }
}

/// Why the worker stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The coordinator drained it.
    Drained,
    /// Stopped here (Ctrl-C, SIGTERM).
    Shutdown,
    /// Its certificate was revoked: join again.
    Revoked,
    /// Its certificate expired: join again.
    Expired,
    /// The coordinator speaks another major version: update.
    Outdated { coordinator_major: u16 },
    /// The coordinator says `Hello` named another worker than the
    /// certificate.
    IdentityMismatch,
    /// Another worker with the same credentials connected.
    Replaced,
}

/// What a session's end means: stop, or connect again.
fn exit_for(reason: StopReason) -> Option<Exit> {
    match reason {
        StopReason::Revoked => Some(Exit::Revoked),
        StopReason::VersionMismatch { coordinator_major } => {
            Some(Exit::Outdated { coordinator_major })
        }
        StopReason::IdentityMismatch => Some(Exit::IdentityMismatch),
        StopReason::Drained => Some(Exit::Drained),
        StopReason::Replaced => Some(Exit::Replaced),
        StopReason::Protocol | StopReason::HeartbeatTimeout | StopReason::Shutdown => None,
    }
}

/// Capped exponential back-off with equal jitter.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    cap: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(base: Duration, cap: Duration) -> Backoff {
        Backoff {
            base: base.max(Duration::from_millis(1)),
            cap: cap.max(base),
            attempt: 0,
        }
    }

    /// The next wait: the step `min(cap, base · 2^n)`, of which the first
    /// half is fixed and the second scaled by `jitter` (any `u64`, so a
    /// random one spreads it evenly).
    pub fn next(&mut self, jitter: u64) -> Duration {
        let step = self
            .base
            .saturating_mul(1u32 << self.attempt.min(20))
            .min(self.cap);
        self.attempt = self.attempt.saturating_add(1);
        let half = step / 2;
        let spread = u64::try_from((step - half).as_millis()).unwrap_or(u64::MAX);
        half + Duration::from_millis(if spread == 0 {
            0
        } else {
            jitter % (spread + 1)
        })
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// How the worker waits between attempts: [`sleep`] in the binary.
pub type Wait = Arc<dyn Fn(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// A real wait.
pub fn sleep() -> Wait {
    Arc::new(|d| Box::pin(tokio::time::sleep(d)))
}

/// A random `u64` for the jitter (0 if the system's source fails: the
/// wait is then the step's lower half, still bounded).
pub fn random_jitter() -> u64 {
    getrandom::u64().unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
    /// Each dial: TCP, TLS and the upgrade.
    pub connect_timeout: Duration,
    /// How often a session checks whether to renew or has expired.
    pub upkeep_every: Duration,
    /// After a renewal that failed for another reason than "too soon".
    pub renew_retry_secs: i64,
    /// A session that lasts this long resets the back-off.
    pub stable_after: Duration,
    pub session: WorkerConfig,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(15),
            upkeep_every: Duration::from_secs(60),
            renew_retry_secs: 600,
            stable_after: Duration::from_secs(60),
            session: WorkerConfig::default(),
        }
    }
}

/// A joined worker, ready to run.
pub struct Worker<C> {
    pub layout: Layout,
    pub creds: Credentials,
    pub connect: C,
    pub clock: Arc<dyn Clock>,
    pub settings: Settings,
    pub report: Report,
    pub jitter: fn() -> u64,
    pub wait: Wait,
}

/// How one session ended.
enum Ended {
    Exit(Exit),
    /// The connection is gone: wait, then connect again.
    Lost,
    /// Renewed: connect again at once on the new certificate.
    Renewed,
}

/// The state of renewal across sessions.
#[derive(Debug, Default)]
struct Renewing {
    /// Don't try again before this (unix seconds).
    after: i64,
    /// The renewed credentials couldn't be saved yet.
    unsaved: bool,
}

impl<C: Connect> Worker<C> {
    fn say(&self, line: &str) {
        (self.report)(line);
    }

    fn expired(&self) -> bool {
        self.clock.now_secs() > self.creds.not_after()
    }

    fn hello(&self) -> Hello {
        let cores = std::thread::available_parallelism()
            .map_or(1, |n| u16::try_from(n.get()).unwrap_or(u16::MAX));
        Hello::new(
            self.creds.worker().0,
            Capabilities {
                cores,
                ..Capabilities::default()
            },
        )
    }

    /// Runs until the worker must stop, connecting again whenever the
    /// connection drops, or until `shutdown` completes.
    pub async fn run<W: Work>(mut self, mut work: W, shutdown: impl Future<Output = ()>) -> Exit {
        let mut shutdown = pin!(shutdown);
        let mut backoff = Backoff::new(self.settings.backoff_base, self.settings.backoff_cap);
        let mut renewing = Renewing::default();
        loop {
            if self.expired() {
                return Exit::Expired;
            }
            let attempt = async {
                let link = self.connect.link(&self.creds).await?;
                WorkerSession::start(
                    link,
                    self.hello(),
                    self.clock.clone(),
                    self.settings.session.clone(),
                )
                .await
            };
            let started = tokio::select! {
                () = shutdown.as_mut() => return Exit::Shutdown,
                r = attempt => r,
            };
            match started {
                Ok(session) => {
                    self.say(&format!("connected as worker {}", self.creds.name()));
                    let since = std::time::Instant::now();
                    let ended = self
                        .drive(session, &mut work, &mut renewing, shutdown.as_mut())
                        .await;
                    // Only a session that lasted resets the back-off: a
                    // coordinator that accepts and drops at once still gets
                    // longer and longer waits.
                    if since.elapsed() >= self.settings.stable_after {
                        backoff.reset();
                    }
                    match ended {
                        Ended::Exit(exit) => return exit,
                        Ended::Renewed => continue,
                        Ended::Lost => self.say("the connection to the coordinator was lost"),
                    }
                }
                Err(ClientError::Stopped(reason)) => {
                    if let Some(exit) = exit_for(reason) {
                        return self.or_expired(exit);
                    }
                    self.say("the coordinator turned the connection away for now");
                }
                Err(e) => self.say(&format!("couldn't connect: {e}")),
            }
            let wait = backoff.next((self.jitter)());
            self.say(&format!("connecting again in {:.1} s", wait.as_secs_f64()));
            tokio::select! {
                () = shutdown.as_mut() => return Exit::Shutdown,
                () = (self.wait)(wait) => {}
            }
        }
    }

    /// A certificate that has run out is reported as expired, whatever the
    /// coordinator called it (a session whose certificate expires is
    /// stopped as revoked).
    fn or_expired(&self, exit: Exit) -> Exit {
        if exit == Exit::Revoked && self.expired() {
            Exit::Expired
        } else {
            exit
        }
    }

    async fn drive<W: Work>(
        &mut self,
        mut session: WorkerSession,
        work: &mut W,
        renewing: &mut Renewing,
        mut shutdown: Pin<&mut impl Future<Output = ()>>,
    ) -> Ended {
        let mut renew_now = session.welcome().renew_now;
        let mut draining = false;
        let mut upkeep = tokio::time::interval(self.settings.upkeep_every);
        upkeep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.as_mut() => return Ended::Exit(Exit::Shutdown),
                event = session.next_event() => match event {
                    None | Some(WorkerEvent::Ended(None)) => return Ended::Lost,
                    Some(WorkerEvent::Ended(Some(reason))) => {
                        return match exit_for(reason) {
                            Some(exit) => Ended::Exit(self.or_expired(exit)),
                            None => Ended::Lost,
                        };
                    }
                    Some(WorkerEvent::Drain) => {
                        // Asked again after each reconnect until it's done.
                        if !draining {
                            draining = true;
                            self.say("draining: finishing the current work, then stopping");
                            if drain(&session, work).await.is_err() {
                                return Ended::Lost;
                            }
                        }
                    }
                    Some(WorkerEvent::ClockSkewed { offset_ms }) => self.say(&format!(
                        "this machine's clock is {} s off the coordinator's; \
                         set it to update automatically (NTP)",
                        offset_ms / 1000
                    )),
                    Some(other) => work.event(other),
                },
                _ = upkeep.tick() => {
                    if let Some(end) = self.upkeep(&session, renewing, &mut renew_now).await {
                        return end;
                    }
                }
            }
        }
    }

    /// The periodic check: expiry, a pending save, and renewal.
    async fn upkeep(
        &mut self,
        session: &WorkerSession,
        renewing: &mut Renewing,
        renew_now: &mut bool,
    ) -> Option<Ended> {
        let now = self.clock.now_secs();
        if self.expired() {
            return Some(Ended::Exit(Exit::Expired));
        }
        if renewing.unsaved {
            self.persist(renewing);
        }
        if !(*renew_now || self.creds.needs_renewal(now)) || now < renewing.after {
            return None;
        }
        let Ok(renewal) = self.creds.begin_renewal() else {
            renewing.after = now + self.settings.renew_retry_secs;
            return None;
        };
        match session.renew(renewal.public_key()).await {
            Ok(Ok(grant)) => match self.creds.renewed(renewal, grant, now) {
                Ok(fresh) => {
                    self.creds = fresh;
                    *renew_now = false;
                    renewing.unsaved = true;
                    self.persist(renewing);
                    self.say("certificate renewed; connecting again with it");
                    Some(Ended::Renewed)
                }
                Err(e) => {
                    self.say(&format!("the renewed certificate was refused: {e}"));
                    renewing.after = now + self.settings.renew_retry_secs;
                    None
                }
            },
            Ok(Err(RenewRefusal::Revoked)) => Some(Ended::Exit(Exit::Revoked)),
            Ok(Err(RenewRefusal::Expired)) => Some(Ended::Exit(Exit::Expired)),
            Ok(Err(RenewRefusal::TooSoon)) => {
                *renew_now = false;
                renewing.after = now + RENEW_MIN_INTERVAL_SECS;
                None
            }
            Ok(Err(RenewRefusal::Failed)) | Err(_) => {
                renewing.after = now + self.settings.renew_retry_secs;
                None
            }
        }
    }

    /// Saves the current credentials; on failure they stay in memory (the
    /// old ones on disk still work until they expire) and the save is
    /// tried again at the next check.
    fn persist(&self, renewing: &mut Renewing) {
        match self.layout.save(&self.creds) {
            Ok(()) => renewing.unsaved = false,
            Err(e) => self.say(&format!(
                "couldn't save the renewed credentials (trying again): {e}"
            )),
        }
    }
}

/// Finishes the runtime's work, reports it, and says `Drained`.
async fn drain<W: Work>(session: &WorkerSession, work: &mut W) -> Result<(), ClientError> {
    let outcomes = work.drain().await;
    for batch in outcomes.chunks(MAX_OUTCOME_BATCH) {
        session.report(batch.to_vec()).await?;
    }
    session.drained().await
}

#[cfg(test)]
pub(crate) mod tests {
    //! Against a real `ClusterServer` over TCP and mutual TLS, on one
    //! controllable clock; the back-off with its waits recorded.

    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Instant;

    use lodestar_cluster::proto::Outcome;
    use lodestar_cluster::{
        Claim, ClusterServer, Coordinator, JoinToken, LEAF_LIFETIME_SECS, ManualClock, Pki,
        ServerConfig, WorkerId,
    };
    use lodestar_integrations::SecretStore;
    use lodestar_metainfo::InfoHash;
    use lodestar_store::{SourceId, Store, StoreConfig};
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::worker::{self, JoinFailure, REFUSED};

    const DEADLINE: Duration = Duration::from_secs(20);

    /// Polls `what` until it holds; fails with what it last saw.
    pub(crate) async fn until<T: std::fmt::Debug>(
        name: &str,
        mut what: impl FnMut() -> (bool, T),
    ) -> T {
        let start = Instant::now();
        loop {
            let (done, seen) = what();
            if done {
                return seen;
            }
            assert!(start.elapsed() < DEADLINE, "{name}: still {seen:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A coordinator's PKI on `dir`'s store and key file.
    fn pki(dir: &Path, now: i64) -> Pki {
        let mut store = Store::open(dir.join("lodestar.db"), StoreConfig::default()).unwrap();
        let secrets = SecretStore::open_or_create(dir).unwrap();
        let coordinator = Coordinator::open(&mut store, &secrets, now).unwrap();
        Pki {
            coordinator,
            store,
            secrets,
        }
    }

    struct Coord {
        server: Arc<ClusterServer>,
        addr: String,
        serving: JoinHandle<()>,
    }

    /// A cluster server on any free port; ticks only by hand.
    async fn coordinator(dir: &Path, clock: &Arc<ManualClock>) -> Coord {
        let server = ClusterServer::new(
            pki(dir, clock.now_secs()),
            clock.clone(),
            ServerConfig {
                auto_tick: false,
                ..ServerConfig::default()
            },
            None,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let serving = tokio::spawn(server.clone().serve(listener));
        Coord {
            server,
            addr,
            serving,
        }
    }

    impl Coord {
        async fn token(&self, name: &str) -> String {
            let token = self.server.create_join_token(name).await.unwrap();
            token.expose().to_string()
        }

        fn connected(&self, worker: WorkerId) -> bool {
            self.server
                .snapshot()
                .workers
                .iter()
                .any(|w| w.worker == worker && w.connected)
        }

        async fn stop(self) {
            self.server.shutdown();
            self.serving.await.unwrap();
        }
    }

    /// Dials whatever address the test sets (a coordinator restarted on
    /// another port).
    #[derive(Clone)]
    struct Dial {
        addr: Arc<Mutex<String>>,
        clock: Arc<dyn Clock>,
    }

    impl Connect for Dial {
        fn link(
            &self,
            creds: &Credentials,
        ) -> impl Future<Output = Result<WorkerLink, ClientError>> + Send {
            let addr = self.addr.lock().unwrap().clone();
            let clock = self.clock.clone();
            async move { lodestar_cluster::connect(&addr, creds, clock, DEADLINE).await }
        }
    }

    /// Every line the worker printed.
    #[derive(Clone, Default)]
    pub(crate) struct Lines(Arc<Mutex<Vec<String>>>);

    impl Lines {
        pub(crate) fn report(&self) -> Report {
            let lines = self.0.clone();
            Arc::new(move |l: &str| lines.lock().unwrap().push(l.to_owned()))
        }

        pub(crate) fn all(&self) -> String {
            self.0.lock().unwrap().join("\n")
        }
    }

    fn settings() -> Settings {
        Settings {
            backoff_base: Duration::from_millis(10),
            backoff_cap: Duration::from_millis(200),
            connect_timeout: DEADLINE,
            upkeep_every: Duration::from_millis(20),
            ..Settings::default()
        }
    }

    struct Running {
        exit: JoinHandle<Exit>,
        stop: oneshot::Sender<()>,
    }

    impl Running {
        async fn exit(self) -> Exit {
            let Running { exit, stop } = self;
            let exit = tokio::time::timeout(DEADLINE, exit).await;
            drop(stop);
            exit.expect("the worker stopped in time").unwrap()
        }

        async fn shutdown(self) -> Exit {
            let _ = self.stop.send(());
            tokio::time::timeout(DEADLINE, self.exit)
                .await
                .expect("the worker stopped in time")
                .unwrap()
        }
    }

    /// Runs the worker saved in `layout` (a restart reads the folder
    /// again).
    fn run<W: Work + 'static>(
        layout: &Layout,
        dial: &Dial,
        clock: &Arc<ManualClock>,
        lines: &Lines,
        work: W,
    ) -> Running {
        let worker = Worker {
            layout: layout.clone(),
            creds: layout.load().unwrap().expect("joined"),
            connect: dial.clone(),
            clock: clock.clone(),
            settings: settings(),
            report: lines.report(),
            jitter: || 0,
            wait: sleep(),
        };
        let (stop, rx) = oneshot::channel::<()>();
        let exit = tokio::spawn(worker.run(work, async move {
            let _ = rx.await;
        }));
        Running { exit, stop }
    }

    struct Setup {
        _coord_dir: tempfile::TempDir,
        _worker_dir: tempfile::TempDir,
        coord_path: std::path::PathBuf,
        clock: Arc<ManualClock>,
        coord: Coord,
        layout: Layout,
        dial: Dial,
        lines: Lines,
        worker: WorkerId,
        token: String,
    }

    /// A coordinator and a worker joined to it.
    async fn joined() -> Setup {
        let coord_dir = tempfile::tempdir().unwrap();
        let worker_dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::starting_now());
        let coord = coordinator(coord_dir.path(), &clock).await;
        let layout = Layout::new(worker_dir.path());
        let token = coord.token("w1").await;
        let creds = worker::join(
            &layout,
            &coord.addr,
            JoinToken::parse(&token).unwrap(),
            clock.clone(),
        )
        .await
        .unwrap();
        assert_eq!(layout.load_address().unwrap(), Some(coord.addr.clone()));
        let dial = Dial {
            addr: Arc::new(Mutex::new(coord.addr.clone())),
            clock: clock.clone(),
        };
        Setup {
            coord_path: coord_dir.path().to_path_buf(),
            _coord_dir: coord_dir,
            _worker_dir: worker_dir,
            clock,
            coord,
            layout,
            dial,
            lines: Lines::default(),
            worker: creds.worker(),
            token,
        }
    }

    impl Setup {
        fn run<W: Work + 'static>(&self, work: W) -> Running {
            run(&self.layout, &self.dial, &self.clock, &self.lines, work)
        }

        async fn until_connected(&self, coord: &Coord) {
            until("connected", || {
                (coord.connected(self.worker), coord.server.snapshot())
            })
            .await;
        }

        /// Nothing the worker printed carries the token, nor its secret
        /// part on its own.
        fn assert_no_token(&self) {
            let all = self.lines.all();
            let secret = &self.token[4..60];
            assert!(
                !all.contains(&self.token) && !all.to_lowercase().contains(secret),
                "the token reached the output:\n{all}"
            );
        }
    }

    #[tokio::test]
    async fn join_then_restart_then_reconnect_to_a_restarted_coordinator() {
        let mut s = joined().await;
        let first = s.run(Idle);
        s.until_connected(&s.coord).await;
        assert_eq!(first.shutdown().await, Exit::Shutdown);
        until("disconnected", || (!s.coord.connected(s.worker), ())).await;

        // A restart runs from the saved credentials.
        let second = s.run(Idle);
        s.until_connected(&s.coord).await;
        assert_eq!(s.coord.server.snapshot().sessions, 2);

        // The coordinator restarts (on another port here): the worker
        // connects again by itself, after a back-off wait.
        let fresh = coordinator(&s.coord_path, &s.clock).await;
        *s.dial.addr.lock().unwrap() = fresh.addr.clone();
        let old = std::mem::replace(&mut s.coord, fresh);
        old.stop().await;
        s.until_connected(&s.coord).await;
        assert_eq!(second.shutdown().await, Exit::Shutdown);
        let all = s.lines.all();
        assert!(all.contains("connecting again in"), "{all}");
        s.assert_no_token();
    }

    #[tokio::test]
    async fn a_reused_or_expired_token_fails_with_one_message() {
        let s = joined().await;
        let before = s.layout.load().unwrap().unwrap().worker();
        // Reused.
        let again = worker::join(
            &s.layout,
            &s.coord.addr,
            JoinToken::parse(&s.token).unwrap(),
            s.clock.clone(),
        )
        .await;
        assert!(matches!(again, Err(JoinFailure::Refused)), "{again:?}");
        assert_eq!(again.unwrap_err().to_string(), REFUSED);
        // The saved credentials are untouched.
        assert_eq!(s.layout.load().unwrap().unwrap().worker(), before);

        // Expired: a token lasts 15 minutes.
        let late = s.coord.token("w2").await;
        s.clock.advance(Duration::from_secs(16 * 60));
        let dir = tempfile::tempdir().unwrap();
        let other = Layout::new(dir.path());
        let expired = worker::join(
            &other,
            &s.coord.addr,
            JoinToken::parse(&late).unwrap(),
            s.clock.clone(),
        )
        .await;
        let message = expired.unwrap_err().to_string();
        assert_eq!(message, REFUSED);
        assert!(!message.contains(&late) && !message.contains(&s.token));
        assert!(other.load().unwrap().is_none(), "nothing saved");
        assert!(other.load_address().unwrap().is_none());
    }

    #[tokio::test]
    async fn renews_at_half_life_and_reconnects_on_the_new_certificate() {
        let s = joined().await;
        let not_after = s.layout.load().unwrap().unwrap().not_after();
        let running = s.run(Idle);
        s.until_connected(&s.coord).await;
        let sessions = s.coord.server.snapshot().sessions;

        // Half-life: renewed, saved, and connected again on it.
        s.clock.advance(Duration::from_secs(
            u64::try_from(LEAF_LIFETIME_SECS / 2).unwrap(),
        ));
        let renewed = until("renewed on disk", || {
            let now = s.layout.load().unwrap().unwrap().not_after();
            (now > not_after, now)
        })
        .await;
        assert_eq!(renewed, s.clock.now_secs() + LEAF_LIFETIME_SECS);
        until("a new session", || {
            let snap = s.coord.server.snapshot();
            (
                snap.sessions > sessions && s.coord.connected(s.worker),
                snap.sessions,
            )
        })
        .await;

        // Past the old certificate's end: the coordinator's sweep (which
        // stops sessions on certificates that no longer stand) leaves the
        // worker alone, and it can still connect afresh.
        s.clock.advance(Duration::from_secs(
            u64::try_from(LEAF_LIFETIME_SECS / 2 + 60).unwrap(),
        ));
        s.coord.server.tick().await;
        let after = s.coord.server.snapshot().sessions;
        until("connected after the old certificate expired", || {
            let snap = s.coord.server.snapshot();
            (
                s.coord.connected(s.worker) && snap.revoked == 0,
                (snap.sessions, after),
            )
        })
        .await;
        assert!(!running.exit.is_finished(), "{}", s.lines.all());
        assert_eq!(running.shutdown().await, Exit::Shutdown);
        assert!(s.lines.all().contains("certificate renewed"));
        s.assert_no_token();
    }

    #[tokio::test]
    async fn a_revoked_worker_stops_within_one_heartbeat() {
        let s = joined().await;
        let running = s.run(Idle);
        s.until_connected(&s.coord).await;
        // Revoked by another process on the same store (the CLI, or the
        // API's own connection): the server learns of it on its next tick.
        {
            let mut other = pki(&s.coord_path, s.clock.now_secs());
            other
                .coordinator
                .revoke(&mut other.store, s.worker)
                .unwrap();
        }
        s.coord.server.tick().await;
        assert_eq!(running.exit().await, Exit::Revoked);
        // It doesn't come back.
        assert!(!s.coord.connected(s.worker));
    }

    /// Keys and their fencing tokens.
    type Leases = Arc<Mutex<Vec<([u8; 20], u64)>>>;

    /// Holds what it's re-granted; on a drain, reports each as found.
    #[derive(Clone, Default)]
    struct Holding {
        held: Leases,
        drained: Arc<Mutex<bool>>,
    }

    impl Work for Holding {
        fn event(&mut self, event: WorkerEvent) {
            if let WorkerEvent::Regranted(items) = event {
                for item in items {
                    if let lodestar_cluster::proto::GrantItem::Granted { key, token, .. } = item {
                        self.held.lock().unwrap().push((key, token));
                    }
                }
            }
        }

        async fn drain(&mut self) -> Vec<OutcomeItem> {
            *self.drained.lock().unwrap() = true;
            self.held
                .lock()
                .unwrap()
                .iter()
                .map(|&(key, token)| OutcomeItem {
                    key,
                    token,
                    outcome: Outcome::PeerFound { reason: 1 },
                })
                .collect()
        }
    }

    #[tokio::test]
    async fn a_drain_finishes_and_reports_the_leases_then_stops() {
        let s = joined().await;
        let work = Holding::default();
        let running = s.run(work.clone());
        s.until_connected(&s.coord).await;

        // A lease that comes back from another worker is re-granted here.
        let key = InfoHash([7; 20]);
        let authority = s.coord.server.authority();
        let elsewhere = WorkerId(999);
        authority.claim(
            elsewhere,
            Claim {
                key,
                source: SourceId::new(0).unwrap(),
                strategy: 0,
                at: s.clock.now_secs(),
            },
            s.clock.now_secs(),
        );
        assert_eq!(authority.evict(elsewhere), 1);
        s.coord.server.tick().await;
        until("re-granted here", || {
            let held = work.held.lock().unwrap().clone();
            (!held.is_empty(), held)
        })
        .await;

        assert!(s.coord.server.drain(s.worker));
        assert_eq!(running.exit().await, Exit::Drained);
        assert!(*work.drained.lock().unwrap());
        // Reported, so the lease ended with its outcome rather than going
        // back for a re-grant.
        let snap = s.coord.server.snapshot();
        assert_eq!((snap.outcomes, snap.pending), (1, 0), "{snap:?}");
        assert!(s.coord.server.lease(&key).is_none());
        assert!(s.lines.all().contains("draining"));
    }

    #[tokio::test]
    async fn an_expired_certificate_stops_before_dialling() {
        let s = joined().await;
        s.clock.advance(Duration::from_secs(
            u64::try_from(LEAF_LIFETIME_SECS + 1).unwrap(),
        ));
        let running = s.run(Idle);
        assert_eq!(running.exit().await, Exit::Expired);
        assert_eq!(s.coord.server.snapshot().sessions, 0);
    }

    #[test]
    fn which_stops_end_the_worker() {
        let table = [
            (StopReason::Revoked, Some(Exit::Revoked)),
            (
                StopReason::VersionMismatch {
                    coordinator_major: 2,
                },
                Some(Exit::Outdated {
                    coordinator_major: 2,
                }),
            ),
            (StopReason::IdentityMismatch, Some(Exit::IdentityMismatch)),
            (StopReason::Drained, Some(Exit::Drained)),
            (StopReason::Replaced, Some(Exit::Replaced)),
            // Passing: connect again.
            (StopReason::Protocol, None),
            (StopReason::HeartbeatTimeout, None),
            (StopReason::Shutdown, None),
        ];
        for (reason, want) in table {
            assert_eq!(exit_for(reason), want, "{reason:?}");
        }
    }

    #[tokio::test]
    async fn a_revocation_after_the_certificate_ran_out_reads_as_expired() {
        let s = joined().await;
        let worker = Worker {
            layout: s.layout.clone(),
            creds: s.layout.load().unwrap().unwrap(),
            connect: s.dial.clone(),
            clock: s.clock.clone(),
            settings: settings(),
            report: s.lines.report(),
            jitter: || 0,
            wait: sleep(),
        };
        assert_eq!(worker.or_expired(Exit::Revoked), Exit::Revoked);
        assert_eq!(worker.or_expired(Exit::Drained), Exit::Drained);
        s.clock.advance(Duration::from_secs(
            u64::try_from(LEAF_LIFETIME_SECS + 1).unwrap(),
        ));
        assert_eq!(worker.or_expired(Exit::Revoked), Exit::Expired);
        assert_eq!(worker.or_expired(Exit::Replaced), Exit::Replaced);
    }

    #[tokio::test]
    async fn a_renewal_that_cant_be_saved_is_kept_and_saved_later() {
        let s = joined().await;
        let not_after = s.layout.load().unwrap().unwrap().not_after();
        let running = s.run(Idle);
        s.until_connected(&s.coord).await;
        let sessions = s.coord.server.snapshot().sessions;
        // The save can't replace its temporary file.
        let blocker = s.layout.cluster().join("credentials.tmp");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("x"), b"").unwrap();
        s.clock.advance(Duration::from_secs(
            u64::try_from(LEAF_LIFETIME_SECS / 2).unwrap(),
        ));
        until("connected on the unsaved renewal", || {
            let snap = s.coord.server.snapshot();
            (
                snap.sessions > sessions
                    && s.coord.connected(s.worker)
                    && s.lines
                        .all()
                        .contains("couldn't save the renewed credentials"),
                snap.sessions,
            )
        })
        .await;
        assert_eq!(s.layout.load().unwrap().unwrap().not_after(), not_after);
        // Fixed: the next check saves it.
        std::fs::remove_dir_all(&blocker).unwrap();
        until("saved", || {
            let now = s.layout.load().unwrap().unwrap().not_after();
            (now > not_after, now)
        })
        .await;
        assert_eq!(running.shutdown().await, Exit::Shutdown);
    }

    #[test]
    fn backoff_doubles_to_the_cap_with_bounded_jitter() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(120));
        let low: Vec<u64> = (0..10).map(|_| b.next(0).as_millis() as u64).collect();
        assert_eq!(
            low,
            [
                500, 1000, 2000, 4000, 8000, 16000, 32000, 60000, 60000, 60000
            ]
        );
        b.reset();
        assert_eq!(
            b.next(500),
            Duration::from_millis(1000),
            "the top of the step"
        );
        for jitter in [0, 1, 12_345, u64::MAX] {
            for _ in 0..40 {
                let d = b.next(jitter);
                assert!(d >= Duration::from_millis(500) && d <= Duration::from_secs(120));
            }
        }
        // Many attempts never overflow.
        for _ in 0..10_000 {
            assert!(b.next(7) <= Duration::from_secs(120));
        }
    }

    /// Always fails.
    struct Down;

    impl Connect for Down {
        fn link(
            &self,
            _creds: &Credentials,
        ) -> impl Future<Output = Result<WorkerLink, ClientError>> + Send {
            std::future::ready(Err(ClientError::Io(std::io::ErrorKind::ConnectionRefused)))
        }
    }

    /// A "coordinator" that welcomes each session, then drops it at once.
    struct Flapping;

    impl Connect for Flapping {
        fn link(
            &self,
            _creds: &Credentials,
        ) -> impl Future<Output = Result<WorkerLink, ClientError>> + Send {
            let (mut coord, worker) = lodestar_cluster::local_pair(16);
            tokio::spawn(async move {
                if let Ok(Some(lodestar_cluster::proto::ToCoordinator::Hello(h))) =
                    coord.recv().await
                {
                    let welcome = lodestar_cluster::proto::Welcome {
                        major: h.major,
                        minor: h.minor,
                        worker: h.worker,
                        heartbeat_ms: 60_000,
                        lease_ttl_secs: 600,
                        renew_now: false,
                    };
                    let _ = coord
                        .send(lodestar_cluster::proto::ToWorker::Welcome(welcome))
                        .await;
                }
                coord.close().await;
            });
            std::future::ready(Ok(worker))
        }
    }

    /// The waits a worker on `connect` asks for, until it has asked `n`.
    async fn waits_of<C: Connect + 'static>(
        connect: C,
        stable_after: Duration,
        n: usize,
    ) -> Vec<u128> {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let clock = Arc::new(ManualClock::starting_now());
        layout
            .save(&crate::worker::tests::credentials(&clock))
            .unwrap();
        let waits = Arc::new(Mutex::new(Vec::new()));
        let seen = waits.clone();
        let lines = Lines::default();
        let worker = Worker {
            layout: layout.clone(),
            creds: layout.load().unwrap().unwrap(),
            connect,
            clock: clock.clone(),
            settings: Settings {
                backoff_base: Duration::from_secs(1),
                backoff_cap: Duration::from_secs(8),
                stable_after,
                ..Settings::default()
            },
            report: lines.report(),
            jitter: || 0,
            wait: Arc::new(move |d| {
                seen.lock().unwrap().push(d.as_millis());
                Box::pin(tokio::task::yield_now())
            }),
        };
        let (stop, rx) = oneshot::channel::<()>();
        let exit = tokio::spawn(worker.run(Idle, async move {
            let _ = rx.await;
        }));
        until("enough waits", || {
            let w = waits.lock().unwrap().clone();
            (w.len() >= n || exit.is_finished(), w)
        })
        .await;
        let _ = stop.send(());
        assert_eq!(exit.await.unwrap(), Exit::Shutdown, "{}", lines.all());
        let w = waits.lock().unwrap().clone();
        w[..n].to_vec()
    }

    #[tokio::test]
    async fn a_session_dropped_at_once_doesnt_reset_the_backoff() {
        let w = waits_of(Flapping, Duration::from_secs(60), 5).await;
        assert_eq!(w, [500, 1000, 2000, 4000, 4000]);
        // One that lasted resets it each time.
        let w = waits_of(Flapping, Duration::ZERO, 3).await;
        assert_eq!(w, [500, 500, 500]);
    }

    /// The waits between attempts follow the back-off: 0.5 s, 1 s, 2 s …
    /// with no jitter, capped. Each is recorded, not waited.
    #[tokio::test]
    async fn reconnect_waits_follow_the_backoff() {
        let w = waits_of(Down, Duration::from_secs(60), 6).await;
        assert_eq!(w, [500, 1000, 2000, 4000, 4000, 4000]);
    }
}
