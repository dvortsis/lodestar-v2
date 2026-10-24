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
    remove_stale_parts(bin);
    let path = part_path(bin, pin);
    let result = tokio::time::timeout(OVERALL, fetch_to(src, &path, pin, progress))
        .await
        .unwrap_or(Err(DownloadError::Fetch(CheckError::Timeout)));
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result.map(|()| path)
}

async fn fetch_to(
    src: &dyn Source,
    path: &Path,
    pin: &Pin,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<(), DownloadError> {
    let mut at = Url::parse(&pin.url).map_err(|_| CheckError::Redirect)?;
    let mut opened = None;
    for _ in 0..=MAX_HOPS {
        if !hop_allowed(&at) {
            return Err(CheckError::Redirect.into());
        }
        let hop = src.open(&at).await?;
        match hop.status {
            200 => {
                opened = Some(hop);
                break;
            }
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
    let Some(mut opened) = opened else {
        return Err(CheckError::Redirect.into());
    };
    let mut file = create_part(path).map_err(|_| DownloadError::Io)?;
    let mut hash = sha2::Sha256::new();
    let mut len = 0u64;
    while let Some(chunk) = opened.body.next().await? {
        len += chunk.len() as u64;
        if len > pin.size {
            return Err(DownloadError::TooLarge);
        }
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|_| DownloadError::Io)?;
        progress(len);
    }
    file.sync_all().map_err(|_| DownloadError::Io)?;
    if len != pin.size || Sha256(hash.finalize().into()) != pin.sha256 {
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

    /// An in-memory download source: URL → (status, Location, body), the
    /// body served in chunks of [`Fake::CHUNK`]; every request recorded.
    #[derive(Default)]
    pub struct Fake {
        pub routes: Mutex<HashMap<String, (u16, Option<String>, Vec<u8>)>>,
        pub asked: Mutex<Vec<String>>,
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
