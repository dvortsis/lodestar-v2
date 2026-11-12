//! Downloading a pinned companion artifact (plan 2026-11-06-v06-companions
//! §4 CMP-1, Q16; architecture §12.2 "Downloading", §18.10). The rules are
//! the updater's (`crates/lodestar/src/update/download.rs`):
//!
//! - **Where from:** the pin's `url`, then each redirect, every hop
//!   `https`, on the default port, with no user information, and on both
//!   the entry's host allowlist and [`DOWNLOAD_HOSTS`] ([`hop_allowed`]),
//!   at most [`MAX_HOPS`] redirects. A redirect's URL (GitHub's signed asset
//!   URLs carry a query) is never logged or kept in an error.
//! - **Route:** the caller's [`GuardedClient`], built with Lodestar's own
//!   egress route ([`client`]); [`Guarded::new`] refuses a client whose
//!   policy reaches anything but public addresses. The SSRF guard checks
//!   every hop, since each one is a separate `send`.
//! - **What it sends:** `GET`, `User-Agent: Lodestar-Companions`, no
//!   cookies, no referer, no query of its own; the HTTP client adds only
//!   `accept: */*` and `host`.
//! - **Where to:** `<data>/companions/.downloads/<id>_<version>.part`,
//!   created new (no-follow and 0600 on Unix). A companion id never starts
//!   with `.`, so the folder can't be a companion's own; `_` is in neither
//!   an id nor a version, so one companion's files can't be taken for
//!   another's. That companion's earlier `.part` and `.verified` files go
//!   at the start of each download. One download per companion at a time.
//! - **Limits:** bytes are counted as they arrive, and one past the pin's
//!   size (itself at most [`MAX_ARTIFACT_SIZE`]) stops the download; they're
//!   hashed as they stream, within [`OVERALL`] and [`IDLE`].
//! - **On completion:** fsync, then the size and SHA-256 must be the pin's
//!   ([`crate::verify::matches`]). Anything else removes the file. A match
//!   is renamed to `.verified` and returned as an open [`Verified`] handle
//!   for CMP-2's extractor.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin as BoxPin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lodestar_integrations::net::{GuardedClient, NetError, NetPolicy, Route, SystemResolver};
use reqwest::Url;
use sha2::Digest;

use crate::catalogue::{DOWNLOAD_HOSTS, Entry, MAX_ARTIFACT_SIZE, Pin, Sha256};
use crate::verify;

type BoxFuture<'a, T> = BoxPin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The one user agent companion downloads send.
pub const USER_AGENT: &str = "Lodestar-Companions";
/// Redirects followed, at most: the updater's number.
pub const MAX_HOPS: usize = 5;
/// The whole download, every hop included: [`MAX_ARTIFACT_SIZE`] at about
/// 150 KB/s.
pub const OVERALL: Duration = Duration::from_secs(60 * 60);
/// The longest wait for the body's next chunk.
pub const IDLE: Duration = Duration::from_secs(60);
/// The largest upstream signature file fetched.
pub const MAX_SIGNATURE: u64 = lodestar_update::minisign::MAX_SIGNATURE_FILE as u64;
/// The folder under `<data>/companions/` downloads go to.
pub const FOLDER: &str = ".downloads";
/// Chunks in flight to the writer.
const QUEUE: usize = 8;

/// How long a download may take, in all and between chunks.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub overall: Duration,
    pub idle: Duration,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        overall: OVERALL,
        idle: IDLE,
    };
}

/// Why a download failed: stable codes, never a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    /// Another download of this companion is running.
    Busy,
    /// The client handed in may reach more than public addresses.
    Route,
    /// A hop to `http`, to a host off the allowlist, without a usable
    /// `Location`, or past [`MAX_HOPS`].
    Redirect,
    /// Any status but 200 or a redirect.
    Status(u16),
    Timeout,
    /// The address is one the policy refuses.
    Blocked,
    /// Couldn't connect, resolve, or speak HTTP.
    Network,
    /// More bytes than the pin's size.
    TooLarge,
    /// The SHA-256 or the size isn't the pin's.
    Mismatch,
    /// An upstream signature file that isn't UTF-8 text.
    SignatureText,
    /// The download folder couldn't be written, or isn't a plain folder.
    Io,
}

impl FetchError {
    pub fn code(self) -> &'static str {
        match self {
            FetchError::Busy => "download_busy",
            FetchError::Route => "download_route",
            FetchError::Redirect => "redirect_refused",
            FetchError::Status(_) => "http_status",
            FetchError::Timeout => "timeout",
            FetchError::Blocked => "blocked",
            FetchError::Network => "network",
            FetchError::TooLarge => "download_too_large",
            FetchError::Mismatch => "download_mismatch",
            FetchError::SignatureText => "signature_file",
            FetchError::Io => "download_io",
        }
    }
}

impl From<NetError> for FetchError {
    fn from(e: NetError) -> FetchError {
        match e {
            NetError::Timeout => FetchError::Timeout,
            NetError::TooLarge => FetchError::TooLarge,
            NetError::Redirect | NetError::InvalidUrl(_) => FetchError::Redirect,
            NetError::Blocked(_) => FetchError::Blocked,
            _ => FetchError::Network,
        }
    }
}

/// One response: its status and `Location`, and its body chunk by chunk.
/// Its `Debug` leaves out the `Location`, which may carry a signed query.
pub struct Opened {
    pub status: u16,
    pub location: Option<String>,
    pub body: Box<dyn Chunks>,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opened")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// A body, read a chunk at a time; `None` at its end.
pub trait Chunks: Send {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, FetchError>>;
}

/// Where downloads come from: [`Guarded`], or a test's own.
pub trait Source: Send + Sync {
    /// One `GET` of `url`, no redirect followed.
    fn open<'a>(&'a self, url: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>>;
}

/// A client for [`Guarded::new`]: public addresses only, through `route`
/// (Lodestar's egress setting, as the updater uses it), with no
/// whole-request timeout shorter than [`OVERALL`].
pub fn client(route: &Route) -> Result<GuardedClient, FetchError> {
    GuardedClient::with_route(
        NetPolicy::notification(false),
        OVERALL,
        Arc::new(SystemResolver),
        route,
    )
    .map_err(|_| FetchError::Route)
}

/// The real source: the caller's [`GuardedClient`]. Its own per-request
/// timeout should be at least [`OVERALL`]; a shorter one only ends a
/// download sooner.
#[derive(Debug)]
pub struct Guarded {
    client: GuardedClient,
}

impl Guarded {
    /// Refuses a client that may reach private or loopback addresses.
    pub fn new(client: GuardedClient) -> Result<Guarded, FetchError> {
        if client.policy() != NetPolicy::notification(false) {
            return Err(FetchError::Route);
        }
        Ok(Guarded { client })
    }

    /// Any client, so tests can reach a loopback server.
    #[cfg(any(test, feature = "testing"))]
    pub fn unchecked(client: GuardedClient) -> Guarded {
        Guarded { client }
    }
}

/// The request every hop sends: a bare `GET` with the companions' agent.
pub fn request(url: &Url) -> reqwest::Request {
    let mut req = reqwest::Request::new(reqwest::Method::GET, url.clone());
    req.headers_mut().insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(USER_AGENT),
    );
    req
}

struct Body(reqwest::Response);

impl Chunks for Body {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, FetchError>> {
        Box::pin(async move {
            match self.0.chunk().await {
                Ok(c) => Ok(c.map(|b| b.to_vec())),
                Err(e) if e.is_timeout() => Err(FetchError::Timeout),
                Err(_) => Err(FetchError::Network),
            }
        })
    }
}

impl Source for Guarded {
    fn open<'a>(&'a self, url: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
        Box::pin(async move {
            let resp = self.client.send(request(url)).await?;
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

/// Whether a hop may go to `url` for `entry`: `https`, the default port,
/// no credentials, a host on the entry's allowlist and [`DOWNLOAD_HOSTS`].
pub fn hop_allowed(entry: &Entry, url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|h| DOWNLOAD_HOSTS.contains(&h) && entry.allows_host(h))
}

/// The response for `url`, every hop through [`hop_allowed`].
/// Each hop's response headers must come within `idle`.
async fn open_allowed(
    src: &dyn Source,
    entry: &Entry,
    url: &str,
    idle: Duration,
) -> Result<Opened, FetchError> {
    let mut at = Url::parse(url).map_err(|_| FetchError::Redirect)?;
    for _ in 0..=MAX_HOPS {
        if !hop_allowed(entry, &at) {
            return Err(FetchError::Redirect);
        }
        let hop = tokio::time::timeout(idle, src.open(&at))
            .await
            .map_err(|_| FetchError::Timeout)??;
        match hop.status {
            200 => return Ok(hop),
            301 | 302 | 303 | 307 | 308 => {
                at = hop
                    .location
                    .as_deref()
                    .and_then(|l| at.join(l).ok())
                    .ok_or(FetchError::Redirect)?;
            }
            s => return Err(FetchError::Status(s)),
        }
    }
    Err(FetchError::Redirect)
}

/// A downloaded artifact whose size and SHA-256 are the pin's, open for
/// reading from its start. It reads and seeks like the file, so CMP-2's
/// extractor takes it as its archive.
#[derive(Debug)]
pub struct Verified {
    file: File,
    path: PathBuf,
    size: u64,
    sha256: Sha256,
    /// The companion's download slot: a second download, which would
    /// remove this file as stale, waits until the handle is gone.
    _busy: Arc<Busy>,
}

impl Verified {
    /// `<data>/companions/.downloads/<id>_<version>.verified`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn sha256(&self) -> &Sha256 {
        &self.sha256
    }

    /// Closes and deletes the file, once it has been extracted.
    pub fn discard(self) -> io::Result<()> {
        let Verified {
            file, path, _busy, ..
        } = self;
        drop(file);
        fs::remove_file(path)
    }
}

impl Read for Verified {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for Verified {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

/// `<data>/companions/.downloads`.
pub fn folder(data: &Path) -> PathBuf {
    data.join("companions").join(FOLDER)
}

fn file_name(entry: &Entry, pin: &Pin, ext: &str) -> String {
    format!("{}_{}.{ext}", entry.id, pin.version)
}

/// Downloads `pin` (one of `entry`'s) through `src` within
/// [`Limits::DEFAULT`]; `progress` gets the bytes so far after each chunk.
/// It takes a [`Guarded`] only, so every download goes through the
/// egress route and the SSRF guard; other sources are for tests.
pub async fn fetch(
    src: &Guarded,
    data: &Path,
    entry: &Entry,
    pin: &Pin,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<Verified, FetchError> {
    fetch_with(src, data, entry, pin, progress, Limits::DEFAULT, None).await
}

/// [`fetch`] from any source, for tests only.
#[cfg(any(test, feature = "testing"))]
pub async fn fetch_from(
    src: &dyn Source,
    data: &Path,
    entry: &Entry,
    pin: &Pin,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<Verified, FetchError> {
    fetch_with(src, data, entry, pin, progress, Limits::DEFAULT, None).await
}

/// [`fetch`] from any source within other limits, for tests only.
#[cfg(any(test, feature = "testing"))]
pub async fn fetch_limited(
    src: &dyn Source,
    data: &Path,
    entry: &Entry,
    pin: &Pin,
    progress: &mut (dyn FnMut(u64) + Send),
    limits: Limits,
) -> Result<Verified, FetchError> {
    fetch_with(src, data, entry, pin, progress, limits, None).await
}

/// Where the writer thread is, for tests that hold it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterAt {
    /// About to create the `.part` file.
    Start,
    /// Finished: the file is closed or handed back (or never created).
    Done,
}

type WriterHook = Arc<dyn Fn(WriterAt) + Send + Sync>;

/// A failure removes the `.part` file only once the writer thread has
/// finished with it, so no write (or the file's creation) lands after the
/// removal. The slot is held by the writer too, so a cancelled download
/// keeps it until its writer has stopped.
async fn fetch_with(
    src: &dyn Source,
    data: &Path,
    entry: &Entry,
    pin: &Pin,
    progress: &mut (dyn FnMut(u64) + Send),
    limits: Limits,
    hook: Option<WriterHook>,
) -> Result<Verified, FetchError> {
    let busy = Arc::new(Busy::take(entry.id.as_str())?);
    let dir = prepare(data).map_err(|_| FetchError::Io)?;
    remove_stale(&dir, entry);
    let part = dir.join(file_name(entry, pin, "part"));
    let deadline = tokio::time::Instant::now() + limits.overall;
    let got = fetch_to(
        src, &part, entry, pin, progress, limits, deadline, &busy, hook,
    )
    .await;
    let result = match got {
        Ok(file) => finish(
            file,
            &part,
            &dir.join(file_name(entry, pin, "verified")),
            pin,
            busy,
        ),
        Err(e) => Err(e),
    };
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// Renames the checked `.part` to `.verified` and rewinds the handle.
fn finish(
    mut file: File,
    part: &Path,
    verified: &Path,
    pin: &Pin,
    busy: Arc<Busy>,
) -> Result<Verified, FetchError> {
    file.seek(SeekFrom::Start(0)).map_err(|_| FetchError::Io)?;
    fs::rename(part, verified).map_err(|_| FetchError::Io)?;
    Ok(Verified {
        file,
        path: verified.to_path_buf(),
        size: pin.size,
        sha256: pin.sha256,
        _busy: busy,
    })
}

#[allow(clippy::too_many_arguments)]
async fn fetch_to(
    src: &dyn Source,
    path: &Path,
    entry: &Entry,
    pin: &Pin,
    progress: &mut (dyn FnMut(u64) + Send),
    limits: Limits,
    deadline: tokio::time::Instant,
    busy: &Arc<Busy>,
    hook: Option<WriterHook>,
) -> Result<File, FetchError> {
    let idle = limits.idle;
    let mut opened =
        tokio::time::timeout_at(deadline, open_allowed(src, entry, pin.url.as_str(), idle))
            .await
            .map_err(|_| FetchError::Timeout)??;
    let cap = pin.size.min(MAX_ARTIFACT_SIZE);
    // The writes, the hash and the fsync run off the async workers; this
    // side counts the bytes (the cap) and reports progress.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(QUEUE);
    let target = path.to_path_buf();
    let slot = Arc::clone(busy);
    let writer = tokio::task::spawn_blocking(move || -> io::Result<(File, [u8; 32])> {
        let _slot = slot;
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
            Ok((file, hash.finalize().into()))
        })();
        at(WriterAt::Done);
        written
    });
    let mut len = 0u64;
    // The deadline ends the streaming only: the writer is joined below
    // whatever happens, before the caller removes the file.
    let streamed = tokio::time::timeout_at(deadline, async {
        loop {
            let next = tokio::time::timeout(idle, opened.body.next())
                .await
                .map_err(|_| FetchError::Timeout)??;
            let Some(chunk) = next else {
                return Ok(());
            };
            len = len.saturating_add(chunk.len() as u64);
            if len > cap {
                return Err(FetchError::TooLarge);
            }
            // A writer that stopped has its error below.
            if tx.send(chunk).await.is_err() {
                return Ok(());
            }
            progress(len);
        }
    })
    .await
    .unwrap_or(Err(FetchError::Timeout));
    drop(tx);
    let written = writer.await.map_err(|_| FetchError::Io)?;
    streamed?;
    let (file, hash) = written.map_err(|_| FetchError::Io)?;
    verify::matches(pin, len, &hash).map_err(|_| FetchError::Mismatch)?;
    Ok(file)
}

/// How long a signature fetch may take, in all and between chunks.
pub const SIGNATURE_LIMITS: Limits = Limits {
    overall: Duration::from_secs(60),
    idle: Duration::from_secs(30),
};

/// Fetches `url`, an upstream signature on `entry`'s hosts, through the
/// same hops as an artifact within [`SIGNATURE_LIMITS`], refusing a body
/// over [`MAX_SIGNATURE`] or one that isn't UTF-8.
pub async fn fetch_signature(
    src: &Guarded,
    entry: &Entry,
    url: &str,
) -> Result<String, FetchError> {
    signature_with(src, entry, url).await
}

/// [`fetch_signature`] from any source, for tests only.
#[cfg(any(test, feature = "testing"))]
pub async fn fetch_signature_from(
    src: &dyn Source,
    entry: &Entry,
    url: &str,
) -> Result<String, FetchError> {
    signature_with(src, entry, url).await
}

async fn signature_with(src: &dyn Source, entry: &Entry, url: &str) -> Result<String, FetchError> {
    let limits = SIGNATURE_LIMITS;
    let deadline = tokio::time::Instant::now() + limits.overall;
    let read = async {
        let mut opened = open_allowed(src, entry, url, limits.idle).await?;
        let mut body = Vec::new();
        while let Some(chunk) = tokio::time::timeout(limits.idle, opened.body.next())
            .await
            .map_err(|_| FetchError::Timeout)??
        {
            if (body.len() + chunk.len()) as u64 > MAX_SIGNATURE {
                return Err(FetchError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        String::from_utf8(body).map_err(|_| FetchError::SignatureText)
    };
    tokio::time::timeout_at(deadline, read)
        .await
        .unwrap_or(Err(FetchError::Timeout))
}

/// Creates `<data>/companions` (with the default mode, as CMP-2's layout
/// does) and then `.downloads` in it (`0700` on Unix), checking each is a
/// plain folder, not a link, before anything is created inside it.
fn prepare(data: &Path) -> io::Result<PathBuf> {
    let companions = data.join("companions");
    let dir = companions.join(FOLDER);
    plain_folder(&companions, fs::DirBuilder::new())?;
    #[cfg_attr(windows, allow(unused_mut))] // only Unix sets a mode
    let mut private = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        private.mode(0o700);
    }
    plain_folder(&dir, private)?;
    Ok(dir)
}

/// Creates `dir` unless it exists; either way it must be a folder, not a
/// link (or a junction) to one.
fn plain_folder(dir: &Path, builder: fs::DirBuilder) -> io::Result<()> {
    match builder.create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    if fs::symlink_metadata(dir)?.file_type().is_dir() {
        Ok(())
    } else {
        Err(io::Error::other("not a plain folder"))
    }
}

/// Removes `entry`'s `.part` and `.verified` files: a download that
/// stopped, or one never extracted. A link of that name is removed too,
/// never followed. Other companions' files stay. A folder of that name
/// (or, on Windows, a folder link) isn't removed: the download then fails
/// with `download_io` until it's cleared by hand, which follows nothing.
fn remove_stale(dir: &Path, entry: &Entry) {
    let Ok(list) = fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{}_", entry.id);
    for e in list.flatten() {
        let name = e.file_name();
        let ours = name.to_str().is_some_and(|n| {
            n.strip_prefix(&prefix).is_some_and(|rest| {
                rest.strip_suffix(".part")
                    .or_else(|| rest.strip_suffix(".verified"))
                    .is_some_and(|v| crate::inputs::Version::parse(v).is_ok())
            })
        });
        if ours && e.file_type().is_ok_and(|t| !t.is_dir()) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// `path`, created new: never through a link; owner-only on Unix.
fn create_part(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    opts.open(path)
}

/// The companions with a download running, in this process.
static RUNNING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Holds one companion's download slot until dropped.
#[derive(Debug)]
struct Busy(String);

impl Busy {
    fn take(id: &str) -> Result<Busy, FetchError> {
        let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
        if !running
            .get_or_insert_with(HashSet::new)
            .insert(id.to_owned())
        {
            return Err(FetchError::Busy);
        }
        Ok(Busy(id.to_owned()))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(set) = running.as_mut() {
            set.remove(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `create_part` itself never opens through a link, even one planted
    /// after the stale files were removed.
    #[cfg(unix)]
    #[test]
    fn a_part_file_is_never_created_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"keep").unwrap();
        let part = dir.path().join("x_1.part");
        std::os::unix::fs::symlink(&target, &part).unwrap();
        assert!(create_part(&part).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        // A dangling link too: nothing is created at its target.
        let gone = dir.path().join("gone");
        let dangling = dir.path().join("y_1.part");
        std::os::unix::fs::symlink(&gone, &dangling).unwrap();
        assert!(create_part(&dangling).is_err());
        assert!(!gone.exists());
    }

    fn pinned(body: &[u8]) -> crate::Catalogue {
        let sha: String = sha2::Sha256::digest(body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let json = format!(
            r#"{{"format":1,"entries":[{{"id":"qbittorrent","name":"qBittorrent",
            "licence":{{"spdx":"GPL-2.0-or-later","url":"https://github.com/qbittorrent/qBittorrent/blob/master/COPYING"}},
            "homepage":"https://www.qbittorrent.org/","needed_for":["download_client"],"opt_in":false,
            "hosts":["github.com"],
            "native":{{"release":"https://github.com/userdocs/qbittorrent-nox-static/releases/download/","artifacts":[
              {{"platform":"linux","arch":"x86_64","archive":"binary","executable":null,
               "pin":{{"version":"5.0.3","url":"https://github.com/userdocs/qbittorrent-nox-static/releases/download/r/q",
                      "size":{},"sha256":"{sha}","upstream_sig":{{"kind":"none"}}}}}}]}},
            "container":null,"ports":[{{"name":"webui","port":8080,"proto":"tcp","exposure":"internal"}}],
            "health":null,"templates":["qbittorrent_conf"]}}]}}"#,
            body.len()
        );
        crate::Catalogue::from_json(&json).unwrap()
    }

    /// When the overall timeout fires, the writer is joined before the
    /// `.part` file is removed: it's held at its start past the deadline
    /// here, and has finished (and its file is gone) by the time the
    /// download returns, so nothing it does can land after the removal.
    #[tokio::test]
    async fn the_overall_timeout_joins_the_writer_before_removing_the_file() {
        struct OneThenStall(bool);
        impl Chunks for OneThenStall {
            fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, FetchError>> {
                if std::mem::replace(&mut self.0, true) {
                    return Box::pin(std::future::pending());
                }
                Box::pin(async { Ok(Some(vec![7u8; 100])) })
            }
        }
        struct Src;
        impl Source for Src {
            fn open<'a>(&'a self, _: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
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
        let c = pinned(&[1u8; 4500]);
        let entry = c.get("qbittorrent").unwrap();
        let pin = entry
            .artifact(
                crate::catalogue::Platform::Linux,
                crate::catalogue::Arch::X86_64,
            )
            .unwrap()
            .pin()
            .unwrap();
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
        let got = fetch_with(
            &Src,
            data.path(),
            entry,
            pin,
            &mut |_| {},
            limits,
            Some(hook),
        )
        .await;
        assert_eq!(got.err(), Some(FetchError::Timeout));
        assert!(
            done.load(std::sync::atomic::Ordering::SeqCst),
            "the writer was joined before the download returned"
        );
        let left = fs::read_dir(folder(data.path())).unwrap().count();
        assert_eq!(left, 0, "no .part file was created after the removal");
    }

    #[test]
    fn errors_have_stable_codes() {
        assert_eq!(FetchError::Status(404).code(), "http_status");
        assert_eq!(FetchError::Mismatch.code(), "download_mismatch");
        assert_eq!(FetchError::from(NetError::Timeout), FetchError::Timeout);
    }

    #[test]
    fn the_request_is_bare() {
        let url = Url::parse("https://github.com/a/b").unwrap();
        let req = request(&url);
        assert_eq!(req.method(), reqwest::Method::GET);
        let names: Vec<&str> = req.headers().keys().map(|k| k.as_str()).collect();
        assert_eq!(names, ["user-agent"]);
        assert_eq!(req.headers()["user-agent"], USER_AGENT);
        assert!(req.body().is_none());
    }
}
