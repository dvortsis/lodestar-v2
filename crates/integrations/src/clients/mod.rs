//! Download clients (§9): one-click and bulk send, "Send selected files"
//! (§10.5).
//!
//! Each client is an adapter in `src/clients/<name>.rs` implementing
//! [`DownloadClient`]. [`qbittorrent`] is the reference; the README lists
//! what a new adapter must do and test.
//!
//! Listing a client's torrents and exporting their `.torrent` bytes
//! (SRC-11, a client as a source) is a [`Lister`] over a [`Wire`], so a
//! source plugin can speak the protocol through its own egress. The
//! adapters' [`DownloadClient::list`] and [`DownloadClient::export`] are
//! the same listers over their guarded client. Every list answer is
//! untrusted and parsed within [`MAX_LIST_BYTES`] and [`MAX_LISTED`].

pub mod aria2;
pub mod deluge;
pub mod magnet;
pub mod qbittorrent;
pub mod rtorrent;
pub mod transmission;
mod xmlrpc;

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::BoxFuture;
use crate::net::{AddrClass, GuardedClient, NetError, NetPolicy, redact};
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
    /// Listing the client's torrents ([`DownloadClient::list`]).
    List,
    /// Exporting a torrent's `.torrent` bytes ([`DownloadClient::export`]).
    Export,
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
            Feature::List => "listing its torrents",
            Feature::Export => "exporting .torrent files",
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
    /// Every torrent of the add was in the client already: it's where the
    /// user wants it, which a send counts as done (§9).
    AlreadyThere,
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
            ClientError::AlreadyThere => f.write_str("the torrent is already in the client"),
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

    /// The torrents in the client (SRC-11), parsed within the
    /// [`MAX_LIST_BYTES`] and [`MAX_LISTED`] bounds; malformed entries are
    /// skipped and counted. Unsupported unless the adapter overrides it.
    fn list(&self) -> BoxFuture<'_, Result<Listing, ClientError>> {
        Box::pin(async { Err(ClientError::Unsupported(Feature::List)) })
    }

    /// The `.torrent` bytes of the listed torrent `hash` (40 hex digits),
    /// at most [`MAX_TORRENT_FILE`]. Untrusted: parse them with
    /// `lodestar-metainfo` and check the info-hash before using them.
    fn export<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, Result<Vec<u8>, ClientError>> {
        let _ = hash;
        Box::pin(async { Err(ClientError::Unsupported(Feature::Export)) })
    }
}

/// Lists and exports a client's torrents over a [`Wire`], so a caller
/// that owns the transport (a source plugin's egress, SRC-5) speaks the
/// client's protocol without building its own HTTP client. The adapters'
/// [`DownloadClient::list`] and [`DownloadClient::export`] are this over
/// their guarded client.
pub trait Lister: Send + Sync + fmt::Debug {
    fn kind(&self) -> ClientKind;

    /// Whether [`export`](Self::export) can work at all for this client.
    fn can_export(&self) -> bool;

    /// The client's torrents. The list request is marked
    /// [`WireRequest::poll`]; logins and retries aren't.
    fn list(&self) -> BoxFuture<'_, Result<Listing, ClientError>>;

    /// The `.torrent` bytes of `hash`, or `Unsupported(Export)`.
    fn export<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, Result<Vec<u8>, ClientError>>;
}

/// The [`Lister`] for a configured client over `wire`. rTorrent and Aria2
/// have none: `Unsupported(List)`.
pub fn lister(config: &ClientConfig, wire: Arc<dyn Wire>) -> Result<Box<dyn Lister>, ClientError> {
    let policy = NetPolicy::download_client();
    match config {
        ClientConfig::QBittorrent {
            url,
            username,
            password,
        } => Ok(Box::new(qbittorrent::QbitLister::new(
            url,
            username.clone(),
            password.clone(),
            wire,
            policy,
        )?)),
        ClientConfig::Transmission { url, auth } => Ok(Box::new(
            transmission::TransmissionLister::new(url, auth.clone(), wire, policy)?,
        )),
        ClientConfig::Deluge { url, password } => Ok(Box::new(deluge::DelugeLister::new(
            url,
            password.clone(),
            wire,
            policy,
        )?)),
        ClientConfig::RTorrent { .. } | ClientConfig::Aria2 { .. } => {
            Err(ClientError::Unsupported(Feature::List))
        }
    }
}

/// An HTTP method a [`Lister`] uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireMethod {
    Get,
    Post,
}

impl WireMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            WireMethod::Get => "GET",
            WireMethod::Post => "POST",
        }
    }
}

/// One request in plain values. Header values may hold a session cookie
/// or a password, so `Debug` prints header names only.
#[derive(Clone)]
pub struct WireRequest {
    pub method: WireMethod,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
    /// The body is refused past this many bytes.
    pub limit: usize,
    /// This is the poll itself (the list), not a login, retry or export:
    /// a source plugin sends it through its poll gate.
    pub poll: bool,
}

impl fmt::Debug for WireRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(n, _)| *n).collect();
        f.debug_struct("WireRequest")
            .field("method", &self.method)
            .field("headers", &names)
            .field("body", &self.body.len())
            .field("poll", &self.poll)
            .finish_non_exhaustive()
    }
}

/// One answer, the body read whole within the request's limit.
#[derive(Clone, PartialEq, Eq)]
pub struct WireResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl fmt::Debug for WireResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireResponse")
            .field("status", &self.status)
            .field("body", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl WireResponse {
    /// Every value of header `name`, ignoring ASCII case.
    pub fn headers<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Sends one [`WireRequest`]. It never follows redirects, and refuses a
/// body past the request's limit.
pub trait Wire: Send + Sync {
    fn exchange<'a>(
        &'a self,
        req: &'a WireRequest,
    ) -> BoxFuture<'a, Result<WireResponse, ClientError>>;
}

/// A [`Wire`] over the download-client [`GuardedClient`].
#[derive(Debug, Clone)]
pub struct GuardedWire(pub GuardedClient);

impl Wire for GuardedWire {
    fn exchange<'a>(
        &'a self,
        req: &'a WireRequest,
    ) -> BoxFuture<'a, Result<WireResponse, ClientError>> {
        Box::pin(async move {
            let headers: Vec<(String, String)> = req
                .headers
                .iter()
                .map(|(n, v)| ((*n).to_owned(), v.clone()))
                .collect();
            let ex = self
                .0
                .exchange(
                    req.method.as_str(),
                    &req.url,
                    &headers,
                    req.body.clone(),
                    req.limit,
                )
                .await?;
            Ok(WireResponse {
                status: ex.status,
                headers: ex.headers,
                body: ex.body,
            })
        })
    }
}

/// The largest list answer read: about 40,000 torrents at qBittorrent's
/// ~400 bytes each.
pub const MAX_LIST_BYTES: usize = 16 << 20;
/// Torrents kept from one list; the rest are counted in
/// [`Listing::over_cap`].
pub const MAX_LISTED: usize = 50_000;
/// Bytes in a listed name; a longer one makes the entry malformed.
pub const MAX_LISTED_NAME: usize = 4096;
/// Bytes in a listed state string.
const MAX_STATE: usize = 64;

/// What a client says a torrent is doing, in common terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorrentState {
    Downloading,
    Seeding,
    Paused,
    Checking,
    Queued,
    Error,
    /// A state this adapter doesn't map, or none.
    Other,
}

/// One torrent as the client listed it. Every field is the client's
/// claim; only [`hash`](Self::hash) is checked for form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The client's id for it: 40 lowercase hex digits (the v1 info-hash,
    /// or a v2-only torrent's truncated v2 hash).
    pub hash: String,
    /// Control characters dropped and trimmed; `None` when empty.
    pub name: Option<String>,
    /// The total size, when the client knows it.
    pub size: Option<u64>,
    pub state: TorrentState,
}

/// One list answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    /// In the client's order, each hash once.
    pub torrents: Vec<Listed>,
    /// Entries skipped as malformed: not an object, a missing or bad hash,
    /// a field of the wrong type, an over-long name, or a repeated hash.
    pub malformed: usize,
    /// Well-formed entries past [`MAX_LISTED`], dropped.
    pub over_cap: usize,
}

impl Listing {
    /// Adds one entry, or counts why not.
    pub(crate) fn push(&mut self, entry: Option<Listed>, seen: &mut HashSet<String>) {
        let Some(entry) = entry else {
            self.malformed += 1;
            return;
        };
        if self.torrents.len() >= MAX_LISTED {
            self.over_cap += 1;
        } else if !seen.insert(entry.hash.clone()) {
            self.malformed += 1;
        } else {
            self.torrents.push(entry);
        }
    }
}

/// A listed entry from its raw fields, or `None` when it's malformed.
pub(crate) fn listed(
    hash: &str,
    name: Option<&str>,
    size: Option<i64>,
    state: TorrentState,
) -> Option<Listed> {
    if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let name = match name {
        Some(n) if n.len() > MAX_LISTED_NAME => return None,
        Some(n) => {
            let clean: String = n.chars().filter(|c| !c.is_control()).collect();
            let clean = clean.trim();
            (!clean.is_empty()).then(|| clean.to_owned())
        }
        None => None,
    };
    Some(Listed {
        hash: hash.to_ascii_lowercase(),
        name,
        // Clients say -1 (or 0) before they have the metadata.
        size: size.and_then(|s| u64::try_from(s).ok()).filter(|&s| s > 0),
        state,
    })
}

/// A raw list entry of one client's shape.
pub(crate) trait RawEntry: DeserializeOwned {
    /// The entry, given the map key it sat under (Deluge's hash).
    fn into_listed(self, key: Option<&str>) -> Option<Listed>;
}

/// An entry that parsed as `R`, or anything else (skipped and counted).
#[derive(Deserialize)]
#[serde(untagged, bound = "R: RawEntry")]
enum Maybe<R> {
    Entry(R),
    Other(IgnoredAny),
}

/// A list (JSON array) or a map keyed by hash (JSON object) of `R`,
/// collected into a [`Listing`].
pub(crate) struct Bounded<R>(pub(crate) Listing, PhantomData<R>);

impl<'de, R: RawEntry> Deserialize<'de> for Bounded<R> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Bounded<R>, D::Error> {
        struct Collector<R>(PhantomData<R>);
        impl<'de, R: RawEntry> Visitor<'de> for Collector<R> {
            type Value = Bounded<R>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a torrent list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Bounded<R>, A::Error> {
                let (mut out, mut seen) = (Listing::default(), HashSet::new());
                while let Some(e) = seq.next_element::<Maybe<R>>()? {
                    out.push(
                        match e {
                            Maybe::Entry(r) => r.into_listed(None),
                            Maybe::Other(_) => None,
                        },
                        &mut seen,
                    );
                }
                Ok(Bounded(out, PhantomData))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Bounded<R>, A::Error> {
                let (mut out, mut seen) = (Listing::default(), HashSet::new());
                while let Some((key, e)) = map.next_entry::<String, Maybe<R>>()? {
                    out.push(
                        match e {
                            Maybe::Entry(r) => r.into_listed(Some(&key)),
                            Maybe::Other(_) => None,
                        },
                        &mut seen,
                    );
                }
                Ok(Bounded(out, PhantomData))
            }
        }
        d.deserialize_any(Collector(PhantomData))
    }
}

/// A state string, cut to a sane length (it's only compared).
pub(crate) fn state_text(s: Option<&str>) -> &str {
    s.filter(|s| s.len() <= MAX_STATE).unwrap_or("")
}

/// Parses one client's list answer, as its [`Lister`] does after the
/// HTTP exchange (Deluge's request-id check aside). Pure, for the fuzz
/// target and tests: the body is untrusted, and is refused past
/// [`MAX_LIST_BYTES`].
pub fn parse_list(kind: ClientKind, body: &[u8]) -> Result<Listing, ClientError> {
    if body.len() > MAX_LIST_BYTES {
        return Err(ClientError::Protocol("the torrent list is too large"));
    }
    match kind {
        ClientKind::QBittorrent => qbittorrent::parse_list(body),
        ClientKind::Transmission => transmission::parse_list(body),
        ClientKind::Deluge => deluge::parse_list(body).map(|(listing, _)| listing),
        ClientKind::RTorrent | ClientKind::Aria2 => Err(ClientError::Unsupported(Feature::List)),
    }
}

/// Whether `hash` is a torrent id an export may be asked for.
pub(crate) fn check_hash(hash: &str) -> Result<(), ClientError> {
    if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ClientError::InvalidRequest(
            "the info-hash isn't 40 hex digits",
        ))
    }
}

/// `application/x-www-form-urlencoded`, as a browser encodes a form.
pub(crate) fn form_encode(fields: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("http://form.invalid/").expect("a fixed URL parses");
    url.query_pairs_mut().extend_pairs(fields);
    url.query().unwrap_or("").to_owned()
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

/// The answer to an add that found every torrent already in the client.
pub(crate) fn already_there() -> ClientError {
    ClientError::AlreadyThere
}

/// Whether `e` is [`already_there`]'s answer: every torrent of the add was
/// in the client already, which a send counts as done (§9). Typed, so no
/// client's own refusal text can be mistaken for it.
pub fn is_already_there(e: &ClientError) -> bool {
    matches!(e, ClientError::AlreadyThere)
}

/// The error for an HTTP status an adapter has no meaning for.
pub(crate) fn rejected_status(status: u16, body: &[u8]) -> ClientError {
    ClientError::Rejected(format!("HTTP {status}: {}", client_reason(body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_all_duplicate_add_is_recognised_by_its_kind_not_its_wording() {
        assert!(is_already_there(&already_there()));
        // A client's own refusal reads the same, but isn't it.
        assert!(!is_already_there(&ClientError::Rejected(
            "the torrent is already in the client".into()
        )));
    }

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

    /// A scripted answer: status, headers and body.
    type Answer<'a> = (u16, &'a [(&'a str, &'a str)], &'a [u8]);

    /// A [`Wire`] that answers from a script and records each request.
    #[derive(Default)]
    struct Script {
        answers: std::sync::Mutex<std::collections::VecDeque<WireResponse>>,
        seen: std::sync::Mutex<Vec<WireRequest>>,
    }

    impl Script {
        fn new(answers: Vec<Answer<'_>>) -> Arc<Script> {
            Arc::new(Script {
                answers: std::sync::Mutex::new(
                    answers
                        .into_iter()
                        .map(|(status, headers, body)| WireResponse {
                            status,
                            headers: headers
                                .iter()
                                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                                .collect(),
                            body: body.to_vec(),
                        })
                        .collect(),
                ),
                seen: std::sync::Mutex::default(),
            })
        }

        fn seen(&self) -> Vec<WireRequest> {
            self.seen.lock().unwrap().clone()
        }

        fn header(req: &WireRequest, name: &str) -> Option<String> {
            req.headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        }
    }

    impl Wire for Script {
        fn exchange<'a>(
            &'a self,
            req: &'a WireRequest,
        ) -> BoxFuture<'a, Result<WireResponse, ClientError>> {
            self.seen.lock().unwrap().push(req.clone());
            let next = self.answers.lock().unwrap().pop_front();
            Box::pin(async move { next.ok_or(ClientError::Unreachable) })
        }
    }

    const H1: &str = "1f2e3d4c5b6a79880123456789abcdeffedcba98";
    const H2: &str = "00112233445566778899aabbccddeeff00112233";

    #[test]
    fn qbittorrent_lists_are_bounded_and_skip_malformed_entries() {
        let body = format!(
            r#"[{{"hash":"{}","name":"Big Buck Bunny\u0007","total_size":928670754,"size":1,"state":"uploading"}},
               {{"hash":"{H2}","name":"Sintel","size":-1,"state":"metaDL"}},
               {{"hash":"{H2}","name":"again"}},
               {{"hash":"xyz"}}, 7, {{"name":"no hash"}}, {{"hash":"{H1}","size":"big"}},
               {{"hash":"{}", "name":"{}"}}]"#,
            H1.to_uppercase(),
            "a".repeat(40),
            "n".repeat(MAX_LISTED_NAME + 1),
        );
        let l = parse_list(ClientKind::QBittorrent, body.as_bytes()).unwrap();
        assert_eq!(
            l.torrents,
            vec![
                Listed {
                    hash: H1.into(),
                    name: Some("Big Buck Bunny".into()),
                    size: Some(928_670_754),
                    state: TorrentState::Seeding,
                },
                Listed {
                    hash: H2.into(),
                    name: Some("Sintel".into()),
                    size: None,
                    state: TorrentState::Downloading,
                },
            ]
        );
        // The repeat, the bad hash, the number, no hash, a string size
        // (also a repeat), and the over-long name.
        assert_eq!((l.malformed, l.over_cap), (6, 0));
        for bad in [&b"{oops"[..], b"\"x\"", b"null"] {
            assert!(parse_list(ClientKind::QBittorrent, bad).is_err());
        }
        let huge = vec![b' '; MAX_LIST_BYTES + 1];
        assert!(parse_list(ClientKind::QBittorrent, &huge).is_err());
    }

    #[test]
    fn lists_stop_keeping_entries_at_the_cap() {
        let mut body = String::from("[");
        for i in 0..MAX_LISTED + 3 {
            body.push_str(&format!(r#"{{"hash":"{i:040x}"}},"#));
        }
        body.push_str("1]");
        let l = parse_list(ClientKind::QBittorrent, body.as_bytes()).unwrap();
        assert_eq!(
            (l.torrents.len(), l.over_cap, l.malformed),
            (MAX_LISTED, 3, 1)
        );
    }

    #[test]
    fn transmission_and_deluge_lists() {
        let body = format!(
            r#"{{"arguments":{{"torrents":[{{"hashString":"{H1}","name":"Big Buck Bunny","totalSize":5,"status":6,"error":0}},
                {{"hashString":"{H2}","status":0,"error":3}}, {{"hashString":1}}]}},"result":"success"}}"#
        );
        let l = parse_list(ClientKind::Transmission, body.as_bytes()).unwrap();
        assert_eq!(l.torrents.len(), 2);
        assert_eq!(l.torrents[0].state, TorrentState::Seeding);
        assert_eq!(l.torrents[1].state, TorrentState::Error);
        assert_eq!(l.malformed, 1);
        assert!(matches!(
            parse_list(ClientKind::Transmission, br#"{"result":"no"}"#),
            Err(ClientError::Rejected(_))
        ));
        assert!(parse_list(ClientKind::Transmission, br#"{"result":"success"}"#).is_err());

        let body = format!(
            r#"{{"id":3,"error":null,"result":{{"{H1}":{{"name":"Sintel","total_size":9,"state":"Paused"}},"short":{{}},"{H2}":[1]}}}}"#
        );
        let l = parse_list(ClientKind::Deluge, body.as_bytes()).unwrap();
        assert_eq!(
            l.torrents,
            vec![Listed {
                hash: H1.into(),
                name: Some("Sintel".into()),
                size: Some(9),
                state: TorrentState::Paused,
            }]
        );
        assert_eq!(l.malformed, 2);
        let refused = br#"{"id":1,"result":null,"error":{"code":5,"message":"echo hunter2"}}"#;
        let e = parse_list(ClientKind::Deluge, refused).unwrap_err();
        assert!(!e.to_string().contains("hunter2"), "{e}");
        for kind in [ClientKind::RTorrent, ClientKind::Aria2] {
            assert_eq!(
                parse_list(kind, b"[]"),
                Err(ClientError::Unsupported(Feature::List))
            );
        }
    }

    fn config(kind: ClientKind) -> ClientConfig {
        let url = "http://nas.lan:8080/".to_owned();
        match kind {
            ClientKind::QBittorrent => ClientConfig::QBittorrent {
                url,
                username: "admin".into(),
                password: Secret::new("hunter2"),
            },
            ClientKind::Transmission => ClientConfig::Transmission {
                url,
                auth: Some(BasicAuth {
                    username: "admin".into(),
                    password: Secret::new("hunter2"),
                }),
            },
            ClientKind::Deluge => ClientConfig::Deluge {
                url,
                password: Secret::new("hunter2"),
            },
            ClientKind::RTorrent => ClientConfig::RTorrent { url, auth: None },
            ClientKind::Aria2 => ClientConfig::Aria2 { url, secret: None },
        }
    }

    #[tokio::test]
    async fn qbittorrent_logs_in_lists_and_exports_logging_in_again_on_403() {
        let list = format!(r#"[{{"hash":"{H1}","name":"Big Buck Bunny"}}]"#);
        let wire = Script::new(vec![
            (200, &[("Set-Cookie", "SID=abc; HttpOnly")], b"Ok."),
            (200, &[], list.as_bytes()),
            // The session expired: one new login, one retry.
            (403, &[], b"Forbidden"),
            (200, &[("Set-Cookie", "SID=def")], b"Ok."),
            (200, &[], b"d4:infod4:name1:xee"),
            (404, &[], b"Not Found"),
        ]);
        let l = lister(&config(ClientKind::QBittorrent), wire.clone()).unwrap();
        assert!(l.can_export() && l.kind() == ClientKind::QBittorrent);
        assert_eq!(l.list().await.unwrap().torrents.len(), 1);
        assert_eq!(l.export(H1).await.unwrap(), b"d4:infod4:name1:xee");
        assert!(matches!(l.export(H2).await, Err(ClientError::Rejected(_))));
        assert_eq!(
            l.export("../../etc").await,
            Err(ClientError::InvalidRequest(
                "the info-hash isn't 40 hex digits"
            ))
        );
        let seen = wire.seen();
        let urls: Vec<_> = seen
            .iter()
            .map(|r| (r.method, r.url.as_str(), r.poll))
            .collect();
        let export = format!("http://nas.lan:8080/api/v2/torrents/export?hash={H1}");
        assert_eq!(
            urls,
            vec![
                (
                    WireMethod::Post,
                    "http://nas.lan:8080/api/v2/auth/login",
                    false
                ),
                (
                    WireMethod::Get,
                    "http://nas.lan:8080/api/v2/torrents/info",
                    true
                ),
                (WireMethod::Get, export.as_str(), false),
                (
                    WireMethod::Post,
                    "http://nas.lan:8080/api/v2/auth/login",
                    false
                ),
                (WireMethod::Get, export.as_str(), false),
                (
                    WireMethod::Get,
                    "http://nas.lan:8080/api/v2/torrents/export?hash=00112233445566778899aabbccddeeff00112233",
                    false
                ),
            ]
        );
        assert_eq!(seen[0].body, b"username=admin&password=hunter2");
        assert_eq!(
            Script::header(&seen[1], "cookie").as_deref(),
            Some("SID=abc")
        );
        assert_eq!(
            Script::header(&seen[4], "cookie").as_deref(),
            Some("SID=def")
        );
        assert_eq!(
            Script::header(&seen[1], "origin").as_deref(),
            Some("http://nas.lan:8080")
        );
        assert_eq!(seen[1].limit, MAX_LIST_BYTES);
        assert_eq!(seen[2].limit, MAX_TORRENT_FILE);
        let debug = format!("{l:?} {:?}", seen[1]);
        assert!(
            !debug.contains("hunter2") && !debug.contains("abc"),
            "{debug}"
        );
    }

    #[tokio::test]
    async fn qbittorrent_refusals_are_typed() {
        let wire = Script::new(vec![(200, &[], b"Fails.")]);
        let l = lister(&config(ClientKind::QBittorrent), wire).unwrap();
        assert_eq!(
            l.list().await,
            Err(ClientError::Auth(AuthFailure::BadCredentials))
        );
        let wire = Script::new(vec![
            (200, &[], b"Ok."),
            (403, &[], b""),
            (200, &[], b"Ok."),
            (403, &[], b""),
        ]);
        let l = lister(&config(ClientKind::QBittorrent), wire.clone()).unwrap();
        assert_eq!(
            l.list().await,
            Err(ClientError::Auth(AuthFailure::SessionRefused))
        );
        // Authentication bypassed for this address: no cookie is sent.
        assert!(Script::header(&wire.seen()[1], "cookie").is_none());
    }

    #[tokio::test]
    async fn transmission_answers_409_once_and_has_no_export() {
        let list = format!(
            r#"{{"arguments":{{"torrents":[{{"hashString":"{H1}","name":"Sintel","totalSize":3,"status":4}}]}},"result":"success"}}"#
        );
        let wire = Script::new(vec![
            (
                409,
                &[("X-Transmission-Session-Id", "tok3n")],
                b"<h1>409</h1>",
            ),
            (200, &[], list.as_bytes()),
            (409, &[("X-Transmission-Session-Id", "tok4n")], b""),
            (409, &[("X-Transmission-Session-Id", "tok5n")], b""),
        ]);
        let l = lister(&config(ClientKind::Transmission), wire.clone()).unwrap();
        assert!(!l.can_export());
        let got = l.list().await.unwrap();
        assert_eq!(got.torrents[0].state, TorrentState::Downloading);
        assert_eq!(
            l.list().await,
            Err(ClientError::Auth(AuthFailure::SessionRefused))
        );
        assert_eq!(
            l.export(H1).await,
            Err(ClientError::Unsupported(Feature::Export))
        );
        let seen = wire.seen();
        assert_eq!(seen[0].url, "http://nas.lan:8080/transmission/rpc");
        assert!(seen[0].poll && !seen[1].poll);
        assert_eq!(
            Script::header(&seen[1], "x-transmission-session-id").as_deref(),
            Some("tok3n")
        );
        assert_eq!(
            Script::header(&seen[0], "authorization").as_deref(),
            Some(format!("Basic {}", base64(b"admin:hunter2")).as_str())
        );
        let body: serde_json::Value = serde_json::from_slice(&seen[0].body).unwrap();
        assert_eq!(body["method"], "torrent-get");
        let wire = Script::new(vec![(401, &[], b"")]);
        let l = lister(&config(ClientKind::Transmission), wire).unwrap();
        assert_eq!(
            l.list().await,
            Err(ClientError::Auth(AuthFailure::BadCredentials))
        );
    }

    #[tokio::test]
    async fn deluge_logs_in_connects_and_logs_in_again_once() {
        let list = format!(
            r#"{{"id":6,"error":null,"result":{{"{H1}":{{"name":"Sintel","total_size":3,"state":"Seeding"}}}}}}"#
        );
        let wire = Script::new(vec![
            (
                200,
                &[("Set-Cookie", "_session_id=s1; Path=/json")],
                br#"{"id":1,"result":true,"error":null}"#,
            ),
            (200, &[], br#"{"id":2,"result":false,"error":null}"#),
            (
                200,
                &[],
                br#"{"id":3,"result":[["abc123","127.0.0.1",58846,"Online"]],"error":null}"#,
            ),
            (200, &[], br#"{"id":4,"result":null,"error":null}"#),
            (200, &[], br#"{"id":5,"result":true,"error":null}"#),
            (200, &[], list.as_bytes()),
            (
                200,
                &[],
                br#"{"id":7,"result":null,"error":{"code":1,"message":"Not authenticated"}}"#,
            ),
            (
                200,
                &[("Set-Cookie", "_session_id=s2")],
                br#"{"id":8,"result":true,"error":null}"#,
            ),
            (200, &[], br#"{"id":9,"result":true,"error":null}"#),
            (200, &[], br#"{"id":99,"result":{},"error":null}"#),
        ]);
        let l = lister(&config(ClientKind::Deluge), wire.clone()).unwrap();
        assert_eq!(l.list().await.unwrap().torrents[0].hash, H1);
        // The retry's answer has the wrong id: refused.
        assert_eq!(
            l.list().await,
            Err(ClientError::Protocol("the answer is for another request"))
        );
        let seen = wire.seen();
        let methods: Vec<String> = seen
            .iter()
            .map(|r| {
                serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["method"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            methods,
            [
                "auth.login",
                "web.connected",
                "web.get_hosts",
                "web.connect",
                "web.connected",
                "core.get_torrents_status",
                "core.get_torrents_status",
                "auth.login",
                "web.connected",
                "core.get_torrents_status"
            ]
        );
        let polls: Vec<usize> = (0..seen.len()).filter(|&i| seen[i].poll).collect();
        assert_eq!(polls, [5, 6]);
        assert_eq!(
            Script::header(&seen[9], "cookie").as_deref(),
            Some("_session_id=s2")
        );
        assert_eq!(seen[0].url, "http://nas.lan:8080/json");
        assert_eq!(
            l.export(H1).await,
            Err(ClientError::Unsupported(Feature::Export))
        );
    }

    #[test]
    fn rtorrent_and_aria2_have_no_lister() {
        for kind in [ClientKind::RTorrent, ClientKind::Aria2] {
            assert!(matches!(
                lister(&config(kind), Script::new(vec![])),
                Err(ClientError::Unsupported(Feature::List))
            ));
        }
        assert!(matches!(
            lister(
                &ClientConfig::Deluge {
                    url: "http://169.254.169.254/".into(),
                    password: Secret::new("x")
                },
                Script::new(vec![])
            ),
            Err(ClientError::Blocked(AddrClass::Metadata))
        ));
    }

    #[tokio::test]
    async fn adapters_without_listing_say_so() {
        let c = connect(config(ClientKind::Aria2)).unwrap();
        assert_eq!(c.list().await, Err(ClientError::Unsupported(Feature::List)));
        assert_eq!(
            c.export(H1).await,
            Err(ClientError::Unsupported(Feature::Export))
        );
        let c = connect(config(ClientKind::Transmission)).unwrap();
        assert_eq!(
            c.export(H1).await,
            Err(ClientError::Unsupported(Feature::Export))
        );
        assert_eq!(form_encode(&[("a b", "x&y=z")]), "a+b=x%26y%3Dz");
    }
}
