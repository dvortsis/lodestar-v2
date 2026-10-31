//! Download clients (§9): one-click and bulk send, "Send selected files"
//! (§10.5).
//!
//! Each client is an adapter in `src/clients/<name>.rs` implementing
//! [`DownloadClient`]. [`qbittorrent`] is the reference; the README lists
//! what a new adapter must do and test.

pub mod aria2;
pub mod deluge;
pub mod magnet;
pub mod qbittorrent;
pub mod rtorrent;
pub mod transmission;
mod xmlrpc;

use std::fmt;
use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, SeqAccess, Visitor};

use crate::BoxFuture;
use crate::net::{AddrClass, NetError, redact};
use crate::secret::Secret;

pub use magnet::{Magnet, MagnetError};

/// Which client an adapter talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientKind {
    QBittorrent,
    Transmission,
    Deluge,
    RTorrent,
    Aria2,
}

impl ClientKind {
    /// The client's own name, for "Send to qBittorrent" (§10.14).
    pub fn display_name(self) -> &'static str {
        match self {
            ClientKind::QBittorrent => "qBittorrent",
            ClientKind::Transmission => "Transmission",
            ClientKind::Deluge => "Deluge",
            ClientKind::RTorrent => "rTorrent",
            ClientKind::Aria2 => "Aria2",
        }
    }
}

/// What a client supports, so the UI offers only what will work and
/// [`check_request`] refuses the rest before any network call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// Categories or labels.
    pub category: bool,
    pub save_path: bool,
    /// Adding without starting.
    pub paused: bool,
    /// Downloading only some of a torrent's files.
    pub file_selection: bool,
    /// File selection for a magnet too. A client that fetches a paused
    /// magnet's metadata only once it's started can't select before
    /// anything downloads; there, only a `.torrent` can be selected.
    pub magnet_file_selection: bool,
    /// Adding from `.torrent` bytes, not only magnets.
    pub torrent_file: bool,
    /// Several magnets in one request (adapters without it loop).
    pub batch: bool,
}

/// A feature a request asked for, named in [`ClientError::Unsupported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Category,
    SavePath,
    Paused,
    FileSelection,
    /// File selection for a magnet (a `.torrent` still can be).
    MagnetFileSelection,
    TorrentFile,
}

impl Feature {
    fn describe(self) -> &'static str {
        match self {
            Feature::Category => "categories or labels",
            Feature::SavePath => "a save path",
            Feature::Paused => "adding paused",
            Feature::FileSelection => "selecting files",
            Feature::MagnetFileSelection => {
                "selecting a magnet's files; send all files, or the .torrent file"
            }
            Feature::TorrentFile => "adding .torrent files",
        }
    }
}

/// Options for an add. Every field is optional; [`check_request`] checks
/// them against the client's [`Capabilities`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddOptions {
    pub save_path: Option<String>,
    /// The category (qBittorrent) or label (Deluge, rTorrent).
    pub category: Option<String>,
    pub paused: bool,
    /// Zero-based file indices to download, in the torrent's file order;
    /// every other file is skipped. Only for a single torrent.
    pub selected_files: Option<Vec<u32>>,
}

/// A `.torrent` file to add. The caller (which parsed it) supplies the v1
/// info-hash, which the client needs to find the torrent afterwards for
/// file selection.
#[derive(Clone, PartialEq, Eq)]
pub struct TorrentFile {
    pub bytes: Vec<u8>,
    /// The torrent's ID in the client's API: lowercase hex, 40 characters.
    /// For a v1 or hybrid torrent that's the v1 info-hash. For a v2-only
    /// torrent, pass the first 20 bytes of the v2 (SHA-256) info-hash, as
    /// [`Magnet::torrent_id`] does; qBittorrent and libtorrent use that
    /// truncated hash as the ID.
    pub info_hash: String,
}

impl TorrentFile {
    pub fn new(bytes: Vec<u8>, info_hash: &str) -> Result<TorrentFile, ClientError> {
        if info_hash.len() != 40 || !info_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ClientError::InvalidRequest(
                "the info-hash isn't 40 hex digits",
            ));
        }
        if bytes.is_empty() || bytes.len() > MAX_TORRENT_FILE {
            return Err(ClientError::InvalidRequest(
                "the .torrent file is empty or larger than 16 MiB",
            ));
        }
        Ok(TorrentFile {
            bytes,
            info_hash: info_hash.to_ascii_lowercase(),
        })
    }
}

impl fmt::Debug for TorrentFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorrentFile")
            .field("bytes", &self.bytes.len())
            .field("info_hash", &self.info_hash)
            .finish()
    }
}

/// The largest `.torrent` a client is sent.
pub const MAX_TORRENT_FILE: usize = 16 << 20;

/// What [`DownloadClient::test`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub kind: ClientKind,
    /// The client's version, as it reports it (`v4.6.3`).
    pub version: String,
    /// Its API version, when it has one separate from the version.
    pub api_version: Option<String>,
}

/// A category or label the client knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub name: String,
    pub save_path: Option<String>,
}

/// The result of an accepted add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddOutcome {
    /// How many torrents the request carried (the client accepted the
    /// request; duplicates are the client's business).
    pub sent: usize,
    pub selection: Selection,
}

/// What happened to a file selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// No selection was asked for.
    None,
    /// Unselected files were set to "don't download".
    Applied,
    /// The client hadn't fetched the magnet's metadata in time, so the
    /// files couldn't be selected. The torrent was left paused so nothing
    /// unwanted downloads; the UI says so.
    Pending,
}

/// Why a client couldn't do what was asked. No variant carries a URL,
/// credential, cookie or raw response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// The client refused the credentials or the session.
    Auth(AuthFailure),
    /// No connection: DNS, refused, reset, TLS.
    Unreachable,
    /// The client answered and said no. The reason is the client's own,
    /// trimmed to one printable line of at most 200 characters.
    Rejected(String),
    /// The client can't do this.
    Unsupported(Feature),
    Timeout,
    /// The URL reaches an address class download clients may not use.
    Blocked(AddrClass),
    /// The request itself is wrong (nothing to add, bad index, bad URL).
    InvalidRequest(&'static str),
    /// The client answered with something this adapter doesn't understand.
    Protocol(&'static str),
    /// A bulk send failed part-way: the client took the first `sent`
    /// torrents (counted as for [`AddOutcome::sent`]), then `cause`
    /// stopped the rest. Retrying all of them would send those again.
    Partial {
        sent: usize,
        cause: Box<ClientError>,
    },
}

/// Why authentication failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    /// Wrong user name or password.
    BadCredentials,
    /// The client banned this address after failed logins.
    Banned,
    /// The client refused the request's origin: its CSRF or host-header
    /// check doesn't match the URL Lodestar uses (a reverse proxy, a
    /// different host name).
    OriginRefused,
    /// The session was refused again straight after logging in.
    SessionRefused,
    /// The client refuses this address: it isn't on the client's IP
    /// allow-list, or the client banned it after failed logins. Some
    /// clients (Transmission) answer both the same way.
    AddressRefused,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Auth(AuthFailure::BadCredentials) => {
                f.write_str("the client refused the user name or password")
            }
            ClientError::Auth(AuthFailure::Banned) => f.write_str(
                "the client has banned this address after failed logins; wait, or unban it in the client",
            ),
            ClientError::Auth(AuthFailure::OriginRefused) => f.write_str(
                "the client refused the request's origin; check the URL matches the client's host-header and CSRF settings",
            ),
            ClientError::Auth(AuthFailure::SessionRefused) => {
                f.write_str("the client refused the session after logging in")
            }
            ClientError::Auth(AuthFailure::AddressRefused) => f.write_str(
                "the client refuses this address; check its IP allow-list, or whether it banned this address after failed logins",
            ),
            ClientError::Unreachable => f.write_str("couldn't reach the client"),
            ClientError::Rejected(why) => write!(f, "the client refused: {why}"),
            ClientError::Unsupported(feature) => {
                write!(f, "this client doesn't support {}", feature.describe())
            }
            ClientError::Timeout => f.write_str("the client didn't answer in time"),
            ClientError::Blocked(class) => write!(
                f,
                "{}",
                NetError::Blocked(*class)
            ),
            ClientError::InvalidRequest(why) => write!(f, "invalid request: {why}"),
            ClientError::Protocol(why) => write!(f, "unexpected answer from the client: {why}"),
            ClientError::Partial { sent, cause } => {
                write!(f, "the client took the first {sent}, then: {cause}")
            }
        }
    }
}

impl std::error::Error for ClientError {}

impl From<NetError> for ClientError {
    fn from(e: NetError) -> ClientError {
        match e {
            NetError::Blocked(class) => ClientError::Blocked(class),
            NetError::Timeout => ClientError::Timeout,
            NetError::InvalidUrl(why) => ClientError::InvalidRequest(why),
            NetError::TooLarge => ClientError::Protocol("the response was too large"),
            NetError::Redirect => ClientError::Protocol("the client redirected"),
            NetError::Resolve | NetError::Connect | NetError::Http => ClientError::Unreachable,
        }
    }
}

/// Cleans a client-supplied reason for [`ClientError::Rejected`]: one line,
/// printable characters only, at most 200 characters.
pub fn client_reason(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let mut out: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.is_empty() {
        "no reason given".to_owned()
    } else {
        out
    }
}

/// The checks every adapter runs before touching the network: something
/// to add, every requested feature supported, sane text fields and a usable
/// file selection. Returns the selection sorted and de-duplicated.
pub fn check_request(
    caps: Capabilities,
    opts: &AddOptions,
    torrents: usize,
) -> Result<Option<Vec<u32>>, ClientError> {
    if torrents == 0 {
        return Err(ClientError::InvalidRequest("nothing to add"));
    }
    if let Some(category) = &opts.category {
        if !caps.category {
            return Err(ClientError::Unsupported(Feature::Category));
        }
        check_text(category)?;
    }
    if let Some(path) = &opts.save_path {
        if !caps.save_path {
            return Err(ClientError::Unsupported(Feature::SavePath));
        }
        check_text(path)?;
    }
    if opts.paused && !caps.paused {
        return Err(ClientError::Unsupported(Feature::Paused));
    }
    let Some(selected) = &opts.selected_files else {
        return Ok(None);
    };
    if !caps.file_selection {
        return Err(ClientError::Unsupported(Feature::FileSelection));
    }
    if torrents != 1 {
        return Err(ClientError::InvalidRequest(
            "file selection applies to one torrent at a time",
        ));
    }
    if selected.is_empty() {
        return Err(ClientError::InvalidRequest("select at least one file"));
    }
    let mut selected = selected.clone();
    selected.sort_unstable();
    selected.dedup();
    Ok(Some(selected))
}

/// [`check_request`] for magnets: also refuses a selection where the
/// client can't select a magnet's files.
pub fn check_magnets(
    caps: Capabilities,
    opts: &AddOptions,
    magnets: usize,
) -> Result<Option<Vec<u32>>, ClientError> {
    let selection = check_request(caps, opts, magnets)?;
    if selection.is_some() && !caps.magnet_file_selection {
        return Err(ClientError::Unsupported(Feature::MagnetFileSelection));
    }
    Ok(selection)
}

fn check_text(value: &str) -> Result<(), ClientError> {
    if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(ClientError::InvalidRequest(
            "a category or save path is empty, too long, or has control characters",
        ));
    }
    Ok(())
}

/// A download client. All methods are cancel-safe to drop; none leaves a
/// half-added torrent running (file selection adds paused first).
pub trait DownloadClient: Send + Sync + fmt::Debug {
    fn kind(&self) -> ClientKind;

    fn capabilities(&self) -> Capabilities;

    /// Checks reachability and credentials, and reports the version.
    fn test(&self) -> BoxFuture<'_, Result<ClientInfo, ClientError>>;

    /// Adds magnets in one request where the client allows it.
    fn add_magnets<'a>(
        &'a self,
        magnets: &'a [Magnet],
        opts: &'a AddOptions,
    ) -> BoxFuture<'a, Result<AddOutcome, ClientError>>;

    /// Adds one magnet.
    fn add_magnet<'a>(
        &'a self,
        magnet: &'a Magnet,
        opts: &'a AddOptions,
    ) -> BoxFuture<'a, Result<AddOutcome, ClientError>> {
        self.add_magnets(std::slice::from_ref(magnet), opts)
    }

    /// Adds a `.torrent` file.
    fn add_torrent_file<'a>(
        &'a self,
        file: &'a TorrentFile,
        opts: &'a AddOptions,
    ) -> BoxFuture<'a, Result<AddOutcome, ClientError>>;

    /// The categories or labels the client knows, sorted by name.
    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>, ClientError>>;
}

/// HTTP basic authentication: Transmission's RPC password, or the web
/// server in front of rTorrent's XML-RPC.
#[derive(Debug, Clone)]
pub struct BasicAuth {
    pub username: String,
    pub password: Secret,
}

/// A configured client, as Settings → Integrations stores it (credentials
/// already opened from the [`SecretStore`](crate::SecretStore)). Its
/// `Debug` shows each URL's scheme, host and port only.
#[derive(Clone)]
pub enum ClientConfig {
    QBittorrent {
        url: String,
        username: String,
        password: Secret,
    },
    /// The daemon's address (`http://host:9091/`), or the full RPC URL.
    Transmission {
        url: String,
        auth: Option<BasicAuth>,
    },
    /// The Web UI's address (`http://host:8112/`).
    Deluge { url: String, password: Secret },
    /// The XML-RPC endpoint a web server exposes (`http://host/RPC2`).
    RTorrent {
        url: String,
        auth: Option<BasicAuth>,
    },
    /// The RPC address (`http://host:6800/jsonrpc`); `secret` is
    /// `--rpc-secret`.
    Aria2 { url: String, secret: Option<Secret> },
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, url) = match self {
            ClientConfig::QBittorrent { url, .. } => (ClientKind::QBittorrent, url),
            ClientConfig::Transmission { url, .. } => (ClientKind::Transmission, url),
            ClientConfig::Deluge { url, .. } => (ClientKind::Deluge, url),
            ClientConfig::RTorrent { url, .. } => (ClientKind::RTorrent, url),
            ClientConfig::Aria2 { url, .. } => (ClientKind::Aria2, url),
        };
        let url = match reqwest::Url::parse(url) {
            Ok(u) => redact(&u),
            Err(_) => "(not a URL)".to_owned(),
        };
        f.debug_struct("ClientConfig")
            .field("kind", &kind)
            .field("url", &url)
            .finish_non_exhaustive()
    }
}

/// The default timeout for one client request.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(15);

/// Builds the adapter for a configuration. The URL is checked against the
/// download-client policy here, so a bad address fails when it's saved.
pub fn connect(config: ClientConfig) -> Result<Box<dyn DownloadClient>, ClientError> {
    match config {
        ClientConfig::QBittorrent {
            url,
            username,
            password,
        } => Ok(Box::new(qbittorrent::QBittorrent::new(
            &url, username, password,
        )?)),
        ClientConfig::Transmission { url, auth } => {
            Ok(Box::new(transmission::Transmission::new(&url, auth)?))
        }
        ClientConfig::Deluge { url, password } => {
            Ok(Box::new(deluge::Deluge::new(&url, password)?))
        }
        ClientConfig::RTorrent { url, auth } => Ok(Box::new(rtorrent::RTorrent::new(&url, auth)?)),
        ClientConfig::Aria2 { url, secret } => Ok(Box::new(aria2::Aria2::new(&url, secret)?)),
    }
}

/// How long an adapter waits for a magnet's metadata before leaving a file
/// selection [`Selection::Pending`] (§16).
pub const METADATA_WAIT: Duration = Duration::from_secs(30);

/// How often an adapter looks while it waits.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Calls `look` until it returns `Some`, and returns that; `None` once
/// `wait` has passed. It always looks at least once, and never sleeps past
/// the deadline.
pub(crate) async fn poll<T, F, Fut>(
    wait: Duration,
    every: Duration,
    mut look: F,
) -> Result<Option<T>, ClientError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>, ClientError>>,
{
    let deadline = Instant::now() + wait;
    loop {
        if let Some(found) = look().await? {
            return Ok(Some(found));
        }
        if Instant::now() + every > deadline {
            return Ok(None);
        }
        tokio::time::sleep(every).await;
    }
}

/// The zero-based indices a selection skips, given the torrent's file
/// count. `selected` is sorted and de-duplicated ([`check_request`]). An
/// index past the last file is refused; the caller has left the torrent
/// paused by then.
pub(crate) fn skipped_files(selected: &[u32], count: usize) -> Result<Vec<u32>, ClientError> {
    check_file_count(count)?;
    if selected.last().is_some_and(|&last| last as usize >= count) {
        return Err(ClientError::InvalidRequest(
            "a selected file isn't in the torrent; it was added paused",
        ));
    }
    Ok((0..count as u32)
        .filter(|i| selected.binary_search(i).is_err())
        .collect())
}

/// Standard base64 with padding, for `.torrent` bytes in JSON and XML-RPC
/// bodies.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(TABLE[(n >> shift & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The most files a torrent is believed to have. A client reporting more is
/// broken or hostile, and the count would otherwise size allocations.
pub(crate) const MAX_FILES: usize = 1_000_000;

/// Refuses a file count past [`MAX_FILES`].
pub(crate) fn check_file_count(count: usize) -> Result<usize, ClientError> {
    if count > MAX_FILES {
        return Err(ClientError::Protocol(
            "the client reported more files than a torrent has",
        ));
    }
    Ok(count)
}

/// A JSON list read only for its length, refused past [`MAX_FILES`]
/// entries without storing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileCount(pub(crate) usize);

impl<'de> Deserialize<'de> for FileCount {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<FileCount, D::Error> {
        struct Counter;
        impl<'de> Visitor<'de> for Counter {
            type Value = FileCount;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a file list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<FileCount, A::Error> {
                let mut n = 0;
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    n += 1;
                    if n > MAX_FILES {
                        return Err(serde::de::Error::custom("too many files"));
                    }
                }
                Ok(FileCount(n))
            }
        }
        d.deserialize_seq(Counter)
    }
}

/// A JSON list of file priorities, at most [`MAX_FILES`] long; an entry
/// that isn't a non-negative integer is `None`, keeping later entries in
/// place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Priorities(pub(crate) Vec<Option<u64>>);

impl<'de> Deserialize<'de> for Priorities {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Priorities, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Entry {
            Number(u64),
            Other(IgnoredAny),
        }
        struct Collector;
        impl<'de> Visitor<'de> for Collector {
            type Value = Priorities;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a priority list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Priorities, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = seq.next_element::<Entry>()? {
                    if out.len() == MAX_FILES {
                        return Err(serde::de::Error::custom("too many files"));
                    }
                    out.push(match entry {
                        Entry::Number(n) => Some(n),
                        Entry::Other(_) => None,
                    });
                }
                Ok(Priorities(out))
            }
        }
        d.deserialize_seq(Collector)
    }
}

/// A category or label name from the client: control characters dropped,
/// trimmed, at most 256 characters; `None` when nothing is left.
pub(crate) fn category_name(raw: &str) -> Option<String> {
    let name: String = raw.chars().filter(|c| !c.is_control()).take(256).collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// The error for a bulk send that failed after `sent` torrents went in.
pub(crate) fn partial(sent: usize, cause: ClientError) -> ClientError {
    if sent == 0 {
        cause
    } else {
        ClientError::Partial {
            sent,
            cause: Box::new(cause),
        }
    }
}

/// The reason [`already_there`] gives.
const ALREADY_THERE: &str = "the torrent is already in the client";

/// The answer to an add that found every torrent already in the client.
pub(crate) fn already_there() -> ClientError {
    ClientError::Rejected(ALREADY_THERE.into())
}

/// Whether `e` is [`already_there`]'s answer: every torrent of the add was
/// in the client already, which a send counts as done (§9).
pub fn is_already_there(e: &ClientError) -> bool {
    matches!(e, ClientError::Rejected(why) if why == ALREADY_THERE)
}

/// The error for an HTTP status an adapter has no meaning for.
pub(crate) fn rejected_status(status: u16, body: &[u8]) -> ClientError {
    ClientError::Rejected(format!("HTTP {status}: {}", client_reason(body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Capabilities = Capabilities {
        category: true,
        save_path: true,
        paused: true,
        file_selection: true,
        magnet_file_selection: false,
        torrent_file: true,
        batch: true,
    };

    #[test]
    fn requests_are_checked_against_capabilities() {
        let none = Capabilities::default();
        let opts = |f: fn(&mut AddOptions)| {
            let mut o = AddOptions::default();
            f(&mut o);
            o
        };
        assert_eq!(
            check_request(ALL, &AddOptions::default(), 0),
            Err(ClientError::InvalidRequest("nothing to add"))
        );
        assert_eq!(check_request(none, &AddOptions::default(), 3), Ok(None));
        assert_eq!(
            check_request(none, &opts(|o| o.category = Some("tv".into())), 1),
            Err(ClientError::Unsupported(Feature::Category))
        );
        assert_eq!(
            check_request(none, &opts(|o| o.save_path = Some("/d".into())), 1),
            Err(ClientError::Unsupported(Feature::SavePath))
        );
        assert_eq!(
            check_request(none, &opts(|o| o.paused = true), 1),
            Err(ClientError::Unsupported(Feature::Paused))
        );
        assert_eq!(
            check_request(none, &opts(|o| o.selected_files = Some(vec![1])), 1),
            Err(ClientError::Unsupported(Feature::FileSelection))
        );
        assert!(matches!(
            check_request(ALL, &opts(|o| o.selected_files = Some(vec![1])), 2),
            Err(ClientError::InvalidRequest(_))
        ));
        assert!(matches!(
            check_request(ALL, &opts(|o| o.selected_files = Some(vec![])), 1),
            Err(ClientError::InvalidRequest(_))
        ));
        assert_eq!(
            check_request(ALL, &opts(|o| o.selected_files = Some(vec![4, 1, 4])), 1),
            Ok(Some(vec![1, 4]))
        );
        for bad in ["", "  ", "a\nb", "tv\r", &"x".repeat(5000)] {
            let o = AddOptions {
                category: Some(bad.into()),
                ..AddOptions::default()
            };
            assert!(matches!(
                check_request(ALL, &o, 1),
                Err(ClientError::InvalidRequest(_))
            ));
        }
    }

    #[test]
    fn client_reasons_are_one_clean_line() {
        assert_eq!(
            client_reason(b"Error: 'x.torrent' is not a valid torrent file."),
            "Error: 'x.torrent' is not a valid torrent file."
        );
        assert_eq!(client_reason(b"a\r\n\x1b[31mb\tc"), "a [31mb c");
        assert_eq!(client_reason(b""), "no reason given");
        assert_eq!(client_reason(&[b'x'; 1000]).len(), 200);
    }

    #[test]
    fn torrent_files_are_checked() {
        assert!(TorrentFile::new(vec![1], &"A".repeat(40)).is_ok());
        assert!(TorrentFile::new(vec![1], "abc").is_err());
        assert!(TorrentFile::new(vec![], &"a".repeat(40)).is_err());
        assert!(TorrentFile::new(vec![1], &"g".repeat(40)).is_err());
        let f = TorrentFile::new(b"d4:infod3:key5:valueee".to_vec(), &"A".repeat(40)).unwrap();
        assert_eq!(f.info_hash, "a".repeat(40));
        assert!(!format!("{f:?}").contains("value"));
    }

    #[test]
    fn base64_matches_rfc_4648() {
        for (raw, enc) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
            (&[0xff, 0xfe, 0x00], "//4A"),
        ] {
            assert_eq!(base64(raw), enc);
        }
    }

    #[test]
    fn hostile_file_counts_are_refused_without_allocating() {
        assert!(matches!(
            skipped_files(&[0], 20_000_000),
            Err(ClientError::Protocol(_))
        ));
        assert!(matches!(
            skipped_files(&[0], usize::MAX),
            Err(ClientError::Protocol(_))
        ));
        assert_eq!(
            skipped_files(&[0], MAX_FILES).map(|v| v.len()),
            Ok(MAX_FILES - 1)
        );
        let count = |json: &str| serde_json::from_str::<FileCount>(json);
        assert_eq!(count("[1, {}, []]").unwrap(), FileCount(3));
        let many = format!("[{}0]", "0,".repeat(MAX_FILES));
        assert!(count(&many).is_err());
        assert!(serde_json::from_str::<Priorities>(&many).is_err());
        assert_eq!(
            serde_json::from_str::<Priorities>("[4, \"x\", -1, 7]").unwrap(),
            Priorities(vec![Some(4), None, None, Some(7)])
        );
    }

    #[test]
    fn magnet_selection_needs_its_own_capability() {
        let select = AddOptions {
            selected_files: Some(vec![0]),
            ..AddOptions::default()
        };
        assert_eq!(
            check_magnets(ALL, &select, 1),
            Err(ClientError::Unsupported(Feature::MagnetFileSelection))
        );
        let caps = Capabilities {
            magnet_file_selection: true,
            ..ALL
        };
        assert_eq!(check_magnets(caps, &select, 1), Ok(Some(vec![0])));
        assert_eq!(check_request(ALL, &select, 1), Ok(Some(vec![0])));
    }

    #[test]
    fn partial_sends_and_category_names() {
        assert_eq!(partial(0, ClientError::Timeout), ClientError::Timeout);
        let e = partial(2, ClientError::Timeout);
        assert_eq!(
            e.to_string(),
            "the client took the first 2, then: the client didn't answer in time"
        );
        assert_eq!(
            category_name(" TV\u{1b}[31m\nShows "),
            Some("TV[31mShows".into())
        );
        assert_eq!(category_name("\u{0}\t "), None);
        assert_eq!(category_name(&"x".repeat(300)).map(|n| n.len()), Some(256));
    }

    #[test]
    fn config_debug_shows_only_scheme_host_and_port() {
        let config = ClientConfig::Aria2 {
            url: "http://user:pw@nas:6800/jsonrpc?token=hunter2".into(),
            secret: Some(Secret::new("s3cret")),
        };
        let debug = format!("{config:?}");
        assert!(
            debug.contains("nas:6800") && debug.contains("Aria2"),
            "{debug}"
        );
        for secret in ["hunter2", "pw", "s3cret", "jsonrpc"] {
            assert!(!debug.contains(secret), "{debug}");
        }
    }

    #[test]
    fn skipped_files_and_range() {
        assert_eq!(skipped_files(&[0, 2], 3), Ok(vec![1]));
        assert_eq!(skipped_files(&[0, 1, 2], 3), Ok(vec![]));
        assert_eq!(skipped_files(&[1], 4), Ok(vec![0, 2, 3]));
        assert!(matches!(
            skipped_files(&[0, 3], 3),
            Err(ClientError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn poll_looks_once_with_no_wait_and_stops_when_found() {
        let mut looks = 0;
        let found: Option<()> = poll(Duration::ZERO, Duration::from_millis(1), || {
            looks += 1;
            async { Ok(None) }
        })
        .await
        .unwrap();
        assert_eq!((found, looks), (None, 1));
        let mut looks = 0;
        let found = poll(Duration::from_secs(5), Duration::from_millis(1), || {
            looks += 1;
            let n = looks;
            async move { Ok((n == 3).then_some(n)) }
        })
        .await
        .unwrap();
        assert_eq!(found, Some(3));
    }

    #[test]
    fn errors_carry_no_secrets_or_urls() {
        let text = format!("{}", ClientError::Rejected(client_reason(b"nope")));
        assert_eq!(text, "the client refused: nope");
        assert_eq!(
            ClientError::from(NetError::Blocked(AddrClass::Metadata)),
            ClientError::Blocked(AddrClass::Metadata)
        );
        assert_eq!(
            ClientError::from(NetError::Connect),
            ClientError::Unreachable
        );
    }
}
