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
    start: Instant,
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
        start: Instant::now(),
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
        let _ = self.start;
        self.ctx.drain()
    }
}

/// Moves the paused clock on by `total`, a minute at a time, letting the
/// plugin run at each step.
async fn run_clock(total: Duration) {
    let step = Duration::from_secs(60);
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
        StaticPrivateTrackers::default(),
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
    let run = start(MIDNIGHT + HOUR, replayer, StaticPrivateTrackers::default()).await;
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
        StaticPrivateTrackers::default(),
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
        StaticPrivateTrackers::default(),
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
        StaticPrivateTrackers::default(),
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
        Arc::new(StaticPrivateTrackers::default()),
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
        Arc::new(StaticPrivateTrackers::default()),
        Arc::new(TokioClock::starting_at(0)),
    );
    let m = p.manifest();
    assert_eq!(m.id, "full_scrape");
    assert_eq!(m.kinds, [Kind::Discovery]);
    assert_eq!(m.consent, Consent::External);
    assert!(m.endpoints.is_empty() && !m.enabled_by_default);
    assert!(p.endpoints().is_some());

    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::Singleton);
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
