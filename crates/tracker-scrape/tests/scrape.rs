//! Both clients against the in-process fake trackers.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_tracker_scrape::testing::{Fake, http_tracker, udp_tracker};
use lodestar_tracker_scrape::{
    Counts, Deny, Error, HttpConfig, Tracker, TrackerUrl, UdpConfig, http, udp,
};

const LIVE: InfoHash = InfoHash([1; 20]);
const DEAD: InfoHash = InfoHash([2; 20]);
const SWARM: Counts = Counts {
    seeders: 12,
    completed: 340,
    leechers: 5,
};

fn udp_cfg() -> UdpConfig {
    UdpConfig {
        timeout: Duration::from_millis(300),
        retries: 1,
        ..UdpConfig::default()
    }
}

fn tracker(fake: &Fake, udp: bool) -> Tracker {
    let url = TrackerUrl::parse(&fake.url(udp)).unwrap();
    let client = http::client(None, None).unwrap();
    Tracker::new(&url, udp_cfg(), HttpConfig::default(), &client).unwrap()
}

async fn setup(udp: bool) -> (Fake, Tracker) {
    let fake = if udp {
        udp_tracker().await
    } else {
        http_tracker().await
    };
    fake.state.set(LIVE, SWARM);
    let t = tracker(&fake, udp);
    (fake, t)
}

#[tokio::test]
async fn scrapes_in_request_order() {
    for udp in [true, false] {
        let (fake, mut t) = setup(udp).await;
        let r = t.scrape(&[DEAD, LIVE]).await.unwrap();
        assert_eq!(
            r.counts,
            [Some(Counts::default()), Some(SWARM)],
            "udp={udp}"
        );
        assert!(!r.counts[0].unwrap().is_reporting());
        assert_eq!(fake.state.hashes.load(Ordering::Relaxed), 2);
        if !udp {
            assert_eq!(*fake.state.last_path.lock().unwrap(), "/scrape");
        }
    }
}

#[tokio::test]
async fn udp_reuses_its_connection_id() {
    let (fake, mut t) = setup(true).await;
    for _ in 0..3 {
        t.scrape(&[LIVE]).await.unwrap();
    }
    assert_eq!(fake.state.connects.load(Ordering::Relaxed), 1);
    assert_eq!(fake.state.scrapes.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn udp_reconnects_when_its_id_is_rejected() {
    let (fake, mut t) = setup(true).await;
    t.scrape(&[LIVE]).await.unwrap();
    fake.state.forget_connections.store(true, Ordering::Relaxed);
    let r = t.scrape(&[LIVE]).await.unwrap();
    assert_eq!(r.counts, [Some(SWARM)]);
    assert_eq!(fake.state.connects.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn udp_retries_a_lost_packet_and_skips_stray_replies() {
    let (fake, mut t) = setup(true).await;
    fake.state.drop_next.store(1, Ordering::Relaxed);
    fake.state.stray_replies.store(true, Ordering::Relaxed);
    let r = t.scrape(&[LIVE]).await.unwrap();
    assert_eq!(r.counts, [Some(SWARM)]);
}

#[tokio::test]
async fn udp_gives_up_after_retries() {
    let (fake, mut t) = setup(true).await;
    fake.state.drop_next.store(10, Ordering::Relaxed);
    assert!(matches!(t.scrape(&[LIVE]).await, Err(Error::Timeout)));
    assert_eq!(
        fake.state.drop_next.load(Ordering::Relaxed),
        8,
        "two attempts"
    );
}

#[tokio::test]
async fn refusals_carry_the_message() {
    for udp in [true, false] {
        let (fake, mut t) = setup(udp).await;
        *fake.state.refuse.lock().unwrap() = Some("rate limited".into());
        match t.scrape(&[LIVE]).await {
            Err(Error::Refused(m)) => assert_eq!(m, "rate limited", "udp={udp}"),
            other => panic!("udp={udp}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn truncated_replies_leave_gaps() {
    for udp in [true, false] {
        let (fake, mut t) = setup(udp).await;
        *fake.state.truncate_to.lock().unwrap() = Some(1);
        let r = t.scrape(&[LIVE, DEAD]).await.unwrap();
        assert_eq!(r.counts, [Some(SWARM), None], "udp={udp}");
    }
}

#[tokio::test]
async fn http_errors() {
    let (fake, mut t) = setup(false).await;
    fake.state.status.store(503, Ordering::Relaxed);
    assert!(matches!(t.scrape(&[LIVE]).await, Err(Error::Status(503))));
    fake.state.status.store(0, Ordering::Relaxed);
    fake.state.oversize.store(true, Ordering::Relaxed);
    assert!(matches!(t.scrape(&[LIVE]).await, Err(Error::TooLarge)));
    fake.state.oversize.store(false, Ordering::Relaxed);
    *fake.state.min_interval.lock().unwrap() = Some(600);
    let r = t.scrape(&[LIVE]).await.unwrap();
    assert_eq!(r.retry_after, Some(Duration::from_secs(600)));
}

#[tokio::test]
async fn batch_limits() {
    let (_fake, mut t) = setup(true).await;
    assert_eq!(t.max_hashes(), udp::MAX_HASHES);
    assert!(matches!(t.scrape(&[]).await, Err(Error::BadRequest(_))));
    let many = vec![LIVE; udp::MAX_HASHES + 1];
    assert!(matches!(t.scrape(&many).await, Err(Error::BadRequest(_))));
    let full: Vec<InfoHash> = (0..udp::MAX_HASHES as u8)
        .map(|i| InfoHash([i; 20]))
        .collect();
    assert_eq!(t.scrape(&full).await.unwrap().counts.len(), udp::MAX_HASHES);
}

#[tokio::test]
async fn unreachable_udp_tracker_fails_fast_or_times_out() {
    // Nothing listens there: either an ICMP refusal or a timeout.
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = socket.local_addr().unwrap();
    drop(socket);
    let url = TrackerUrl::parse(&format!("udp://{addr}")).unwrap();
    let client = http::client(None, None).unwrap();
    let mut t = Tracker::new(&url, udp_cfg(), HttpConfig::default(), &client).unwrap();
    assert!(matches!(
        t.scrape(&[LIVE]).await,
        Err(Error::Io(_) | Error::Timeout)
    ));
}

/// Requests that reached `fake`.
fn reached(fake: &Fake) -> u64 {
    fake.state.connects.load(Ordering::Relaxed) + fake.state.scrapes.load(Ordering::Relaxed)
}

fn denying(url: &str, deny: &Deny) -> Tracker {
    let url = TrackerUrl::parse(url).unwrap();
    let client = http::client(None, Some(deny)).unwrap();
    let udp = UdpConfig {
        deny: Some(deny.clone()),
        ..udp_cfg()
    };
    let http = HttpConfig {
        deny: Some(deny.clone()),
        ..HttpConfig::default()
    };
    Tracker::new(&url, udp, http, &client).unwrap()
}

#[tokio::test]
async fn blocklisted_trackers_are_never_contacted() {
    let listed = Arc::new(AtomicBool::new(false));
    let deny = {
        let listed = listed.clone();
        Deny(Arc::new(move |ip: IpAddr| {
            listed.load(Ordering::Relaxed) && ip.is_loopback()
        }))
    };
    for udp in [true, false] {
        listed.store(false, Ordering::Relaxed);
        let fake = if udp {
            udp_tracker().await
        } else {
            http_tracker().await
        };
        fake.state.set(LIVE, SWARM);
        let mut t = denying(&fake.url(udp), &deny);
        let r = t.scrape(&[LIVE]).await.unwrap();
        assert_eq!(r.counts, [Some(SWARM)], "allowed: udp={udp}");
        // Listed while the socket is open: refused before anything is sent.
        listed.store(true, Ordering::Relaxed);
        let before = reached(&fake);
        let e = t.scrape(&[LIVE]).await.unwrap_err();
        assert!(matches!(e, Error::Denied), "udp={udp}: {e}");
        assert_eq!(reached(&fake), before, "udp={udp}");

        // By name: the listed addresses are never connected to.
        let by_name = fake.url(udp).replace("127.0.0.1", "localhost");
        let e = denying(&by_name, &deny).scrape(&[LIVE]).await.unwrap_err();
        assert!(matches!(e, Error::Denied), "by name, udp={udp}: {e}");
        assert_eq!(reached(&fake), before, "by name, udp={udp}");
    }
}
