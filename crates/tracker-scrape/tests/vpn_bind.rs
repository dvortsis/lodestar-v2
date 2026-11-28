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

/// The two tests that read the process-wide lookup counter run one at a
/// time, so neither sees the other's lookups.
static LOOKUP_COUNTER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
        matches!(&e, Error::Io(io) if lodestar_network_policy::iface::pin_failure(io).and_then(std::io::Error::raw_os_error) == Some(19)),
        "{e:?}"
    );
    assert_eq!(fake.state.connects.load(Ordering::Relaxed), 0);
    // ENODEV trips the guard at once; the (lying) check brings it back.
    assert_eq!(guard.state(), State::Down(Down::Down));
    assert!(guard.poll().is_up(), "the check said up, so up");
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
    let _one = LOOKUP_COUNTER.lock().await;
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

/// The control: without an interface the same tracker's name is looked
/// up (and the scrape works), so the counter above does count lookups.
#[tokio::test]
async fn without_an_interface_the_name_is_looked_up() {
    let _one = LOOKUP_COUNTER.lock().await;
    let fake = udp_tracker().await;
    fake.state.set(LIVE, SWARM);
    let url = TrackerUrl::parse(&format!("udp://localhost:{}", fake.addr.port())).unwrap();
    let client = http::client(None, None).unwrap();
    let cfg = UdpConfig {
        timeout: Duration::from_secs(10),
        retries: 0,
        ..UdpConfig::default()
    };
    let mut t = Tracker::new(&url, cfg, HttpConfig::default(), &client).unwrap();
    let before = lodestar_tracker_scrape::lookups();
    assert_eq!(t.scrape(&[LIVE]).await.unwrap().counts, [Some(SWARM)]);
    assert!(
        lodestar_tracker_scrape::lookups() > before,
        "the name went to the system resolver"
    );
}

/// A server that answers every request with a redirect to the same path
/// and query on `to`.
async fn redirector(to: std::net::SocketAddr) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                let reply = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{to}{path}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
    addr
}

/// With an interface set no redirect is followed: one could name an
/// address of a family the interface has none of, which would leave with
/// another interface's source address. The control follows it.
#[tokio::test]
async fn a_pinned_client_refuses_every_redirect() {
    let fake = http_tracker().await;
    fake.state.set(LIVE, SWARM);
    let hop = redirector(fake.addr).await;
    let url = TrackerUrl::parse(&format!("http://{hop}/announce")).unwrap();
    let tracker = |client: &reqwest::Client| {
        Tracker::new(&url, UdpConfig::default(), HttpConfig::default(), client).unwrap()
    };

    let pinned = http::client_through(&Interface::parse("lo").unwrap(), None).unwrap();
    let e = tracker(&pinned).scrape(&[LIVE]).await.unwrap_err();
    assert_eq!(
        fake.state.scrapes.load(Ordering::Relaxed),
        0,
        "not followed: {e}"
    );

    let plain = http::client(None, None).unwrap();
    assert_eq!(
        tracker(&plain).scrape(&[LIVE]).await.unwrap().counts,
        [Some(SWARM)]
    );
    assert_eq!(
        fake.state.scrapes.load(Ordering::Relaxed),
        1,
        "the control follows it"
    );
}
