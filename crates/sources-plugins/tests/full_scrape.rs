//! Contract tests for the BEP 48 full-scrape plugin (§18.9), with no
//! network: exchanges are built in the test and replayed, on tokio's
//! paused clock with a wall clock that follows it ([`TokioClock`]), so
//! days pass in no real time.
//!
//! Days are moved through with [`run_clock`], which advances the paused
//! clock a minute at a time and lets the plugin run at each step. No real
//! time passes, and every wait on the plugin is a condition with a
//! deadline (`wait_for`).

use std::sync::Arc;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_sources::state::PluginState;
use lodestar_sources::testing::{FakeCtx, Replayer, parse_exchange, wait_for};
use lodestar_sources::{
    Confidence, Consent, Discovery, Distribution, Kind, SourcePlugin, UserLimits, floors,
};
use lodestar_sources_plugins::full_scrape::{
    self, Config, DAY, FullScrape, StaticPrivateTrackers, TokioClock, WINDOW, to_found,
};
use lodestar_store::SourceId;
use proptest::prelude::*;
use tokio::time::Instant;

const SCRAPE: &str = "http://tracker.example/scrape";
const ANNOUNCE: &str = "http://tracker.example/announce";
const HOST: &str = "tracker.example";
/// A midnight, UTC.
const MIDNIGHT: i64 = 1_800_000_000 - 1_800_000_000 % 86_400;
const HOUR: i64 = 3600;

fn config() -> Config {
    Config::new(&[ANNOUNCE]).unwrap()
}

fn entry(h: u8) -> Vec<u8> {
    let mut v = b"20:".to_vec();
    v.extend_from_slice(&[h; 20]);
    v.extend_from_slice(b"d8:completei5e10:downloadedi50e10:incompletei1ee");
    v
}

/// A reply listing `hashes`, plus one malformed entry (a 19-byte key).
fn reply(hashes: &[u8]) -> Vec<u8> {
    let mut v = b"d5:filesd".to_vec();
    for h in hashes {
        v.extend_from_slice(&entry(*h));
    }
    v.extend_from_slice(b"19:");
    v.extend_from_slice(&[0xee; 19]);
    v.extend_from_slice(b"deee");
    v
}

fn ok(body: Vec<u8>) -> lodestar_sources::testing::Exchange {
    parse_exchange(
        &format!("GET {SCRAPE}\n\nHTTP 200\nContent-Type: text/plain\n"),
        body,
        Vec::new(),
    )
    .unwrap()
}

fn status(code: u16) -> lodestar_sources::testing::Exchange {
    parse_exchange(
        &format!("GET {SCRAPE}\n\nHTTP {code}\n"),
        Vec::new(),
        Vec::new(),
    )
    .unwrap()
}

struct Run {
    ctx: FakeCtx,
    running: lodestar_sources::testing::Running,
    replayer: Arc<Replayer>,
    clock: TokioClock,
}

/// Starts the plugin with the wall clock at `at` and `replayer` answering.
async fn start(at: i64, replayer: Replayer, private: StaticPrivateTrackers) -> Run {
    start_with(at, replayer, private, None).await
}

/// As [`start`], with the tracker's last attempt already in the state.
async fn start_with(
    at: i64,
    replayer: Replayer,
    private: StaticPrivateTrackers,
    last: Option<i64>,
) -> Run {
    let clock = TokioClock::starting_at(at);
    let p = Arc::new(FullScrape::new(
        config(),
        Arc::new(private),
        Arc::new(clock),
    ));
    let replayer = Arc::new(replayer);
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(9).unwrap(), replayer.clone());
    if let Some(last) = last {
        ctx.state
            .set(&format!("last:{HOST}"), last.to_string().into_bytes())
            .await
            .unwrap();
    }
    let running = ctx.start(p);
    Run {
        ctx,
        running,
        replayer,
        clock,
    }
}

impl Run {
    /// The wall-clock time of a request the replayer saw at `at`.
    fn wall(&self, at: Instant) -> i64 {
        use lodestar_sources_plugins::full_scrape::Clock;
        self.clock.now() - (Instant::now() - at).as_secs() as i64
    }

    fn requests(&self) -> Vec<i64> {
        self.replayer
            .requests()
            .into_iter()
            .map(|(at, req)| {
                assert_eq!(req.url, SCRAPE, "only the scrape URL is requested");
                self.wall(at)
            })
            .collect()
    }

    async fn finish(mut self) -> Vec<Discovery> {
        assert!(!self.running.is_finished(), "the plugin ended on its own");
        self.running.stop_within().await.unwrap();
        self.replayer.finish().unwrap();
        self.ctx.drain()
    }
}

/// Moves the paused clock on by `total`, a minute at a time, letting the
/// plugin run at each step.
async fn run_clock(total: Duration) {
    run_clock_by(total, Duration::from_secs(60)).await
}

/// As [`run_clock`], in steps of `step`: a step is the finest time at
/// which the test can see the plugin act.
async fn run_clock_by(total: Duration, step: Duration) {
    let mut left = total;
    while !left.is_zero() {
        let d = left.min(step);
        tokio::time::advance(d).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        left -= d;
    }
}

fn in_window(t: i64) -> bool {
    let minute = (t.rem_euclid(86_400) / 60) as u16;
    WINDOW.contains(minute)
}

#[tokio::test(start_paused = true)]
async fn keys_are_emitted_with_the_tracker_claim() {
    let mut run = start(
        MIDNIGHT + HOUR,
        Replayer::new(vec![ok(reply(&[1, 2, 3]))]),
        StaticPrivateTrackers::none(),
    )
    .await;
    wait_for("the scrape", || run.replayer.requests().len() == 1).await;
    let mut got = Vec::new();
    wait_for("the keys", || {
        got.extend(run.ctx.drain());
        got.len() >= 3
    })
    .await;
    let keys: Vec<InfoHash> = got.iter().map(|d| d.found.key).collect();
    assert_eq!(
        keys,
        [InfoHash([1; 20]), InfoHash([2; 20]), InfoHash([3; 20])]
    );
    for d in &got {
        let f = &d.found;
        assert!(!f.private_source && !f.interest && f.peers.is_empty());
        assert_eq!(f.strategy, full_scrape::STRATEGY_FULL_SCRAPE);
        let c = f.claims.as_deref().unwrap();
        assert_eq!(c.trackers, [ANNOUNCE]);
        assert!(c.name.is_none() && c.size.is_none() && c.files.is_empty());
        let p = f.provenance.as_deref().unwrap();
        assert_eq!(p.confidence, Confidence::Medium);
        assert!(p.external_ref.is_none());
    }
    // The attempt is recorded for the day.
    let last = run.ctx.state.get(&format!("last:{HOST}")).await.unwrap();
    assert_eq!(last, Some((MIDNIGHT + HOUR).to_string().into_bytes()));
    assert_eq!(run.requests(), [MIDNIGHT + HOUR]);
    let rest = run.finish().await;
    assert!(rest.is_empty(), "{rest:#?}");
}

#[tokio::test(start_paused = true)]
async fn at_most_one_scrape_per_tracker_per_day() {
    // Day 1 fails with a 500, which still counts; day 2 succeeds; day 3's
    // scrape is the third exchange. Nothing more in 3 days.
    let replayer = Replayer::new(vec![status(500), ok(reply(&[7])), ok(reply(&[8]))]);
    let run = start(MIDNIGHT + HOUR, replayer, StaticPrivateTrackers::none()).await;
    wait_for("day 1's scrape", || run.replayer.requests().len() == 1).await;
    // The rest of day 1, through the window and past it.
    run_clock(Duration::from_secs(23 * 3600)).await;
    assert_eq!(run.replayer.requests().len(), 1, "a second scrape on day 1");
    run_clock(Duration::from_secs(2 * 3600)).await;
    wait_for("day 2's scrape", || run.replayer.requests().len() == 2).await;
    run_clock(Duration::from_secs(25 * 3600)).await;
    wait_for("day 3's scrape", || run.replayer.requests().len() == 3).await;
    run_clock(Duration::from_secs(6 * 3600)).await;

    let times = run.requests();
    assert_eq!(times.len(), 3);
    for pair in times.windows(2) {
        assert!(pair[1] - pair[0] >= DAY.as_secs() as i64, "{times:?}");
    }
    assert!(times.iter().all(|t| in_window(*t)), "{times:?}");
    let got: Vec<InfoHash> = run.finish().await.iter().map(|d| d.found.key).collect();
    assert_eq!(got, [InfoHash([7; 20]), InfoHash([8; 20])]);
}

#[tokio::test(start_paused = true)]
async fn a_restart_keeps_the_days_cap() {
    // The state says the tracker was scraped at 00:30 today: a restart at
    // 01:00 waits until the window after 00:30 tomorrow, i.e. 01:00.
    let replayer = Replayer::new(vec![ok(reply(&[4]))]);
    let run = start_with(
        MIDNIGHT + HOUR,
        replayer,
        StaticPrivateTrackers::none(),
        Some(MIDNIGHT + HOUR / 2),
    )
    .await;
    run_clock(Duration::from_secs(23 * 3600)).await;
    assert!(
        run.replayer.requests().is_empty(),
        "scraped again within the day"
    );
    run_clock(Duration::from_secs(2 * 3600)).await;
    wait_for("the next day's scrape", || {
        run.replayer.requests().len() == 1
    })
    .await;
    let t = run.requests()[0];
    assert!(t >= MIDNIGHT + HOUR / 2 + DAY.as_secs() as i64 && in_window(t));
    run.finish().await;
}

#[tokio::test(start_paused = true)]
async fn nothing_is_sent_outside_the_window() {
    // Started at noon: the first scrape waits for 01:00.
    let replayer = Replayer::new(vec![ok(reply(&[5]))]);
    let run = start(
        MIDNIGHT + 12 * HOUR,
        replayer,
        StaticPrivateTrackers::none(),
    )
    .await;
    run_clock(Duration::from_secs(12 * 3600 + 59 * 60)).await;
    assert!(
        run.replayer.requests().is_empty(),
        "scraped outside the window"
    );
    run_clock(Duration::from_secs(2 * 60)).await;
    wait_for("the scrape at 01:00", || run.replayer.requests().len() == 1).await;
    let t = run.requests()[0];
    assert!(in_window(t) && t >= MIDNIGHT + 25 * HOUR, "{t}");
    run.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_private_tracker_is_never_scraped() {
    for private in [
        StaticPrivateTrackers::new(&[HOST]),
        // A lookup that fails counts as private.
        StaticPrivateTrackers::failing(),
    ] {
        let run = start(MIDNIGHT + HOUR, Replayer::new(Vec::new()), private).await;
        run_clock(Duration::from_secs(3 * 24 * 3600)).await;
        assert!(run.replayer.requests().is_empty());
        // No attempt is recorded either: nothing was sent.
        let last = run.ctx.state.get(&format!("last:{HOST}")).await.unwrap();
        assert!(last.is_none());
        run.finish().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_broken_reply_keeps_its_complete_entries() {
    let mut body = b"d5:filesd".to_vec();
    body.extend_from_slice(&entry(1));
    body.extend_from_slice(&entry(2));
    body.extend_from_slice(b"x garbage");
    let mut run = start(
        MIDNIGHT + HOUR,
        Replayer::new(vec![ok(body)]),
        StaticPrivateTrackers::none(),
    )
    .await;
    wait_for("the scrape", || run.replayer.requests().len() == 1).await;
    let mut got = Vec::new();
    wait_for("the keys", || {
        got.extend(run.ctx.drain());
        got.len() >= 2
    })
    .await;
    run_clock(Duration::from_secs(3600)).await;
    got.extend(run.finish().await);
    let keys: Vec<InfoHash> = got.iter().map(|d| d.found.key).collect();
    assert_eq!(keys, [InfoHash([1; 20]), InfoHash([2; 20])]);
}

#[tokio::test(start_paused = true)]
async fn an_unconfigured_plugin_fails_before_any_request() {
    let p = Arc::new(FullScrape::unconfigured(
        Arc::new(StaticPrivateTrackers::none()),
        Arc::new(TokioClock::starting_at(MIDNIGHT + HOUR)),
    ));
    assert!(p.endpoints().is_none());
    let mut ctx = FakeCtx::new(&*p, SourceId::new(9).unwrap());
    let err = p.run(ctx.take()).await.unwrap_err();
    assert!(err.0.starts_with("needs setup"), "{err}");
}

#[test]
fn manifest_policy_and_caps() {
    let p = FullScrape::new(
        config(),
        Arc::new(StaticPrivateTrackers::none()),
        Arc::new(TokioClock::starting_at(0)),
    );
    let m = p.manifest();
    assert_eq!(m.id, "full_scrape");
    assert_eq!(m.kinds, [Kind::Discovery]);
    assert_eq!(m.consent, Consent::None);
    assert!(m.endpoints.is_empty() && !m.enabled_by_default);
    assert!(p.endpoints().is_some());

    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::ShardedUnit);
    assert_eq!(policy.caps.max_full_scrapes_per_day, 1);
    assert!(!policy.schedule.is_open(0) && policy.schedule.is_open(60));
    assert!(policy.schedule.is_open(7 * 60 - 1) && !policy.schedule.is_open(7 * 60));
    // No setting raises the daily cap or shortens the day.
    let user = UserLimits {
        max_full_scrapes_per_day: Some(100),
        poll_interval: Some(Duration::from_secs(60)),
        ..UserLimits::default()
    };
    let limits = policy.limits(&user);
    assert_eq!(limits.max_full_scrapes_per_day, 1);
    assert_eq!(limits.poll_interval, DAY);
    assert!(limits.within(&policy.caps));
    assert!(limits.max_full_scrapes_per_day <= floors::FULL_SCRAPES_PER_DAY);
    let schema: serde_json::Value = serde_json::from_str(policy.config_schema).unwrap();
    assert_eq!(schema["properties"]["trackers"]["maxItems"], 32);
}

#[test]
fn config_refuses_private_local_and_unscrapable_trackers() {
    let c = config();
    let t = &c.trackers()[0];
    assert_eq!(
        (t.announce(), t.scrape_url(), t.host()),
        (ANNOUNCE, SCRAPE, HOST)
    );
    let https = Config::new(&["https://open.example:8443/announce.php"]).unwrap();
    assert_eq!(
        https.trackers()[0].scrape_url(),
        "https://open.example:8443/scrape.php"
    );

    for bad in [
        "udp://tracker.example:1337/announce",
        "http://tracker.example/announce?passkey=REDACTED",
        "http://tracker.example/announce?info_hash=x",
        "http://tracker.example/0123456789abcdef0123456789abcdef/announce",
        "http://tracker.example/announce/REDACTEDREDACTED00",
        "http://user:pw@tracker.example/announce",
        "http://tracker.example/announce#x",
        "http://tracker.example/tracker",
        "http://192.168.1.10/announce",
        "http://127.0.0.1:6969/announce",
        "http://[::1]/announce",
        "http://169.254.169.254/announce",
        "wss://tracker.example/announce",
        "not a url",
        "",
    ] {
        assert!(Config::new(&[bad]).is_err(), "{bad} was accepted");
    }
    let none: [&str; 0] = [];
    assert!(Config::new(&none).is_err());
    assert!(Config::new(&["http://a.example/announce", "https://a.example:81/announce"]).is_err());
    let many: Vec<String> = (0..33)
        .map(|i| format!("http://t{i}.example/announce"))
        .collect();
    assert!(Config::new(&many).is_err());
    assert!(Config::new(&many[..32]).is_ok());

    assert!(Config::from_json(r#"{"trackers":["http://tracker.example/announce"]}"#).is_ok());
    assert!(Config::from_json(r#"{"trackers":[],"extra":1}"#).is_err());
    // A refusal names the rule, never the address.
    let err = Config::new(&["http://tracker.example/announce?passkey=SECRET123"]).unwrap_err();
    assert!(!err.to_string().contains("SECRET"));
}

#[test]
fn to_found_is_a_plain_public_key() {
    let f = to_found(InfoHash([9; 20]), &config().trackers()[0]);
    assert_eq!(f.key, InfoHash([9; 20]));
    assert!(!f.private_source);
    assert_eq!(f.claims.unwrap().trackers, [ANNOUNCE]);
}

proptest! {
    /// The config parser never panics, and what it accepts is always a
    /// public http(s) tracker with a query-free scrape address.
    #[test]
    fn config_never_panics(raw in "\\PC{0,80}", url in "(http|https|udp)://[a-z0-9.:\\[\\]@/?#=-]{0,40}") {
        for s in [raw.as_str(), url.as_str()] {
            if let Ok(c) = Config::new(&[s]) {
                let t = &c.trackers()[0];
                prop_assert!(t.scrape_url().starts_with("http"));
                prop_assert!(!t.scrape_url().contains('?'));
            }
        }
    }
}

#[test]
fn the_window_closes_at_seven() {
    use full_scrape::minutes_until_close as left;
    assert_eq!(left(59), 0);
    assert_eq!(left(60), 6 * 60);
    assert_eq!(left(6 * 60 + 58), 2);
    assert_eq!(left(6 * 60 + 59), 1);
    assert_eq!(left(7 * 60), 0);
    assert_eq!(left(12 * 60), 0);
}

/// A tracker that never answers. Records when each request started and
/// how many were abandoned (their future dropped).
#[derive(Default)]
struct Hanging {
    started: std::sync::Mutex<Vec<Instant>>,
    dropped: Arc<std::sync::atomic::AtomicUsize>,
}

struct OnDrop(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for OnDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl lodestar_sources::Fetch for Hanging {
    fn fetch<'a>(
        &'a self,
        _: &'a lodestar_sources::Request,
    ) -> lodestar_sources::BoxFuture<
        'a,
        Result<lodestar_sources::Response, lodestar_sources::HttpError>,
    > {
        self.started.lock().unwrap().push(Instant::now());
        let guard = OnDrop(self.dropped.clone());
        Box::pin(async move {
            let _guard = guard;
            std::future::pending().await
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_request_still_running_at_seven_is_abandoned() {
    use std::sync::atomic::Ordering;
    let clock = TokioClock::starting_at(MIDNIGHT + 6 * HOUR + 58 * 60);
    let p = Arc::new(FullScrape::new(
        config(),
        Arc::new(StaticPrivateTrackers::none()),
        Arc::new(clock),
    ));
    let fetch = Arc::new(Hanging::default());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(9).unwrap(), fetch.clone());
    let running = ctx.start(p);
    wait_for("the scrape to start", || {
        !fetch.started.lock().unwrap().is_empty()
    })
    .await;
    let begun = fetch.started.lock().unwrap()[0];
    wait_for("the scrape to be abandoned", || {
        fetch.dropped.load(Ordering::SeqCst) == 1
    })
    .await;
    // Dropped by 07:00, i.e. within two minutes of 06:58.
    assert!(Instant::now() - begun <= Duration::from_secs(120));
    // Nothing else goes out until the next day's window.
    run_clock(Duration::from_secs(17 * 3600)).await;
    assert_eq!(fetch.started.lock().unwrap().len(), 1);
    running.stop_within().await.unwrap();
}

/// A plugin state that can't be read.
#[derive(Debug)]
struct BrokenState;

impl PluginState for BrokenState {
    fn get<'a>(
        &'a self,
        _: &'a str,
    ) -> lodestar_sources::BoxFuture<'a, Result<Option<Vec<u8>>, lodestar_sources::StateError>>
    {
        Box::pin(async { Err(lodestar_sources::StateError::Store("broken".into())) })
    }
    fn set<'a>(
        &'a self,
        _: &'a str,
        _: Vec<u8>,
    ) -> lodestar_sources::BoxFuture<'a, Result<(), lodestar_sources::StateError>> {
        Box::pin(async { Err(lodestar_sources::StateError::Store("broken".into())) })
    }
    fn remove<'a>(
        &'a self,
        _: &'a str,
    ) -> lodestar_sources::BoxFuture<'a, Result<(), lodestar_sources::StateError>> {
        Box::pin(async { Err(lodestar_sources::StateError::Store("broken".into())) })
    }
    fn compare_and_set<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a [u8]>,
        _: Option<Vec<u8>>,
    ) -> lodestar_sources::BoxFuture<'a, Result<bool, lodestar_sources::StateError>> {
        Box::pin(async { Err(lodestar_sources::StateError::Store("broken".into())) })
    }
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_state_means_no_scrape() {
    use lodestar_sources::{Emitter, HostLimiter, Kit, Shutdown, SourceCtx};
    let p = Arc::new(FullScrape::new(
        config(),
        Arc::new(StaticPrivateTrackers::none()),
        Arc::new(TokioClock::starting_at(MIDNIGHT + HOUR)),
    ));
    let replayer = Arc::new(Replayer::new(Vec::new()));
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let (trigger, shutdown) = Shutdown::new();
    let source = SourceId::new(9).unwrap();
    let ctx = SourceCtx {
        source,
        emitter: Emitter::new(source, tx),
        peer_requests: None,
        peer_replies: None,
        shutdown,
        kit: Kit::with_limiter(
            Arc::new(BrokenState),
            replayer.clone(),
            HostLimiter::new(p.policy().limits(&UserLimits::default())),
        ),
    };
    let task = tokio::spawn({
        let p = p.clone();
        async move { p.run(ctx).await }
    });
    run_clock(Duration::from_secs(2 * 24 * 3600)).await;
    assert!(replayer.requests().is_empty());
    let _ = trigger.send(true);
    task.await.unwrap().unwrap();
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_garbled_or_future_state_restarts_the_day() {
    for stored in [
        b"garbage".to_vec(),
        (MIDNIGHT + 400 * 24 * HOUR).to_string().into_bytes(),
    ] {
        let replayer = Replayer::new(vec![ok(reply(&[3]))]);
        let clock = TokioClock::starting_at(MIDNIGHT + HOUR);
        let p = Arc::new(FullScrape::new(
            config(),
            Arc::new(StaticPrivateTrackers::none()),
            Arc::new(clock),
        ));
        let replayer = Arc::new(replayer);
        let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(9).unwrap(), replayer.clone());
        let key = format!("last:{HOST}");
        ctx.state.set(&key, stored).await.unwrap();
        let running = ctx.start(p);
        // Not today: the day is reset to start now.
        run_clock(Duration::from_secs(23 * 3600)).await;
        assert!(replayer.requests().is_empty());
        // Reset to the plugin's first look, within its first minutes.
        let reset = ctx.state.get(&key).await.unwrap().unwrap();
        let reset: i64 = String::from_utf8(reset).unwrap().parse().unwrap();
        assert!(
            (MIDNIGHT + HOUR..=MIDNIGHT + HOUR + 120).contains(&reset),
            "{reset}"
        );
        // Tomorrow it runs as usual.
        run_clock(Duration::from_secs(2 * 3600)).await;
        wait_for("the next day's scrape", || replayer.requests().len() == 1).await;
        running.stop_within().await.unwrap();
        replayer.finish().unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn the_windows_last_minute_keeps_the_day() {
    // Reached at 06:59, with under two minutes left: nothing is sent and
    // nothing is used up, so the next window's 01:00 still scrapes.
    let replayer = Replayer::new(vec![ok(reply(&[6]))]);
    let run = start(
        MIDNIGHT + 6 * HOUR + 59 * 60,
        replayer,
        StaticPrivateTrackers::none(),
    )
    .await;
    run_clock(Duration::from_secs(5 * 60)).await;
    assert!(
        run.replayer.requests().is_empty(),
        "scraped in the last minute"
    );
    let last = run.ctx.state.get(&format!("last:{HOST}")).await.unwrap();
    assert!(last.is_none(), "the day was used up without a scrape");
    run_clock(Duration::from_secs(18 * 3600)).await;
    wait_for("the next window's scrape", || {
        run.replayer.requests().len() == 1
    })
    .await;
    let t = run.requests()[0];
    assert!(in_window(t) && t < MIDNIGHT + 25 * HOUR + 60, "{t}");
    run.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_late_slot_stays_put() {
    // First scrape at 06:50. Each later one is due 24 h on and must land
    // then, not slide past 07:00 and lose a day.
    let replayer = Replayer::new(vec![ok(reply(&[1])), ok(reply(&[2])), ok(reply(&[3]))]);
    let run = start(
        MIDNIGHT + 6 * HOUR + 50 * 60,
        replayer,
        StaticPrivateTrackers::none(),
    )
    .await;
    wait_for("day 1's scrape", || run.replayer.requests().len() == 1).await;
    for day in 2..=3 {
        // To a minute before it's due, then by the second, so the test
        // sees the scrape at the second it happens.
        run_clock(Duration::from_secs(24 * 3600 - 60)).await;
        run_clock_by(Duration::from_secs(120), Duration::from_secs(1)).await;
        wait_for("the next day's scrape", || {
            run.replayer.requests().len() == day
        })
        .await;
    }
    let times = run.requests();
    for pair in times.windows(2) {
        let gap = pair[1] - pair[0];
        assert!((24 * HOUR..=24 * HOUR + 5).contains(&gap), "{times:?}");
    }
    assert!(times.iter().all(|t| in_window(*t)), "{times:?}");
    run.finish().await;
}

// ---- Streamed replies (SRC-12b) ----

/// A tracker that streams a generated full scrape: `entries` entries of
/// about 1 KiB each (so the reply is well over the old 16 MiB buffered
/// limit), in 64 KiB chunks, one every `every` of the paused clock. It
/// builds each chunk when asked, so the reply never exists whole.
struct StreamingTracker {
    entries: u32,
    every: Duration,
    /// Requests seen.
    sent: std::sync::atomic::AtomicUsize,
}

/// Entry `i`'s info hash: unique per entry.
fn streamed_hash(i: u32) -> InfoHash {
    let mut h = [0x77; 20];
    h[..4].copy_from_slice(&i.to_be_bytes());
    InfoHash(h)
}

/// Entry `i`, padded to about 1 KiB with a field the parser skips.
fn padded_entry(i: u32) -> Vec<u8> {
    let mut v = b"20:".to_vec();
    v.extend_from_slice(&streamed_hash(i).0);
    v.extend_from_slice(b"d8:completei1e1:x960:");
    v.extend_from_slice(&[b'p'; 960]);
    v.push(b'e');
    v
}

struct StreamingBody {
    entries: u32,
    every: Duration,
    next: u32,
    started: bool,
    ended: bool,
}

impl lodestar_sources::http::BodyStream for StreamingBody {
    fn next_chunk(
        &mut self,
    ) -> lodestar_sources::BoxFuture<'_, Result<Option<Vec<u8>>, lodestar_sources::HttpError>> {
        Box::pin(async move {
            tokio::time::sleep(self.every).await;
            if self.ended {
                return Ok(None);
            }
            let mut chunk = Vec::with_capacity(64 * 1024 + 1024);
            if !self.started {
                chunk.extend_from_slice(b"d5:filesd");
                self.started = true;
            }
            while chunk.len() < 64 * 1024 && self.next < self.entries {
                chunk.extend_from_slice(&padded_entry(self.next));
                self.next += 1;
            }
            if self.next == self.entries {
                chunk.extend_from_slice(b"ee");
                self.ended = true;
            }
            Ok(Some(chunk))
        })
    }
}

impl lodestar_sources::Fetch for StreamingTracker {
    fn fetch<'a>(
        &'a self,
        _: &'a lodestar_sources::Request,
    ) -> lodestar_sources::BoxFuture<
        'a,
        Result<lodestar_sources::Response, lodestar_sources::HttpError>,
    > {
        // The plugin must stream: a buffered fetch is a bug.
        Box::pin(async { Err(lodestar_sources::HttpError::Transport("buffered".into())) })
    }

    fn fetch_stream<'a>(
        &'a self,
        req: &'a lodestar_sources::Request,
        _: lodestar_sources::http::StreamLimits,
    ) -> lodestar_sources::BoxFuture<
        'a,
        Result<lodestar_sources::http::StreamResponse, lodestar_sources::HttpError>,
    > {
        assert_eq!(req.url, SCRAPE);
        self.sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let body = Box::new(StreamingBody {
            entries: self.entries,
            every: self.every,
            next: 0,
            started: false,
            ended: false,
        });
        Box::pin(async move {
            Ok(lodestar_sources::http::StreamResponse {
                status: 200,
                headers: Vec::new(),
                body,
            })
        })
    }
}

/// What [`scrape_streamed`] saw.
struct Streamed {
    keys: Vec<InfoHash>,
    /// Requests the tracker got.
    sent: usize,
    /// Paused-clock time from the start until the last key came.
    took: Duration,
    /// The tracker's persisted last attempt.
    last: Option<i64>,
}

/// Starts the plugin at 01:00 against `tracker`; collects keys until
/// `want` have come or twice the plugin's timeout has passed.
async fn scrape_streamed(tracker: StreamingTracker, want: usize) -> Streamed {
    let p = Arc::new(FullScrape::new(
        config(),
        Arc::new(StaticPrivateTrackers::none()),
        Arc::new(TokioClock::starting_at(MIDNIGHT + HOUR)),
    ));
    let tracker = Arc::new(tracker);
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(9).unwrap(), tracker.clone());
    let running = ctx.start(p);
    let mut keys = Vec::new();
    let t0 = Instant::now();
    let mut took = Duration::ZERO;
    wait_for("the keys, or the scrape's end", || {
        let before = keys.len();
        keys.extend(ctx.drain().into_iter().map(|d| d.found.key));
        if keys.len() > before {
            took = Instant::now() - t0;
        }
        keys.len() >= want || Instant::now() - t0 > full_scrape::STREAM.timeout * 2
    })
    .await;
    running.stop_within().await.unwrap();
    keys.extend(ctx.drain().into_iter().map(|d| d.found.key));
    let last = ctx
        .state
        .get(&format!("last:{HOST}"))
        .await
        .unwrap()
        .map(|v| String::from_utf8(v).unwrap().parse().unwrap());
    Streamed {
        keys,
        sent: tracker.sent.load(std::sync::atomic::Ordering::SeqCst),
        took,
        last,
    }
}

/// A 24 MiB reply that takes over six minutes to arrive, past both the
/// old buffered limit (16 MiB) and the egress's usual timeout (30 s), is
/// read whole: every key, in order, from one request.
#[tokio::test(start_paused = true)]
async fn a_large_slow_reply_is_streamed_whole() {
    let entries = 24 * 1024;
    let got = scrape_streamed(
        StreamingTracker {
            entries,
            every: Duration::from_secs(1),
            sent: Default::default(),
        },
        entries as usize,
    )
    .await;
    assert_eq!(got.sent, 1);
    assert_eq!(got.keys.len(), entries as usize);
    assert!(
        got.keys
            .iter()
            .zip(0..)
            .all(|(k, i)| *k == streamed_hash(i))
    );
    // About 390 one-second chunks.
    assert!(got.took >= Duration::from_secs(6 * 60), "{:?}", got.took);
}

/// A reply slower than the plugin's ten minutes fails closed: the keys of
/// the entries that came stand, the rest never come, and the day is used.
#[tokio::test(start_paused = true)]
async fn a_reply_over_the_plugins_timeout_fails_closed() {
    let entries = 24 * 1024;
    // About 63 entries per chunk, one chunk every 30 s: over three hours.
    let got = scrape_streamed(
        StreamingTracker {
            entries,
            every: Duration::from_secs(30),
            sent: Default::default(),
        },
        entries as usize,
    )
    .await;
    assert_eq!(got.sent, 1);
    let n = got.keys.len();
    assert!(n > 0 && n < entries as usize, "{n}");
    assert!(
        got.keys
            .iter()
            .zip(0..)
            .all(|(k, i)| *k == streamed_hash(i))
    );
    // Nothing came after the plugin's ten minutes (plus a wait_for step).
    assert!(
        got.took <= full_scrape::STREAM.timeout + Duration::from_secs(1),
        "{:?}",
        got.took
    );
    // The day is used: recorded at the attempt, 01:00.
    assert_eq!(got.last, Some(MIDNIGHT + HOUR));
}

/// A reply over the plugin's 256 MiB cap fails closed at the cap.
#[tokio::test(start_paused = true)]
async fn a_reply_over_the_plugins_cap_fails_closed() {
    // About 270 MiB.
    let entries = 270 * 1024;
    let Streamed { keys, sent, .. } = scrape_streamed(
        StreamingTracker {
            entries,
            every: Duration::ZERO,
            sent: Default::default(),
        },
        entries as usize,
    )
    .await;
    assert_eq!(sent, 1);
    let cap = full_scrape::STREAM.max_bytes as usize;
    // Every entry wholly under the cap came, and none past it.
    let entry = padded_entry(0).len();
    assert!(keys.len() < entries as usize);
    assert!(keys.len() * entry <= cap, "{}", keys.len());
    assert!((keys.len() + 70) * entry >= cap, "{}", keys.len());
}

#[test]
fn the_stream_limits_are_the_plugins_own() {
    use lodestar_sources::http::STREAM_CEILING;
    let s = full_scrape::STREAM;
    assert_eq!(s.max_bytes, 256 * 1024 * 1024);
    assert_eq!(s.timeout, Duration::from_secs(600));
    assert_eq!(s.clamped(), s, "within the kit's ceiling");
    assert!(s.max_bytes < STREAM_CEILING.max_bytes);
    assert_eq!(full_scrape::REPLY_LIMITS.max_bytes, s.max_bytes);
}
