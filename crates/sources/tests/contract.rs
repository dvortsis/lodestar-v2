//! Plugin contract v2 (§18.1, §18.9): caps can't be loosened, the limiter
//! never exceeds them, back-off stays bounded, plugins stop within a
//! second of shutdown, and fixtures replay without a network.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_sources::http::{WaitReason, backoff_delay};
use lodestar_sources::testing::{DEADLINE, FakeCtx, Replayer, STOP_WITHIN};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Claims, Client, Consent, Distribution, EMPTY_SCHEMA, Found,
    HostLimiter, HostRate, HttpError, Kind, Limits, Manifest, Network, PluginError, Policy,
    Priority, Request, Risk, Schedule, SourceCtx, SourcePlugin, UserLimits, Window,
};
use lodestar_store::SourceId;
use proptest::prelude::*;
use tokio::time::Instant;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn src() -> SourceId {
    SourceId::new(5).unwrap()
}

// ---- Caps against user settings ----

fn caps() -> impl Strategy<Value = Caps> {
    (
        1u64..100_000,
        1u64..10_000,
        1u32..8,
        0u32..4,
        1u64..600,
        1u64..100_000,
    )
        .prop_map(|(poll, iv, burst, scrapes, base, max)| Caps {
            min_poll_interval: Duration::from_millis(poll),
            per_host: HostRate {
                interval: Duration::from_millis(iv),
                burst,
            },
            max_full_scrapes_per_day: scrapes,
            backoff: Backoff {
                base: secs(base),
                max: secs(max),
            },
        })
}

fn user() -> impl Strategy<Value = UserLimits> {
    let d = || proptest::option::of((0u64..200_000).prop_map(Duration::from_millis));
    (
        d(),
        proptest::option::of((0u64..20_000, 0u32..20)),
        proptest::option::of(0u32..10),
        proptest::option::of((0u64..1000, 0u64..200_000)),
    )
        .prop_map(|(poll, rate, scrapes, backoff)| UserLimits {
            poll_interval: poll,
            per_host: rate.map(|(iv, burst)| HostRate {
                interval: Duration::from_millis(iv),
                burst,
            }),
            max_full_scrapes_per_day: scrapes,
            backoff: backoff.map(|(b, m)| Backoff {
                base: secs(b),
                max: secs(m),
            }),
        })
}

fn policy(caps: Caps) -> Policy {
    Policy {
        caps,
        ..Policy::STRICT
    }
}

proptest! {
    /// No user setting loosens a hard cap (§18.1, §18.4).
    #[test]
    fn user_settings_never_loosen_caps(caps in caps(), user in user()) {
        let limits = policy(caps).limits(&user);
        prop_assert!(limits.within(&caps), "{limits:?} looser than {caps:?}");
    }

    /// Stricter user settings are kept.
    #[test]
    fn stricter_user_settings_win(caps in caps(), extra in 0u64..10_000) {
        let user = UserLimits {
            poll_interval: Some(caps.min_poll_interval + Duration::from_millis(extra)),
            per_host: Some(HostRate {
                interval: caps.per_host.interval + Duration::from_millis(extra),
                burst: 1,
            }),
            max_full_scrapes_per_day: Some(0),
            backoff: None,
        };
        let limits = policy(caps).limits(&user);
        prop_assert_eq!(limits.poll_interval, user.poll_interval.unwrap());
        prop_assert_eq!(limits.per_host, user.per_host.unwrap());
        prop_assert_eq!(limits.max_full_scrapes_per_day, 0);
    }

    /// Whatever the request times, every span of time `t` sees at most
    /// `burst + t / interval` grants to one host.
    #[test]
    fn host_rate_never_exceeded(
        iv in 1u64..5_000,
        burst in 1u32..6,
        gaps in proptest::collection::vec(0u64..3_000, 1..200),
    ) {
        let rate = HostRate { interval: Duration::from_millis(iv), burst };
        let limiter = HostLimiter::with_seed(limits_with(rate, 0), 1);
        let t0 = Instant::now();
        let mut now = t0;
        let mut grants = Vec::new();
        for g in gaps {
            now += Duration::from_millis(g);
            match limiter.try_acquire("h", now) {
                Ok(()) => grants.push(now),
                Err(w) => {
                    prop_assert_eq!(w.reason, WaitReason::Rate);
                    prop_assert!(w.until > now);
                }
            }
        }
        for (i, a) in grants.iter().enumerate() {
            for (j, b) in grants.iter().enumerate().skip(i) {
                let span = b.duration_since(*a).as_nanos();
                let allowed = u128::from(burst) + span / rate.interval.as_nanos();
                prop_assert!((j - i + 1) as u128 <= allowed);
            }
        }
    }

    /// A refused request names a time at which it would be granted.
    #[test]
    fn waits_are_honest(iv in 1u64..5_000, burst in 1u32..6, n in 1usize..20) {
        let rate = HostRate { interval: Duration::from_millis(iv), burst };
        let limiter = HostLimiter::with_seed(limits_with(rate, 0), 1);
        let now = Instant::now();
        for _ in 0..n {
            if let Err(w) = limiter.try_acquire("h", now) {
                prop_assert!(limiter.try_acquire("h", w.until).is_ok());
                break;
            }
        }
    }

    /// No more full scrapes per host than the daily cap in any 24 hours.
    #[test]
    fn full_scrapes_capped_per_day(
        max in 0u32..4,
        gaps in proptest::collection::vec(0u64..30_000, 1..100),
    ) {
        let limiter = HostLimiter::with_seed(limits_with(HostRate::per_second(1000), max), 1);
        let mut now = Instant::now();
        let mut grants = Vec::new();
        for g in gaps {
            now += secs(g);
            if limiter.try_full_scrape("t", now).is_ok() {
                grants.push(now);
            }
        }
        for (i, a) in grants.iter().enumerate() {
            let in_day = grants[i..].iter().take_while(|b| b.duration_since(*a) < secs(86_400)).count();
            prop_assert!(in_day <= max as usize);
        }
    }

    /// Back-off is bounded by the ceiling, never below half the floor,
    /// and its envelope never shrinks as failures grow.
    #[test]
    fn backoff_bounded(
        base in 1u64..100_000,
        max in 1u64..10_000_000,
        failures in 1u32..200,
        entropy: u64,
    ) {
        let b = Backoff { base: Duration::from_millis(base), max: Duration::from_millis(max) };
        let ceiling = b.max.max(b.base);
        let d = backoff_delay(b, failures, entropy);
        prop_assert!(d <= ceiling);
        prop_assert!(d >= b.base.min(ceiling) / 2);
        let hi = backoff_delay(b, failures, u64::MAX);
        let next_lo = backoff_delay(b, failures + 1, 0);
        prop_assert!(next_lo >= hi / 2);
        prop_assert!(backoff_delay(b, failures + 1, u64::MAX) >= hi);
    }

    /// The next open minute is open, and nothing before it is.
    #[test]
    fn schedule_next_open(start in 0u16..1440, end in 0u16..1440, m in 0u16..1440) {
        let w: &'static [Window] = Box::leak(vec![Window::new(start, end)].into_boxed_slice());
        let s = Schedule(w);
        let d = s.minutes_until_open(m);
        prop_assert!(d < 1440);
        prop_assert!(s.is_open((m + d) % 1440));
        for e in 0..d {
            prop_assert!(!s.is_open((m + e) % 1440));
        }
    }
}

fn limits_with(per_host: HostRate, scrapes: u32) -> Limits {
    Limits {
        poll_interval: secs(60),
        per_host,
        max_full_scrapes_per_day: scrapes,
        backoff: Backoff {
            base: secs(1),
            max: secs(60),
        },
    }
}

#[test]
fn windows_wrap_midnight() {
    let night = Window::new(22 * 60, 2 * 60);
    assert!(night.contains(23 * 60) && night.contains(60) && !night.contains(12 * 60));
    let s = Schedule(&[Window {
        start: 60,
        end: 7 * 60,
    }]);
    assert_eq!(s.minutes_until_open(0), 60);
    assert_eq!(s.minutes_until_open(7 * 60), 18 * 60);
    assert_eq!(Schedule::ALWAYS.minutes_until_open(123), 0);
}

#[test]
fn strict_defaults() {
    let kit = lodestar_sources::Kit::default();
    let strict = Policy::STRICT.limits(&UserLimits::default());
    assert_eq!(*kit.limiter.limits(), strict);
    assert_eq!(strict.max_full_scrapes_per_day, 0);
    assert_eq!(Policy::STRICT.distribution, Distribution::Coordinator);
    assert_eq!(Policy::STRICT.config_schema, EMPTY_SCHEMA);
}

// ---- A feed plugin on the fake context ----

const URL: &str = "https://feed.example/rss";

static MANIFEST: Manifest = Manifest {
    id: "test-feed",
    name: "Test feed",
    version: "0",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    endpoints: &[],
    risk: Risk::Low,
    consent: Consent::External,
    default_priority: Priority::Low,
    enabled_by_default: false,
};

static POLICY: Policy = Policy {
    caps: Caps {
        min_poll_interval: Duration::from_secs(300),
        per_host: HostRate::per_second(1),
        max_full_scrapes_per_day: 0,
        backoff: Backoff {
            base: Duration::from_secs(30),
            max: Duration::from_secs(3600),
        },
    },
    ..Policy::STRICT
};

/// Polls one feed: each body line is `<40 hex> <name>`. Keeps the ETag in
/// its state and stops promptly on shutdown, whatever it's waiting on.
struct Feed {
    fetch: Arc<Replayer>,
}

impl SourcePlugin for Feed {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let SourceCtx {
                emitter,
                mut shutdown,
                kit,
                ..
            } = ctx;
            let client = Client::new(self.fetch.clone(), kit.limiter.clone());
            let poll = kit.limiter.limits().poll_interval;
            let err = |e: &dyn std::fmt::Display| PluginError(e.to_string());
            while !shutdown.is_requested() {
                let mut req = Request::get(URL);
                if let Some(tag) = kit.state.get("etag").await.map_err(|e| err(&e))? {
                    req = req.header("If-None-Match", String::from_utf8_lossy(&tag));
                }
                let res = tokio::select! {
                    r = client.send(&req) => r,
                    _ = shutdown.requested() => break,
                };
                match res {
                    Ok(res) if res.status == 200 => {
                        for line in String::from_utf8_lossy(&res.body).lines() {
                            let Some((hex, name)) = line.split_once(' ') else {
                                continue;
                            };
                            let Some(key) = parse_hex(hex) else { continue };
                            let found = Found::key(key, 0).with_claims(Claims {
                                name: Some(name.to_owned()),
                                ..Claims::default()
                            });
                            if emitter.emit(found).await.is_err() {
                                return Ok(());
                            }
                        }
                        if let Some(tag) = res.header("etag") {
                            kit.state
                                .set("etag", tag.as_bytes().to_vec())
                                .await
                                .map_err(|e| err(&e))?;
                        }
                    }
                    Ok(_) => {}
                    // Blocked: wait out the block, but stay stoppable.
                    Err(HttpError::Blocked { until }) => tokio::select! {
                        _ = tokio::time::sleep_until(until) => continue,
                        _ = shutdown.requested() => break,
                    },
                    Err(_) => {}
                }
                tokio::select! {
                    _ = tokio::time::sleep(poll) => {}
                    _ = shutdown.requested() => break,
                }
            }
            Ok(())
        })
    }
}

fn parse_hex(s: &str) -> Option<InfoHash> {
    if s.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(InfoHash(out))
}

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

/// Writes a fixture case to a fresh directory under the target dir.
fn fixture(case: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("contract-{}-{case}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    dir
}

/// Waits (on paused time) until `cond` holds, failing after [`DEADLINE`].
async fn until(mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE * 1000;
    while !cond() {
        assert!(Instant::now() < deadline, "condition not met");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn feed_replays_fixtures_and_keeps_its_cursor() {
    let body = format!("{HASH} Ubuntu 24.04\nnot a hash line\n");
    let dir = fixture(
        "etag",
        &[
            (
                "001.http",
                format!("GET {URL}\n\nHTTP 200\nETag: \"v1\"\n").as_bytes(),
            ),
            ("001.body", body.as_bytes()),
            (
                "002.http",
                format!("GET {URL}\r\nIf-None-Match: \"v1\"\r\n\r\nHTTP 304\r\n").as_bytes(),
            ),
        ],
    );
    let replayer = Arc::new(Replayer::load(&dir).unwrap());
    let plugin = Arc::new(Feed {
        fetch: replayer.clone(),
    });
    let mut fake = FakeCtx::new(plugin.as_ref(), src());
    let running = fake.start(plugin);

    let d = fake.next().await.expect("a discovery");
    assert_eq!(d.source, src());
    assert_eq!(d.found.key, parse_hex(HASH).unwrap());
    assert_eq!(
        d.found.claims.as_ref().and_then(|c| c.name.as_deref()),
        Some("Ubuntu 24.04")
    );
    until(|| replayer.requests().len() >= 2).await;
    replayer.finish().unwrap();
    let times: Vec<_> = replayer.requests().iter().map(|(t, _)| *t).collect();
    assert!(times[1].duration_since(times[0]) >= POLICY.caps.min_poll_interval);
    assert_eq!(fake.state.len(), 1);

    let stopped = running.stop().await.unwrap();
    stopped.result.unwrap();
    assert!(stopped.took <= STOP_WITHIN, "took {:?}", stopped.took);
    assert!(fake.drain().is_empty());
}

#[tokio::test(start_paused = true)]
async fn blocked_by_source_stops_requests_and_still_cancels() {
    let dir = fixture(
        "blocked",
        &[(
            "001.http",
            format!("GET {URL}\n\nHTTP 429\nRetry-After: 7200\n").as_bytes(),
        )],
    );
    let replayer = Arc::new(Replayer::load(&dir).unwrap());
    let plugin = Arc::new(Feed {
        fetch: replayer.clone(),
    });
    let mut fake = FakeCtx::new(plugin.as_ref(), src());
    let running = fake.start(plugin);
    until(|| replayer.requests().len() == 1).await;
    let host = "feed.example";
    until(|| {
        matches!(
            fake.limiter.state(host, Instant::now()),
            lodestar_sources::HostState::Blocked { .. }
        )
    })
    .await;
    // Nothing more reaches the source while it's blocked. The clock is
    // paused, so this advances virtual time to just short of the block's
    // end rather than waiting.
    tokio::time::sleep(secs(3600)).await;
    assert_eq!(replayer.requests().len(), 1);
    let stopped = running.stop().await.unwrap();
    assert!(stopped.took <= STOP_WITHIN);
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn transport_errors_back_off() {
    let dir = fixture(
        "down",
        &[
            (
                "001.http",
                format!("GET {URL}\n\nERROR connection refused\n").as_bytes(),
            ),
            ("002.http", format!("GET {URL}\n\nHTTP 200\n").as_bytes()),
        ],
    );
    let replayer = Arc::new(Replayer::load(&dir).unwrap());
    let plugin = Arc::new(Feed {
        fetch: replayer.clone(),
    });
    let mut fake = FakeCtx::new(plugin.as_ref(), src());
    let running = fake.start(plugin);
    until(|| replayer.requests().len() == 1).await;
    until(|| {
        matches!(
            fake.limiter.state("feed.example", Instant::now()),
            lodestar_sources::HostState::BackingOff { failures: 1, .. }
        )
    })
    .await;
    // The next answer is normal, so the back-off resets.
    until(|| replayer.requests().len() == 2).await;
    until(|| {
        fake.limiter.state("feed.example", Instant::now()) == lodestar_sources::HostState::Ready
    })
    .await;
    running.stop().await.unwrap().result.unwrap();
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn shutdown_before_any_request() {
    let replayer = Arc::new(Replayer::new(Vec::new()));
    let plugin = Arc::new(Feed {
        fetch: replayer.clone(),
    });
    let mut fake = FakeCtx::new(plugin.as_ref(), src());
    fake.shutdown();
    let running = fake.start(plugin);
    let stopped = running.stop().await.unwrap();
    assert!(stopped.took <= STOP_WITHIN);
    assert!(replayer.requests().is_empty());
}

#[tokio::test]
async fn emitter_clamps_claims() {
    let plugin = Feed {
        fetch: Arc::new(Replayer::new(Vec::new())),
    };
    let mut fake = FakeCtx::new(&plugin, src());
    let ctx = fake.take();
    let found = Found::key(InfoHash([1; 20]), 0).with_claims(Claims {
        name: Some("n".repeat(10_000)),
        ..Claims::default()
    });
    ctx.emitter.emit(found).await.unwrap();
    let d = fake.next().await.unwrap();
    assert_eq!(
        d.found.claims.unwrap().name.unwrap().len(),
        lodestar_sources::claims::MAX_NAME
    );
    // A bare key carries nothing extra.
    assert!(Found::key(InfoHash([2; 20]), 0).claims.is_none());
    assert!(
        Found::key(InfoHash([2; 20]), 0)
            .with_claims(Claims::default())
            .claims
            .is_none()
    );
}
