//! Downloading a pinned artifact (§12.2 "Downloading").
//!
//! - **Where from:** the pin's `url` (`https`, a download host, checked in
//!   the signed manifest), then each redirect through the same allowlist
//!   as the check ([`check::hop_allowed`]), at most [`check::MAX_HOPS`].
//!   A redirect's own URL (GitHub's signed asset URLs carry a query) is
//!   never logged.
//! - **Where to:** `<data>/bin/<version>.part`, created new (`O_NOFOLLOW`,
//!   0600). Bytes are counted as they arrive (one past the pinned size
//!   stops it) and hashed as they go; then fsync, and the SHA-256 and size
//!   must be the pin's. Anything else removes the file. Stale `.part`
//!   files go at the start of the next download.
//! - **One at a time**, and no resume: the engine holds its one-check lock
//!   for the whole install.
//! - **What it sends:** the check's request: `User-Agent:
//!   Lodestar-Updater` and nothing that identifies the installation.

use std::fs::File;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin as BoxPin;
use std::sync::Arc;
use std::time::Duration;

use lodestar_integrations::net::{GuardedClient, NetPolicy};
use lodestar_update::{Pin, Sha256};
use reqwest::Url;
use sha2::Digest;

use super::check::{self, CheckError, MAX_HOPS, hop_allowed};
use super::layout::Bin;

type BoxFuture<'a, T> = BoxPin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The whole download, every hop included: 256 MiB at about 1 MB/s.
pub const OVERALL: Duration = Duration::from_secs(30 * 60);
/// The longest wait for the body's next chunk: a stalled server fails
/// the download (`timeout`) rather than hold the install for [`OVERALL`].
pub const IDLE: Duration = Duration::from_secs(60);
/// Chunks in flight to the writer.
const QUEUE: usize = 8;

/// How long a download may take, in all and between chunks.
#[derive(Debug, Clone, Copy)]
struct Limits {
    overall: Duration,
    idle: Duration,
}

const LIMITS: Limits = Limits {
    overall: OVERALL,
    idle: IDLE,
};

/// Where the writer thread is, for tests that hold it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterAt {
    /// About to create the `.part` file.
    Start,
    /// Finished: the file is closed (or was never created).
    Done,
}

type WriterHook = Arc<dyn Fn(WriterAt) + Send + Sync>;

/// One response: its status and `Location`, and its body chunk by chunk.
pub struct Opened {
    pub status: u16,
    pub location: Option<String>,
    pub body: Box<dyn Chunks>,
}

/// A body, read a chunk at a time; `None` at its end.
pub trait Chunks: Send {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, CheckError>>;
}

/// Where downloads come from: the real client, or tests' in-memory one.
pub trait Source: Send + Sync {
    fn open<'a>(&'a self, url: &'a Url) -> BoxFuture<'a, Result<Opened, CheckError>>;
}

/// Why a download failed: stable codes, never a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadError {
    /// The fetch failed (`redirect_refused`, `network`, `timeout`, …).
    Fetch(CheckError),
    /// More bytes than the pinned size.
    TooLarge,
    /// The SHA-256 or the size isn't the pin's.
    Mismatch,
    /// `<data>/bin` couldn't be written.
    Io,
}

impl DownloadError {
    pub fn code(self) -> &'static str {
        match self {
            DownloadError::Fetch(e) => e.code(),
            DownloadError::TooLarge => "download_too_large",
            DownloadError::Mismatch => "download_mismatch",
            DownloadError::Io => "download_io",
        }
    }
}

impl From<CheckError> for DownloadError {
    fn from(e: CheckError) -> DownloadError {
        DownloadError::Fetch(e)
    }
}

/// `<data>/bin/<version>.part`.
pub fn part_path(bin: &Bin, pin: &Pin) -> PathBuf {
    bin.path(&format!("{}.part", pin.version))
}

/// Removes every `*.part` in `<data>/bin` (a download that stopped).
pub fn remove_stale_parts(bin: &Bin) {
    let Ok(entries) = std::fs::read_dir(bin.dir()) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let is_part = name.to_str().is_some_and(|n| n.ends_with(".part"));
        if is_part && e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Downloads `pin` to its `.part` file within [`OVERALL`]; `progress` gets
/// the bytes so far after each chunk. The file is the pin's when this
/// returns `Ok`.
pub async fn download(
    src: &dyn Source,
    bin: &Bin,
    pin: &Pin,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<PathBuf, DownloadError> {
    download_with(src, bin, pin, progress, LIMITS, None).await
}

/// [`download`] within `limits`. A failure removes the `.part` file only
/// once the writer thread has finished with it, the overall timeout
/// included, so no write (or the file's creation) lands after the
/// removal. The join itself isn't bounded: a disk write that hangs keeps
/// the download past `limits.overall` rather than remove the file under
/// the writer.
async fn download_with(
    src: &dyn Source,
    bin: &Bin,
    pin: &Pin,
    progress: &mut (dyn FnMut(u64) + Send),
    limits: Limits,
    hook: Option<WriterHook>,
) -> Result<PathBuf, DownloadError> {
    remove_stale_parts(bin);
    let path = part_path(bin, pin);
    let deadline = tokio::time::Instant::now() + limits.overall;
    let result = fetch_to(src, &path, pin, progress, limits.idle, deadline, hook).await;
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result.map(|()| path)
}

/// The response for `pin`, every hop through the allowlist.
async fn open_pinned(src: &dyn Source, pin: &Pin) -> Result<Opened, DownloadError> {
    let mut at = Url::parse(&pin.url).map_err(|_| CheckError::Redirect)?;
    for _ in 0..=MAX_HOPS {
        if !hop_allowed(&at) {
            return Err(CheckError::Redirect.into());
        }
        let hop = src.open(&at).await?;
        match hop.status {
            200 => return Ok(hop),
            301 | 302 | 303 | 307 | 308 => {
                at = hop
                    .location
                    .as_deref()
                    .and_then(|l| at.join(l).ok())
                    .ok_or(CheckError::Redirect)?;
            }
            s => return Err(CheckError::Status(s).into()),
        }
    }
    Err(CheckError::Redirect.into())
}

async fn fetch_to(
    src: &dyn Source,
    path: &Path,
    pin: &Pin,
    progress: &mut (dyn FnMut(u64) + Send),
    idle: Duration,
    deadline: tokio::time::Instant,
    hook: Option<WriterHook>,
) -> Result<(), DownloadError> {
    let timeout = || DownloadError::Fetch(CheckError::Timeout);
    let mut opened = tokio::time::timeout_at(deadline, open_pinned(src, pin))
        .await
        .map_err(|_| timeout())??;
    // The file's writes, its hash and its fsync run off the async
    // workers; this side counts the bytes (the size cap) and reports.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(QUEUE);
    let target = path.to_path_buf();
    let writer = tokio::task::spawn_blocking(move || -> io::Result<Sha256> {
        let at = |w| {
            if let Some(h) = &hook {
                h(w);
            }
        };
        at(WriterAt::Start);
        let written = (|| {
            let mut file = create_part(&target)?;
            let mut hash = sha2::Sha256::new();
            while let Some(chunk) = rx.blocking_recv() {
                hash.update(&chunk);
                file.write_all(&chunk)?;
            }
            file.sync_all()?;
            Ok(Sha256(hash.finalize().into()))
        })();
        at(WriterAt::Done);
        written
    });
    let mut len = 0u64;
    // The overall deadline ends the streaming only: the writer is joined
    // below whatever happens, before the caller removes the file.
    let streamed = tokio::time::timeout_at(deadline, async {
        loop {
            let next = tokio::time::timeout(idle, opened.body.next())
                .await
                .map_err(|_| CheckError::Timeout)??;
            let Some(chunk) = next else {
                return Ok(());
            };
            len += chunk.len() as u64;
            if len > pin.size {
                return Err(DownloadError::TooLarge);
            }
            // A writer that stopped has its error below.
            if tx.send(chunk).await.is_err() {
                return Ok(());
            }
            progress(len);
        }
    })
    .await
    .unwrap_or_else(|_| Err(timeout()));
    // The writer ends (and closes the file) before anything is decided,
    // so a failure's file can be removed.
    drop(tx);
    let written = writer.await.map_err(|_| DownloadError::Io)?;
    streamed?;
    let hash = written.map_err(|_| DownloadError::Io)?;
    if len != pin.size || hash != pin.sha256 {
        return Err(DownloadError::Mismatch);
    }
    Ok(())
}

/// `path`, created new: never through a link, owner-only.
fn create_part(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(super::layout::nofollow())
        .open(path)
}

/// The real source: the check's client (public addresses only), with no
/// whole-response timeout of its own; [`OVERALL`] bounds the download.
pub struct Guarded {
    client: GuardedClient,
}

impl Guarded {
    pub fn new() -> Result<Guarded, CheckError> {
        let client = GuardedClient::new(NetPolicy::notification(false), OVERALL)
            .map_err(CheckError::from)?;
        Ok(Guarded { client })
    }
}

struct Body(reqwest::Response);

impl Chunks for Body {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, CheckError>> {
        Box::pin(async move {
            match self.0.chunk().await {
                Ok(c) => Ok(c.map(|b| b.to_vec())),
                Err(e) if e.is_timeout() => Err(CheckError::Timeout),
                Err(_) => Err(CheckError::Network),
            }
        })
    }
}

impl Source for Guarded {
    fn open<'a>(&'a self, url: &'a Url) -> BoxFuture<'a, Result<Opened, CheckError>> {
        Box::pin(async move {
            let resp = self.client.send(check::request(url)).await?;
            let status = resp.status().as_u16();
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            Ok(Opened {
                status,
                location,
                body: Box::new(Body(resp)),
            })
        })
    }
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use lodestar_update::semver::Version;

    use super::*;

    /// A served URL: its status, `Location` and body.
    type Route = (u16, Option<String>, Vec<u8>);

    /// An in-memory download source: URL → (status, Location, body), the
    /// body served in chunks of [`Fake::CHUNK`]; every request recorded.
    #[derive(Default)]
    pub struct Fake {
        pub routes: Mutex<HashMap<String, Route>>,
        pub asked: Mutex<Vec<String>>,
        /// When set, every request goes over real HTTP to this loopback
        /// server instead (its path and query kept): see [`Loopback`].
        pub forward: Mutex<Option<Url>>,
    }

    impl Fake {
        pub const CHUNK: usize = 1000;

        pub fn serve(&self, url: &str, status: u16, location: Option<&str>, body: &[u8]) {
            self.routes.lock().unwrap().insert(
                url.to_string(),
                (status, location.map(str::to_owned), body.to_vec()),
            );
        }
    }

    struct Memory(Vec<Vec<u8>>);

    impl Chunks for Memory {
        fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, CheckError>> {
            let c = (!self.0.is_empty()).then(|| self.0.remove(0));
            Box::pin(async move { Ok(c) })
        }
    }

    impl Source for Fake {
        fn open<'a>(&'a self, url: &'a Url) -> BoxFuture<'a, Result<Opened, CheckError>> {
            self.asked.lock().unwrap().push(url.to_string());
            if let Some(base) = self.forward.lock().unwrap().clone() {
                // The allowlist already passed `url`; only where it's
                // fetched from changes, as a DNS override would.
                let mut to = base;
                to.set_path(url.path());
                to.set_query(url.query());
                return Box::pin(async move {
                    let client =
                        GuardedClient::new(NetPolicy::download_client(), Duration::from_secs(30))
                            .map_err(CheckError::from)?;
                    Guarded { client }.open(&to).await
                });
            }
            let route = self.routes.lock().unwrap().get(url.as_str()).cloned();
            Box::pin(async move {
                let (status, location, body) = route.unwrap_or((404, None, vec![]));
                Ok(Opened {
                    status,
                    location,
                    body: Box::new(Memory(
                        body.chunks(Fake::CHUNK).map(<[u8]>::to_vec).collect(),
                    )),
                })
            })
        }
    }

    /// A real HTTP/1.1 server on loopback (never the network): path →
    /// (status, `Location`, body), one response per connection; every
    /// request's head is recorded.
    pub struct Loopback {
        pub base: Url,
        routes: Arc<Mutex<HashMap<String, Route>>>,
        pub heads: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Loopback {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl Loopback {
        pub async fn start() -> Loopback {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
            let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
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
                        let (status, location, body) = route.unwrap_or((404, None, vec![]));
                        let mut out = format!(
                            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                            body.len()
                        );
                        if let Some(l) = location {
                            out.push_str(&format!("Location: {l}\r\n"));
                        }
                        out.push_str("\r\n");
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

        /// Serves `path` (with its query, if any).
        pub fn serve(&self, path: &str, status: u16, location: Option<&str>, body: &[u8]) {
            self.routes.lock().unwrap().insert(
                path.to_string(),
                (status, location.map(str::to_owned), body.to_vec()),
            );
        }
    }

    const URL: &str = "https://github.com/dvortsis/lodestar-v2/releases/download/v0.6.0/lodestar";
    const ASSET: &str = "https://release-assets.githubusercontent.com/x/lodestar?sig=Zq7Secret";

    fn pin(binary: &[u8]) -> Pin {
        Pin {
            version: Version::new(0, 6, 0),
            url: URL.into(),
            sha256: Sha256::of(binary),
            size: binary.len() as u64,
        }
    }

    fn binary() -> Vec<u8> {
        (0..4500u32).map(|i| (i % 251) as u8).collect()
    }

    #[tokio::test]
    async fn the_pinned_bytes_land_in_a_private_part_file() {
        use std::os::unix::fs::PermissionsExt;

        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let f = Fake::default();
        let b = binary();
        f.serve(URL, 302, Some(ASSET), b"");
        f.serve(ASSET, 200, None, &b);
        let mut seen = vec![];
        let path = download(&f, &bin, &pin(&b), &mut |n| seen.push(n))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(seen, [1000, 2000, 3000, 4000, 4500], "progress per chunk");
    }

    #[tokio::test]
    async fn a_wrong_hash_size_or_oversized_body_leaves_nothing() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let b = binary();
        let f = Fake::default();
        // Same size, another body.
        let mut other = b.clone();
        other[10] ^= 1;
        f.serve(URL, 200, None, &other);
        let p = pin(&b);
        assert_eq!(
            download(&f, &bin, &p, &mut |_| {}).await,
            Err(DownloadError::Mismatch)
        );
        assert!(!part_path(&bin, &p).exists());
        // Shorter than pinned.
        f.serve(URL, 200, None, &b[..4000]);
        assert_eq!(
            download(&f, &bin, &p, &mut |_| {}).await,
            Err(DownloadError::Mismatch)
        );
        // Longer: stopped one chunk past the pin, not read to the end.
        let mut long = b.clone();
        long.extend(std::iter::repeat_n(0u8, 100_000));
        f.serve(URL, 200, None, &long);
        assert_eq!(
            download(&f, &bin, &p, &mut |_| {}).await,
            Err(DownloadError::TooLarge)
        );
        assert!(!part_path(&bin, &p).exists());
    }

    #[tokio::test]
    async fn every_hop_passes_the_allowlist() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let b = binary();
        let p = pin(&b);
        for to in [
            "https://evil.example/lodestar",
            "http://github.com/lodestar",
            "https://github.com:8443/lodestar",
            "https://user:pw@github.com/lodestar",
        ] {
            let f = Fake::default();
            f.serve(URL, 302, Some(to), b"");
            f.serve(to, 200, None, &b);
            assert_eq!(
                download(&f, &bin, &p, &mut |_| {}).await,
                Err(DownloadError::Fetch(CheckError::Redirect)),
                "{to}"
            );
            assert_eq!(f.asked.lock().unwrap().len(), 1, "{to} isn't asked");
        }
        // Six redirects are one too many; a 404 is an error here.
        let f = Fake::default();
        for i in 0..6 {
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
        f.serve("https://github.com/hop/6", 200, None, &b);
        assert_eq!(
            download(&f, &bin, &p, &mut |_| {}).await,
            Err(DownloadError::Fetch(CheckError::Redirect))
        );
        let f = Fake::default();
        assert_eq!(
            download(&f, &bin, &p, &mut |_| {}).await,
            Err(DownloadError::Fetch(CheckError::Status(404)))
        );
    }

    /// A body that stops sending fails at the idle limit, not the
    /// overall one, and leaves no file.
    #[tokio::test]
    async fn a_stalled_body_times_out_and_leaves_nothing() {
        struct Stalled;
        impl Chunks for Stalled {
            fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, CheckError>> {
                Box::pin(std::future::pending())
            }
        }
        struct Source1;
        impl Source for Source1 {
            fn open<'a>(&'a self, _: &'a Url) -> BoxFuture<'a, Result<Opened, CheckError>> {
                Box::pin(async {
                    Ok(Opened {
                        status: 200,
                        location: None,
                        body: Box::new(Stalled),
                    })
                })
            }
        }
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let p = pin(&binary());
        let limits = Limits {
            overall: OVERALL,
            idle: Duration::from_millis(50),
        };
        let got = download_with(&Source1, &bin, &p, &mut |_| {}, limits, None).await;
        assert_eq!(got, Err(DownloadError::Fetch(CheckError::Timeout)));
        assert!(!part_path(&bin, &p).exists());
    }

    /// When the overall timeout fires, the writer is joined before the
    /// `.part` file is removed: it's held at its start past the deadline
    /// here, and has finished (and its file is gone) by the time the
    /// download returns, so nothing it does can land after the removal.
    #[tokio::test]
    async fn the_overall_timeout_joins_the_writer_before_removing_the_file() {
        struct OneThenStall(bool);
        impl Chunks for OneThenStall {
            fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, CheckError>> {
                if std::mem::replace(&mut self.0, true) {
                    return Box::pin(std::future::pending());
                }
                Box::pin(async { Ok(Some(vec![7u8; 100])) })
            }
        }
        struct Source1;
        impl Source for Source1 {
            fn open<'a>(&'a self, _: &'a Url) -> BoxFuture<'a, Result<Opened, CheckError>> {
                Box::pin(async {
                    Ok(Opened {
                        status: 200,
                        location: None,
                        body: Box::new(OneThenStall(false)),
                    })
                })
            }
        }
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let p = pin(&binary());
        let overall = Duration::from_millis(100);
        // Paces the writer (never assumes work finished): held at its
        // start until well past the deadline.
        let held_until = std::time::Instant::now() + overall * 10;
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = done.clone();
        let hook: WriterHook = Arc::new(move |w| match w {
            WriterAt::Start => {
                while std::time::Instant::now() < held_until {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            WriterAt::Done => seen.store(true, std::sync::atomic::Ordering::SeqCst),
        });
        let limits = Limits {
            overall,
            idle: OVERALL,
        };
        let got = download_with(&Source1, &bin, &p, &mut |_| {}, limits, Some(hook)).await;
        assert_eq!(got, Err(DownloadError::Fetch(CheckError::Timeout)));
        assert!(
            done.load(std::sync::atomic::Ordering::SeqCst),
            "the writer was joined before the download returned"
        );
        assert!(!part_path(&bin, &p).exists());
        assert!(
            std::fs::read_dir(bin.dir())
                .unwrap()
                .all(|e| { !e.unwrap().file_name().to_string_lossy().ends_with(".part") })
        );
    }

    /// A `.part` left by an earlier download goes; one planted as a link
    /// is never written through.
    #[tokio::test]
    async fn stale_parts_go_and_a_link_is_never_followed() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let b = binary();
        let p = pin(&b);
        std::fs::write(bin.path("0.5.9.part"), b"old").unwrap();
        let f = Fake::default();
        f.serve(URL, 200, None, &b);
        download(&f, &bin, &p, &mut |_| {}).await.unwrap();
        assert!(!bin.path("0.5.9.part").exists());

        std::fs::remove_file(part_path(&bin, &p)).unwrap();
        let target = data.path().join("elsewhere");
        std::fs::write(&target, b"keep").unwrap();
        // A link in place of the .part file: removed as stale (a link is
        // not a file to `remove_stale_parts`, so it's refused instead).
        std::os::unix::fs::symlink(&target, part_path(&bin, &p)).unwrap();
        let got = download(&f, &bin, &p, &mut |_| {}).await;
        assert_eq!(got, Err(DownloadError::Io));
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"keep",
            "not written through"
        );
    }
}
