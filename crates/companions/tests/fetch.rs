//! CMP-1's download and verification contract: only the pinned bytes, from
//! the entry's hosts, within the cap and the timeouts, land as a verified
//! file; everything else leaves nothing behind. Upstream minisign
//! signatures verify with the pinned key only.
//!
//! The network tests use a real HTTP server on loopback reached through a
//! real `GuardedClient`, as the updater's download tests do: the allowlist
//! sees the catalogue's `https` URL, and only where it's fetched from
//! changes, as a DNS override would.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_companions::catalogue::{Arch, Catalogue, Pin, Platform};
use lodestar_companions::fetch::{
    self, Chunks, FetchError, Guarded, Limits, MAX_SIGNATURE, Opened, Source, USER_AGENT,
};
use lodestar_companions::verify::{self, VerifyError};
use lodestar_companions::{Entry, fetch::Verified};
use lodestar_integrations::net::{GuardedClient, NetPolicy};
use lodestar_update::testing::TestKey;
use reqwest::Url;
use serde_json::json;
use sha2::Digest;

type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

const URL: &str = "https://github.com/userdocs/qbittorrent-nox-static/releases/download/release-5.0.3_v2.0.11/x86_64-qbittorrent-nox";
const ASSET: &str = "https://release-assets.githubusercontent.com/x/qbittorrent-nox?sig=Zq7Secret";

/// The download slot is per companion and per process: tests that
/// download take turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn binary() -> Vec<u8> {
    (0..4500u32).map(|i| (i % 251) as u8).collect()
}

/// A catalogue whose qBittorrent Linux build is pinned to `body`.
fn catalogue(body: &[u8]) -> Catalogue {
    let sha = hex(&sha2::Sha256::digest(body));
    let v = json!({
        "format": 1,
        "entries": [{
            "id": "qbittorrent",
            "name": "qBittorrent",
            "licence": {"spdx": "GPL-2.0-or-later", "url": "https://github.com/qbittorrent/qBittorrent/blob/master/COPYING"},
            "homepage": "https://www.qbittorrent.org/",
            "needed_for": ["download_client"],
            "opt_in": false,
            "hosts": ["github.com", "release-assets.githubusercontent.com"],
            "native": {"release": "https://github.com/userdocs/qbittorrent-nox-static/releases/download/", "artifacts": [
                {"platform": "linux", "arch": "x86_64", "archive": "binary", "executable": null,
                 "pin": {"version": "5.0.3", "url": URL, "size": body.len(), "sha256": sha,
                         "upstream_sig": {"kind": "none"}}}
            ]},
            "container": null,
            "ports": [{"name": "webui", "port": 8080, "proto": "tcp", "exposure": "internal"}],
            "health": null,
            "templates": ["qbittorrent_conf"]
        }]
    });
    Catalogue::from_json(&v.to_string()).unwrap()
}

fn entry(c: &Catalogue) -> &Entry {
    c.get("qbittorrent").unwrap()
}

fn pin(c: &Catalogue) -> &Pin {
    entry(c)
        .artifact(Platform::Linux, Arch::X86_64)
        .unwrap()
        .pin()
        .unwrap()
}

fn downloads(data: &Path) -> Vec<String> {
    let Ok(list) = std::fs::read_dir(fetch::folder(data)) else {
        return vec![];
    };
    let mut names: Vec<String> = list
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A served URL: its status, `Location` and body.
type Route = (u16, Option<String>, Vec<u8>);

/// An in-memory source: URL → route, bodies in chunks of 1000 bytes;
/// every request recorded.
#[derive(Default)]
struct Fake {
    routes: Mutex<HashMap<String, Route>>,
    asked: Mutex<Vec<String>>,
}

impl Fake {
    fn serve(&self, url: &str, status: u16, location: Option<&str>, body: &[u8]) {
        self.routes.lock().unwrap().insert(
            url.to_string(),
            (status, location.map(str::to_owned), body.to_vec()),
        );
    }

    fn asked(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

struct Memory(Vec<Vec<u8>>);

impl Chunks for Memory {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, FetchError>> {
        let c = (!self.0.is_empty()).then(|| self.0.remove(0));
        Box::pin(async move { Ok(c) })
    }
}

impl Source for Fake {
    fn open<'a>(&'a self, url: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
        self.asked.lock().unwrap().push(url.to_string());
        let route = self.routes.lock().unwrap().get(url.as_str()).cloned();
        Box::pin(async move {
            let (status, location, body) = route.unwrap_or((404, None, vec![]));
            Ok(Opened {
                status,
                location,
                body: Box::new(Memory(body.chunks(1000).map(<[u8]>::to_vec).collect())),
            })
        })
    }
}

/// A source whose body sends `chunks` chunks of 100 bytes, `every`
/// apart, then stalls for good (`chunks: 0` stalls at once).
#[derive(Clone)]
struct Drip {
    chunks: usize,
    every: Duration,
}

impl Chunks for Drip {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, FetchError>> {
        if self.chunks == 0 {
            return Box::pin(std::future::pending());
        }
        self.chunks -= 1;
        let every = self.every;
        // Paces the sender; nothing assumes work finished.
        Box::pin(async move {
            tokio::time::sleep(every).await;
            Ok(Some(vec![7u8; 100]))
        })
    }
}

impl Source for Drip {
    fn open<'a>(&'a self, _: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
        let body = Box::new(self.clone());
        Box::pin(async move {
            Ok(Opened {
                status: 200,
                location: None,
                body,
            })
        })
    }
}

async fn get(src: &dyn Source, data: &Path, c: &Catalogue) -> Result<Verified, FetchError> {
    fetch::fetch_from(src, data, entry(c), pin(c), &mut |_| {}).await
}

#[tokio::test]
async fn the_pinned_bytes_arrive_verified_and_private() {
    use std::io::Read;
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let f = Fake::default();
    f.serve(URL, 302, Some(ASSET), b"");
    f.serve(ASSET, 200, None, &b);
    let mut seen = vec![];
    let mut v = fetch::fetch_from(&f, data.path(), entry(&c), pin(&c), &mut |n| seen.push(n))
        .await
        .unwrap();
    assert_eq!(seen, [1000, 2000, 3000, 4000, 4500], "progress per chunk");
    assert_eq!(v.size(), b.len() as u64);
    assert_eq!(v.sha256(), &pin(&c).sha256);
    let mut got = vec![];
    v.read_to_end(&mut got).unwrap();
    assert_eq!(got, b, "the handle reads the file from its start");
    assert_eq!(downloads(data.path()), ["qbittorrent_5.0.3.verified"]);
    assert!(
        v.path()
            .starts_with(data.path().join("companions").join(".downloads"))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(v.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    v.discard().unwrap();
    assert!(downloads(data.path()).is_empty());
}

#[tokio::test]
async fn a_wrong_hash_or_size_leaves_nothing() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let f = Fake::default();
    let mut other = b.clone();
    other[10] ^= 1;
    let mut one_more = b.clone();
    one_more.push(0);
    let mut long = b.clone();
    long.extend(std::iter::repeat_n(0u8, 100_000));
    for (body, want, what) in [
        (other, FetchError::Mismatch, "same size, another body"),
        (b[..4000].to_vec(), FetchError::Mismatch, "short"),
        (b[..4499].to_vec(), FetchError::Mismatch, "one byte short"),
        (one_more, FetchError::TooLarge, "one byte long"),
        (long, FetchError::TooLarge, "long"),
    ] {
        f.serve(URL, 200, None, &body);
        assert_eq!(get(&f, data.path(), &c).await.err(), Some(want), "{what}");
        assert!(downloads(data.path()).is_empty(), "{what}");
    }
}

#[tokio::test]
async fn a_long_body_stops_at_the_cap_unread() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    // 50 chunks of 100 bytes against a 4,500-byte pin: the 46th stops it.
    let src = Drip {
        chunks: 50,
        every: Duration::from_millis(1),
    };
    let mut last = 0;
    let got = fetch::fetch_from(&src, data.path(), entry(&c), pin(&c), &mut |n| last = n).await;
    assert_eq!(got.err(), Some(FetchError::TooLarge));
    assert_eq!(last, 4500, "nothing past the pin was passed on");
    assert!(downloads(data.path()).is_empty());
}

#[tokio::test]
async fn every_hop_must_be_https_on_the_entrys_hosts() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    for to in [
        "https://evil.example/qbittorrent-nox",
        "http://github.com/qbittorrent-nox",
        "http://release-assets.githubusercontent.com/x",
        // On DOWNLOAD_HOSTS, but not on this entry's allowlist.
        "https://objects.githubusercontent.com/x",
        "https://github.com:8443/x",
        "https://user:pw@github.com/x",
        "https://github.com./x",
    ] {
        let f = Fake::default();
        f.serve(URL, 302, Some(to), b"");
        f.serve(to, 200, None, &b);
        assert_eq!(
            get(&f, data.path(), &c).await.err(),
            Some(FetchError::Redirect),
            "{to}"
        );
        assert_eq!(f.asked(), 1, "{to} isn't asked");
        assert!(downloads(data.path()).is_empty());
    }
    // A redirect without a Location.
    let f = Fake::default();
    f.serve(URL, 302, None, b"");
    assert_eq!(
        get(&f, data.path(), &c).await.err(),
        Some(FetchError::Redirect)
    );
    // Any status but 200 or a redirect.
    let f = Fake::default();
    assert_eq!(
        get(&f, data.path(), &c).await.err(),
        Some(FetchError::Status(404))
    );
}

#[tokio::test]
async fn five_redirects_are_followed_and_six_are_refused() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let chain = |hops: usize| {
        let f = Fake::default();
        for i in 0..hops {
            let from = if i == 0 {
                URL.to_string()
            } else {
                format!("https://github.com/hop/{i}")
            };
            f.serve(
                &from,
                302,
                Some(&format!("https://github.com/hop/{}", i + 1)),
                b"",
            );
        }
        f.serve(&format!("https://github.com/hop/{hops}"), 200, None, &b);
        f
    };
    let f = chain(fetch::MAX_HOPS);
    get(&f, data.path(), &c).await.unwrap().discard().unwrap();
    let f = chain(fetch::MAX_HOPS + 1);
    assert_eq!(
        get(&f, data.path(), &c).await.err(),
        Some(FetchError::Redirect)
    );
    assert_eq!(f.asked(), fetch::MAX_HOPS + 1, "the last hop isn't asked");
}

#[tokio::test]
async fn a_stalled_body_hits_the_idle_limit() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let c = catalogue(&binary());
    let src = Drip {
        chunks: 2,
        every: Duration::from_millis(1),
    };
    let limits = Limits {
        overall: Duration::from_secs(600),
        idle: Duration::from_millis(100),
    };
    let started = std::time::Instant::now();
    let got =
        fetch::fetch_limited(&src, data.path(), entry(&c), pin(&c), &mut |_| {}, limits).await;
    assert_eq!(got.err(), Some(FetchError::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(300),
        "idle, not overall"
    );
    assert!(downloads(data.path()).is_empty());
}

/// A hop whose response headers never come hits the idle limit, before
/// any `.part` file exists.
#[tokio::test]
async fn a_hop_that_never_answers_hits_the_idle_limit() {
    struct Silent;
    impl Source for Silent {
        fn open<'a>(&'a self, _: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
            Box::pin(std::future::pending())
        }
    }
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let c = catalogue(&binary());
    let limits = Limits {
        overall: Duration::from_secs(600),
        idle: Duration::from_millis(100),
    };
    let started = std::time::Instant::now();
    let got = fetch::fetch_limited(
        &Silent,
        data.path(),
        entry(&c),
        pin(&c),
        &mut |_| {},
        limits,
    )
    .await;
    assert_eq!(got.err(), Some(FetchError::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(300),
        "idle, not overall"
    );
    assert!(downloads(data.path()).is_empty());
}

#[tokio::test]
async fn a_slow_drip_hits_the_overall_limit() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let c = catalogue(&binary());
    // Every chunk well inside the idle limit, the whole far past overall.
    let src = Drip {
        chunks: 1000,
        every: Duration::from_millis(20),
    };
    let limits = Limits {
        overall: Duration::from_millis(200),
        idle: Duration::from_secs(60),
    };
    let mut last = 0;
    let got = fetch::fetch_limited(
        &src,
        data.path(),
        entry(&c),
        pin(&c),
        &mut |n| last = n,
        limits,
    )
    .await;
    assert_eq!(got.err(), Some(FetchError::Timeout));
    assert!(last < 4500, "stopped before the body ended");
    assert!(downloads(data.path()).is_empty());
}

#[tokio::test]
async fn stale_files_of_this_companion_go_and_others_stay() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let dir = fetch::folder(data.path());
    std::fs::create_dir_all(&dir).unwrap();
    for name in [
        "qbittorrent_5.0.2.part",
        "qbittorrent_5.0.3.part",
        "qbittorrent_5.0.1.verified",
        "prowlarr_1.28.2.part",
        "qbittorrent-x_1.0.part",
        "qbittorrent_notes.txt",
    ] {
        std::fs::write(dir.join(name), b"old").unwrap();
    }
    let f = Fake::default();
    f.serve(URL, 200, None, &b);
    let v = get(&f, data.path(), &c).await.unwrap();
    assert_eq!(
        downloads(data.path()),
        [
            "prowlarr_1.28.2.part",
            "qbittorrent-x_1.0.part",
            "qbittorrent_5.0.3.verified",
            "qbittorrent_notes.txt",
        ]
    );
    drop(v);
}

/// A link planted where the `.part` file goes is removed, never written
/// through, and the download lands in a new file of its own.
#[cfg(unix)]
#[tokio::test]
async fn a_planted_link_is_never_followed() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let dir = fetch::folder(data.path());
    std::fs::create_dir_all(&dir).unwrap();
    let target = data.path().join("elsewhere");
    std::fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, dir.join("qbittorrent_5.0.3.part")).unwrap();
    std::os::unix::fs::symlink(&target, dir.join("qbittorrent_5.0.3.verified")).unwrap();
    let f = Fake::default();
    f.serve(URL, 200, None, &b);
    let v = get(&f, data.path(), &c).await.unwrap();
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"keep",
        "not written through"
    );
    let meta = std::fs::symlink_metadata(v.path()).unwrap();
    assert!(meta.file_type().is_file());
    assert_eq!(std::fs::read(v.path()).unwrap(), b);
}

/// A download folder that's a link is refused.
#[cfg(unix)]
#[tokio::test]
async fn a_linked_download_folder_is_refused() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let elsewhere = data.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir_all(data.path().join("companions")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, fetch::folder(data.path())).unwrap();
    let f = Fake::default();
    f.serve(URL, 200, None, &b);
    assert_eq!(get(&f, data.path(), &c).await.err(), Some(FetchError::Io));
    assert_eq!(f.asked(), 0);
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// A `companions` folder that's a link is refused before anything is
/// created through it.
#[cfg(unix)]
#[tokio::test]
async fn a_linked_companions_folder_is_refused() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let elsewhere = data.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, data.path().join("companions")).unwrap();
    let f = Fake::default();
    f.serve(URL, 200, None, &b);
    assert_eq!(get(&f, data.path(), &c).await.err(), Some(FetchError::Io));
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// The slot stays taken while the verified file is still open, so a
/// second download can't remove it as stale from under the extractor.
#[tokio::test]
async fn the_slot_is_held_until_the_verified_file_is_gone() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let f = Fake::default();
    f.serve(URL, 200, None, &b);
    let v = get(&f, data.path(), &c).await.unwrap();
    assert_eq!(get(&f, data.path(), &c).await.err(), Some(FetchError::Busy));
    assert!(v.path().exists());
    v.discard().unwrap();
    get(&f, data.path(), &c).await.unwrap();
}

/// A second download of the same companion while one runs is refused.
#[tokio::test]
async fn one_download_per_companion_at_a_time() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let stalled = Drip {
        chunks: 0,
        every: Duration::ZERO,
    };
    let limits = Limits {
        overall: Duration::from_secs(600),
        idle: Duration::from_millis(200),
    };
    let f = Fake::default();
    f.serve(URL, 200, None, &b);
    // The first takes the slot on its first poll, before the second runs.
    let mut ignore = |_| {};
    let first = fetch::fetch_limited(
        &stalled,
        data.path(),
        entry(&c),
        pin(&c),
        &mut ignore,
        limits,
    );
    let (first, second) = tokio::join!(first, get(&f, data.path(), &c));
    assert_eq!(first.err(), Some(FetchError::Timeout));
    assert_eq!(second.err(), Some(FetchError::Busy));
    assert_eq!(f.asked(), 0);
    // And free again once the first ended.
    get(&f, data.path(), &c).await.unwrap();
}

#[test]
fn only_a_public_only_client_is_accepted() {
    let public = GuardedClient::new(NetPolicy::notification(false), fetch::OVERALL).unwrap();
    assert!(Guarded::new(public).is_ok());
    for policy in [NetPolicy::notification(true), NetPolicy::download_client()] {
        let c = GuardedClient::new(policy, fetch::OVERALL).unwrap();
        assert_eq!(Guarded::new(c).err(), Some(FetchError::Route));
    }
    let route = lodestar_integrations::net::Route::Direct;
    assert!(Guarded::new(fetch::client(&route).unwrap()).is_ok());
}

// ─── Over real HTTP ─────────────────────────────────────────────────────

/// A loopback route: status, extra header lines, body.
type Served = (u16, String, Vec<u8>);

/// A real HTTP/1.1 server on loopback: path → (status, extra headers,
/// body), one response per connection; every request's head recorded.
struct Loopback {
    base: Url,
    routes: Arc<Mutex<HashMap<String, Served>>>,
    heads: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Loopback {
    async fn start() -> Loopback {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let routes: Arc<Mutex<HashMap<String, Served>>> = Arc::default();
        let heads: Arc<Mutex<Vec<String>>> = Arc::default();
        let (r, h) = (routes.clone(), heads.clone());
        let task = tokio::spawn(async move {
            while let Ok((mut conn, _)) = listener.accept().await {
                let (r, h) = (r.clone(), h.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match conn.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let path = head.split(' ').nth(1).unwrap_or("").to_string();
                    h.lock().unwrap().push(head);
                    let route = r.lock().unwrap().get(&path).cloned();
                    let (status, extra, body) = route.unwrap_or((404, String::new(), vec![]));
                    let out = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                        body.len()
                    );
                    let _ = conn.write_all(out.as_bytes()).await;
                    let _ = conn.write_all(&body).await;
                    let _ = conn.shutdown().await;
                });
            }
        });
        Loopback {
            base,
            routes,
            heads,
            task,
        }
    }

    fn serve(&self, path: &str, status: u16, extra: &str, body: &[u8]) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_string(), (status, extra.to_string(), body.to_vec()));
    }
}

/// Sends every hop to the loopback server, path and query kept, through
/// a real `GuardedClient` (loopback allowed, for the test only).
struct Forward {
    base: Url,
    guarded: Guarded,
}

impl Forward {
    fn new(base: &Url) -> Forward {
        let client =
            GuardedClient::new(NetPolicy::download_client(), Duration::from_secs(30)).unwrap();
        Forward {
            base: base.clone(),
            guarded: Guarded::unchecked(client),
        }
    }
}

impl Source for Forward {
    fn open<'a>(&'a self, url: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
        let mut to = self.base.clone();
        to.set_path(url.path());
        to.set_query(url.query());
        Box::pin(async move { self.guarded.open(&to).await })
    }
}

/// What each hop sends: a bare `GET` with the companions' agent; no
/// cookie (even one the server set), no referer, nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_requests_carry_nothing_identifying() {
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let server = Loopback::start().await;
    let first = Url::parse(URL).unwrap();
    server.serve(
        first.path(),
        302,
        &format!("Location: {ASSET}\r\nSet-Cookie: id=tracked; Path=/\r\n"),
        b"",
    );
    server.serve("/x/qbittorrent-nox?sig=Zq7Secret", 200, "", &b);
    let src = Forward::new(&server.base);
    let v = get(&src, data.path(), &c).await.unwrap();
    assert_eq!(std::fs::read(v.path()).unwrap(), b);
    let heads = server.heads.lock().unwrap().clone();
    assert_eq!(heads.len(), 2);
    for head in &heads {
        let mut lines = head.split("\r\n").filter(|l| !l.is_empty());
        assert!(lines.next().unwrap().starts_with("GET /"), "{head}");
        let mut names = vec![];
        for line in lines {
            let (name, value) = line.split_once(':').unwrap();
            let name = name.to_ascii_lowercase();
            if name == "user-agent" {
                assert_eq!(value.trim(), USER_AGENT);
            }
            names.push(name);
        }
        names.sort();
        assert_eq!(names, ["accept", "host", "user-agent"], "{head}");
    }
}

// ─── Upstream minisign ──────────────────────────────────────────────────

async fn verified(data: &Path, body: &[u8], c: &Catalogue) -> Verified {
    let f = Fake::default();
    f.serve(URL, 200, None, body);
    get(&f, data, c).await.unwrap()
}

#[tokio::test]
async fn a_minisign_signature_verifies_with_the_pinned_key_only() {
    use std::io::Read;
    let _turn = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap();
    let b = binary();
    let c = catalogue(&b);
    let mut v = verified(data.path(), &b, &c).await;
    let key = TestKey::new(1);

    assert_eq!(
        verify::minisign(&mut v, &key.public(), &key.sign(&b)),
        Ok(())
    );
    let mut again = vec![];
    v.read_to_end(&mut again).unwrap();
    assert_eq!(again, b, "left at the start");

    let mut other = b.clone();
    other[0] ^= 1;
    assert_eq!(
        verify::minisign(&mut v, &key.public(), &key.sign(&other)),
        Err(VerifyError::BadSignature)
    );
    assert_eq!(
        verify::minisign(&mut v, &TestKey::new(2).public(), &key.sign(&b)),
        Err(VerifyError::WrongKey)
    );
    assert_eq!(
        verify::minisign(&mut v, &key.public(), &key.sign_legacy(&b)),
        Err(VerifyError::SignatureFile)
    );
    // A trusted comment changed after signing: the global signature fails.
    let good = key.sign(&b);
    let tampered: String = good
        .lines()
        .map(|l| {
            if l.starts_with("trusted comment: ") {
                "trusted comment: timestamp:0\tfile:other".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(tampered.trim_end(), good.trim_end());
    assert_eq!(
        verify::minisign(&mut v, &key.public(), &tampered),
        Err(VerifyError::BadSignature)
    );
    assert_eq!(
        verify::minisign(&mut v, &key.public(), "not a signature"),
        Err(VerifyError::SignatureFile)
    );
}

#[tokio::test]
async fn a_signature_comes_through_the_same_hops_within_its_cap() {
    let c = catalogue(&binary());
    let sig_url = format!("{URL}.minisig");
    let key = TestKey::new(1);
    let sig = key.sign(b"x");
    let f = Fake::default();
    f.serve(&sig_url, 302, Some("https://evil.example/s.minisig"), b"");
    assert_eq!(
        fetch::fetch_signature_from(&f, entry(&c), &sig_url).await,
        Err(FetchError::Redirect)
    );
    f.serve(&sig_url, 200, None, sig.as_bytes());
    assert_eq!(
        fetch::fetch_signature_from(&f, entry(&c), &sig_url).await,
        Ok(sig)
    );
    f.serve(&sig_url, 200, None, &vec![b'a'; MAX_SIGNATURE as usize + 1]);
    assert_eq!(
        fetch::fetch_signature_from(&f, entry(&c), &sig_url).await,
        Err(FetchError::TooLarge)
    );
    f.serve(&sig_url, 200, None, &[0xff, 0xfe]);
    assert_eq!(
        fetch::fetch_signature_from(&f, entry(&c), &sig_url).await,
        Err(FetchError::SignatureText)
    );
}

#[test]
fn the_pin_check_compares_size_and_hash() {
    let b = binary();
    let c = catalogue(&b);
    let p = pin(&c);
    let hash: [u8; 32] = sha2::Sha256::digest(&b).into();
    assert_eq!(verify::matches(p, b.len() as u64, &hash), Ok(()));
    assert_eq!(verify::upstream(p), verify::Upstream::PinOnly);
    assert_eq!(
        verify::matches(p, b.len() as u64 + 1, &hash),
        Err(VerifyError::Mismatch)
    );
    let mut wrong = hash;
    wrong[31] ^= 1;
    assert_eq!(
        verify::matches(p, b.len() as u64, &wrong),
        Err(VerifyError::Mismatch)
    );
}
