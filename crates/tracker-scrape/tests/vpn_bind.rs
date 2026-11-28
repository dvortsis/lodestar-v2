//! X-vpn-bind: scrapes through one network interface. UDP sockets are
//! pinned before they bind and nothing is sent while the guard is down;
//! the HTTP client is pinned to the device. Linux: it's where an
//! unprivileged test can name an interface that exists (`lo`) and one
//! that doesn't.
#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_network_policy::iface::{Down, Guard, Interface, Link, State};
use lodestar_tracker_scrape::testing::{http_tracker, udp_tracker};
use lodestar_tracker_scrape::{Counts, Error, HttpConfig, Tracker, TrackerUrl, UdpConfig, http};

const LIVE: InfoHash = InfoHash([1; 20]);
const SWARM: Counts = Counts {
    seeders: 3,
    completed: 4,
    leechers: 5,
};

fn udp_cfg(guard: Arc<Guard>) -> UdpConfig {
    UdpConfig {
        timeout: Duration::from_secs(10),
        retries: 0,
        guard: Some(guard),
        ..UdpConfig::default()
    }
}

fn udp(url: &str, guard: Arc<Guard>) -> Tracker {
    let url = TrackerUrl::parse(url).unwrap();
    let client = http::client(None, None).unwrap();
    Tracker::new(&url, udp_cfg(guard), HttpConfig::default(), &client).unwrap()
}

#[tokio::test]
async fn udp_scrapes_through_the_interface_and_not_while_its_down() {
    let fake = udp_tracker().await;
    fake.state.set(LIVE, SWARM);
    let up = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let u = up.clone();
    let guard = Guard::with_probe(Interface::parse("lo").unwrap(), move |i| {
        if u.load(Ordering::Relaxed) {
            lodestar_network_policy::iface::check(i)
        } else {
            Err(Down::Down)
        }
    });
    let mut t = udp(&fake.url(true), guard.clone());
    assert_eq!(t.scrape(&[LIVE]).await.unwrap().counts, [Some(SWARM)]);
    let asked = fake.state.connects.load(Ordering::Relaxed);
    up.store(false, Ordering::Relaxed);
    guard.poll();
    let e = t.scrape(&[LIVE]).await.unwrap_err();
    assert!(
        matches!(&e, Error::Io(io) if io.kind() == std::io::ErrorKind::NetworkDown),
        "{e:?}"
    );
    assert_eq!(
        fake.state.connects.load(Ordering::Relaxed),
        asked,
        "sent nothing"
    );
    // Back up under a new generation: a fresh socket, and it works again.
    up.store(true, Ordering::Relaxed);
    assert_eq!(guard.poll(), State::Up { generation: 2 });
    assert_eq!(t.scrape(&[LIVE]).await.unwrap().counts, [Some(SWARM)]);
}

/// The socket is the safety, not the probe.
#[tokio::test]
async fn a_probe_that_lies_still_cant_send_a_udp_scrape() {
    let fake = udp_tracker().await;
    let guard = Guard::with_probe(
        Interface::parse("lsnone0").unwrap(),
        |_| Ok(Link::default()),
    );
    let mut t = udp(&fake.url(true), guard.clone());
    let e = t.scrape(&[LIVE]).await.unwrap_err();
    assert!(
        matches!(&e, Error::Io(io) if io.raw_os_error() == Some(19)),
        "{e:?}"
    );
    assert_eq!(fake.state.connects.load(Ordering::Relaxed), 0);
    assert_eq!(guard.state(), State::Down(Down::BindFailed));
}

#[tokio::test]
async fn the_http_client_is_pinned_to_the_device() {
    let fake = http_tracker().await;
    fake.state.set(LIVE, SWARM);
    let url = TrackerUrl::parse(&fake.url(false)).unwrap();
    let through = |name: &str| {
        let client = http::client_through(&Interface::parse(name).unwrap(), None).unwrap();
        let cfg = UdpConfig::default();
        Tracker::new(&url, cfg, HttpConfig::default(), &client).unwrap()
    };
    let mut lo = through("lo");
    assert_eq!(lo.scrape(&[LIVE]).await.unwrap().counts, [Some(SWARM)]);
    assert_eq!(fake.state.scrapes.load(Ordering::Relaxed), 1);
    let mut gone = through("lsnone0");
    assert!(gone.scrape(&[LIVE]).await.is_err());
    assert_eq!(
        fake.state.scrapes.load(Ordering::Relaxed),
        1,
        "never reached"
    );
}

/// No name goes to the system resolver with an interface set: a tracker
/// named by host isn't asked at all, over UDP or HTTP; one named by
/// address is (the tests above).
#[tokio::test]
async fn trackers_named_by_host_are_not_looked_up() {
    let before = lodestar_tracker_scrape::lookups();
    let fake = udp_tracker().await;
    let guard = Guard::new(Interface::parse("lo").unwrap());
    let mut t = udp(&format!("udp://localhost:{}", fake.addr.port()), guard);
    assert!(matches!(t.scrape(&[LIVE]).await, Err(Error::Resolve)));
    assert_eq!(fake.state.connects.load(Ordering::Relaxed), 0);

    let fake = http_tracker().await;
    let url =
        TrackerUrl::parse(&format!("http://localhost:{}/announce", fake.addr.port())).unwrap();
    let client = http::client_through(&Interface::parse("lo").unwrap(), None).unwrap();
    let mut t = Tracker::new(&url, UdpConfig::default(), HttpConfig::default(), &client).unwrap();
    assert!(t.scrape(&[LIVE]).await.is_err());
    assert_eq!(fake.state.scrapes.load(Ordering::Relaxed), 0);
    assert_eq!(
        lodestar_tracker_scrape::lookups(),
        before,
        "no name went to the system resolver"
    );
}
