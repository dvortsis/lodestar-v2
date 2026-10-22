//! Download clients (§9): one-click and bulk send, "Send selected files"
//! (§10.5).
//!
//! Each client is an adapter in `src/clients/<name>.rs` implementing
//! [`DownloadClient`]. [`qbittorrent`] is the reference; the README lists
//! what a new adapter must do and test.

pub mod magnet;
pub mod qbittorrent;

use std::fmt;
use std::time::Duration;

use crate::BoxFuture;
use crate::net::{AddrClass, NetError};
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
    TorrentFile,
}

impl Feature {
    fn describe(self) -> &'static str {
        match self {
            Feature::Category => "categories or labels",
            Feature::SavePath => "a save path",
            Feature::Paused => "adding paused",
            Feature::FileSelection => "selecting files",
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
    /// Lowercase hex, 40 characters.
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

/// A configured client, as Settings → Integrations stores it (credentials
/// already opened from the [`SecretStore`](crate::SecretStore)).
#[derive(Debug, Clone)]
pub enum ClientConfig {
    QBittorrent {
        url: String,
        username: String,
        password: Secret,
    },
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Capabilities = Capabilities {
        category: true,
        save_path: true,
        paused: true,
        file_selection: true,
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
