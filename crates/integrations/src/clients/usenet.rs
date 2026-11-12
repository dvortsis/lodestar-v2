//! Usenet download clients (§18.6, §18.10): "Send to SABnzbd/NZBGet".
//!
//! A usenet result is an NZB, not a magnet, so these adapters have their
//! own trait, [`UsenetClient`], next to the torrent [`DownloadClient`]
//! (`super::DownloadClient`). It does three things: add an NZB, test the
//! connection, and list categories. Nothing lists downloads or reads the
//! client's history.
//!
//! - **Bytes, never a URL.** An NZB link from an indexer carries the
//!   indexer's API key. Lodestar fetches the NZB itself at send time (UN-3)
//!   and uploads the bytes: SABnzbd `mode=addfile`, NZBGet `append` with
//!   base64 content. No adapter calls `addurl` or appends a URL, which would
//!   copy the indexer's key into the client's queue, history and logs.
//! - **Bounded.** An [`Nzb`] is at most [`MAX_NZB_BYTES`], checked when it's
//!   built, so an oversize one never reaches the network. Every answer is
//!   read within a per-call cap and parsed strictly.
//! - **The torrent adapters' rules hold** (`crates/integrations/README.md`):
//!   every request goes through a [`GuardedClient`](crate::GuardedClient)
//!   with [`NetPolicy::download_client`](crate::NetPolicy::download_client),
//!   redirects aren't followed, credentials are [`Secret`]s, and no URL,
//!   credential or raw body reaches an error, `Debug` or a log.

use std::fmt;

use super::{Category, ClientError, nzbget, sabnzbd};
use crate::BoxFuture;
use crate::net::redact;
use crate::secret::{Secret, Zeroizing};

/// The largest NZB a client is sent: 16 MiB, the `.torrent` cap
/// ([`MAX_TORRENT_FILE`](super::MAX_TORRENT_FILE)). An NZB lists every
/// article's message id (~100 bytes per 750 KB article), so 16 MiB covers a
/// release of about 100 GB. NZBGet's base64 makes the request ~21 MiB.
pub const MAX_NZB_BYTES: usize = 16 << 20;

/// The longest file name an NZB is sent under, in characters.
pub const MAX_NZB_NAME: usize = 200;

/// How far into an NZB its `<nzb` root element is looked for.
const NZB_SNIFF: usize = 64 << 10;

/// Which Usenet client an adapter talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsenetKind {
    Sabnzbd,
    Nzbget,
}

impl UsenetKind {
    /// The client's own name, for "Send to SABnzbd" (§10.14).
    pub fn display_name(self) -> &'static str {
        match self {
            UsenetKind::Sabnzbd => "SABnzbd",
            UsenetKind::Nzbget => "NZBGet",
        }
    }
}

/// An NZB file to send: its bytes and the file name the client shows.
#[derive(Clone, PartialEq, Eq)]
pub struct Nzb {
    bytes: Vec<u8>,
    file_name: String,
}

impl Nzb {
    /// Checks `bytes` and cleans `file_name` (a release title, say).
    ///
    /// Refuses an empty NZB, one over [`MAX_NZB_BYTES`], and one with no
    /// `<nzb` element in its first 64 KiB (an indexer's HTML error page,
    /// for example). The file name loses any path, quotes and control
    /// characters, is cut to [`MAX_NZB_NAME`] characters and always ends in
    /// `.nzb`, so a client never reads it as an archive.
    pub fn new(bytes: Vec<u8>, file_name: &str) -> Result<Nzb, ClientError> {
        if bytes.is_empty() {
            return Err(ClientError::InvalidRequest("the NZB is empty"));
        }
        if bytes.len() > MAX_NZB_BYTES {
            return Err(ClientError::InvalidRequest("the NZB is larger than 16 MiB"));
        }
        let head = &bytes[..bytes.len().min(NZB_SNIFF)];
        if !head.windows(4).any(|w| w.eq_ignore_ascii_case(b"<nzb")) {
            return Err(ClientError::InvalidRequest("this isn't an NZB file"));
        }
        Ok(Nzb {
            bytes,
            file_name: nzb_file_name(file_name),
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The cleaned file name, ending in `.nzb`.
    pub fn file_name(&self) -> &str {
        &self.file_name
    }
}

impl fmt::Debug for Nzb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Nzb")
            .field("len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

fn nzb_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || c == '"' { ' ' } else { c })
        .collect();
    let mut stem = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if stem.len() >= 4 && stem.is_char_boundary(stem.len() - 4) {
        let (head, ext) = stem.split_at(stem.len() - 4);
        if ext.eq_ignore_ascii_case(".nzb") {
            stem = head.to_owned();
        }
    }
    let stem: String = stem
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .chars()
        .take(MAX_NZB_NAME - 4)
        .collect();
    let stem = stem.trim_end();
    if stem.is_empty() {
        "lodestar.nzb".to_owned()
    } else {
        format!("{stem}.nzb")
    }
}

/// A job's priority in the client's queue. `None` in [`NzbOptions`] leaves
/// the client's default (the category's, in SABnzbd).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NzbPriority {
    Low,
    Normal,
    High,
    /// Download now, even while the queue is paused.
    Force,
}

/// How to add an NZB.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NzbOptions {
    /// A category the client knows ([`UsenetClient::categories`]).
    pub category: Option<String>,
    pub priority: Option<NzbPriority>,
    /// Add without starting. SABnzbd's "paused" is a priority, so there a
    /// paused add keeps the client's default priority.
    pub paused: bool,
}

impl NzbOptions {
    /// Refuses an empty or over-long category, or one with control
    /// characters, before anything is sent.
    pub(crate) fn check(&self) -> Result<(), ClientError> {
        if let Some(category) = &self.category
            && (category.trim().is_empty()
                || category.len() > 256
                || category
                    .chars()
                    .any(|c| c.is_control() || c == '"' || c == '\\'))
        {
            // NZBGet 24.x's request parser ends a string early at two
            // adjacent escapes (fixed in 26), so `"` and `\` are refused.
            return Err(ClientError::InvalidRequest(
                "the category is empty, too long, or has quotes, backslashes or control characters",
            ));
        }
        Ok(())
    }
}

/// The client's id for an added job: SABnzbd's `nzo_id`
/// (`SABnzbd_nzo_…`), or NZBGet's `NZBID` in decimal. Checked before it's
/// returned: short, ASCII, no control characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(String);

impl JobId {
    pub(crate) fn new(id: String) -> JobId {
        JobId(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the configured credentials may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Everything this adapter does.
    Full,
    /// Add NZBs only: SABnzbd's NZB key. Categories can't be listed.
    AddOnly,
    /// The client doesn't say (NZBGet: its add-only user is refused only
    /// when it lists categories).
    Unknown,
}

/// What [`UsenetClient::test`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsenetInfo {
    pub kind: UsenetKind,
    /// The client's version, as it reports it (`4.5.5`, `24.8`).
    pub version: String,
    pub access: Access,
}

/// A Usenet download client. Every method is cancel-safe to drop.
pub trait UsenetClient: Send + Sync + fmt::Debug {
    fn kind(&self) -> UsenetKind;

    /// Checks the address and the credentials and returns the version,
    /// for the settings screen.
    fn test(&self) -> BoxFuture<'_, Result<UsenetInfo, ClientError>>;

    /// Uploads the NZB's bytes and returns the client's job id.
    fn add_nzb<'a>(
        &'a self,
        nzb: &'a Nzb,
        opts: &'a NzbOptions,
    ) -> BoxFuture<'a, Result<JobId, ClientError>>;

    /// The client's categories, sorted by name (no save paths).
    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>, ClientError>>;
}

/// A configured Usenet client, as Settings stores it (credentials already
/// opened from the [`SecretStore`](crate::SecretStore)). Its `Debug` shows
/// the URL's scheme, host and port only.
#[derive(Clone)]
pub enum UsenetConfig {
    /// The web interface's address (`http://host:8080/`, or
    /// `http://host:8080/sabnzbd/`); `api_key` is the API key, or the NZB
    /// key, which can add but not list categories.
    Sabnzbd { url: String, api_key: Secret },
    /// The web interface's address (`http://host:6789/`), or the full
    /// `…/jsonrpc` URL. Empty credentials send no `Authorization`.
    Nzbget {
        url: String,
        username: Secret,
        password: Secret,
    },
}

impl fmt::Debug for UsenetConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, url) = match self {
            UsenetConfig::Sabnzbd { url, .. } => (UsenetKind::Sabnzbd, url),
            UsenetConfig::Nzbget { url, .. } => (UsenetKind::Nzbget, url),
        };
        let url = match reqwest::Url::parse(url) {
            Ok(u) => redact(&u),
            Err(_) => "(not a URL)".to_owned(),
        };
        f.debug_struct("UsenetConfig")
            .field("kind", &kind)
            .field("url", &url)
            .finish_non_exhaustive()
    }
}

/// An adapter for `config`. Fails when the URL or a credential is refused,
/// so a bad setting fails when it's saved.
pub fn connect_usenet(config: UsenetConfig) -> Result<Box<dyn UsenetClient>, ClientError> {
    match config {
        UsenetConfig::Sabnzbd { url, api_key } => {
            Ok(Box::new(sabnzbd::Sabnzbd::new(&url, api_key)?))
        }
        UsenetConfig::Nzbget {
            url,
            username,
            password,
        } => Ok(Box::new(nzbget::Nzbget::new(&url, username, password)?)),
    }
}

/// A version string from a client: one short printable line, or `None`.
pub(crate) fn version_text(raw: &str) -> Option<String> {
    let v = raw.trim();
    (!v.is_empty() && v.len() <= 64 && !v.chars().any(char::is_control)).then(|| v.to_owned())
}

/// The shortest credential [`parse_guarded`] looks for: a shorter one
/// would match ordinary text and withhold reasons for nothing.
const MIN_GUARDED: usize = 4;

/// The reason given instead of one that echoed a credential.
pub(crate) const WITHHELD: &str =
    "the client refused; its reason was withheld because it repeated a credential";

/// Parses an answer as it was received, then keeps every credential out of
/// the error's reason. Only a [`ClientError::Rejected`] carries the client's
/// text, so a success or any other error is returned untouched.
///
/// When the answer or the reason holds a credential (a reverse proxy's
/// error page that echoes the request, say), the answer is parsed again
/// with every copy replaced by `***`, so a copy cut short by the 200-character
/// limit can't leak either. If that reason still holds one, [`WITHHELD`] is
/// given instead. The scrubbed copy is zeroized.
pub(crate) fn parse_guarded<T>(
    body: &[u8],
    secrets: &[&str],
    parse: impl Fn(&[u8]) -> Result<T, ClientError>,
) -> Result<T, ClientError> {
    let result = parse(body);
    let secrets: Vec<&[u8]> = secrets
        .iter()
        .map(|s| s.as_bytes())
        .filter(|s| s.len() >= MIN_GUARDED)
        .collect();
    let leaks = |text: &[u8]| {
        secrets
            .iter()
            .any(|s| text.windows(s.len()).any(|w| w == *s))
    };
    let leaked = match &result {
        Err(ClientError::Rejected(why)) => leaks(body) || leaks(why.as_bytes()),
        _ => false,
    };
    if !leaked {
        return result;
    }
    let clean = Zeroizing::new(replace_all(body, &secrets));
    match parse(&clean) {
        Err(ClientError::Rejected(why)) if !leaks(why.as_bytes()) => {
            Err(ClientError::Rejected(why))
        }
        _ => Err(ClientError::Rejected(WITHHELD.to_owned())),
    }
}

fn replace_all(body: &[u8], secrets: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    'scan: while i < body.len() {
        for s in secrets {
            if body[i..].starts_with(s) {
                out.extend_from_slice(b"***");
                i += s.len();
                continue 'scan;
            }
        }
        out.push(body[i]);
        i += 1;
    }
    out
}

/// The error for a redirect: never followed, since the target would get
/// the credentials in the body.
pub(crate) fn redirected() -> ClientError {
    ClientError::Protocol("the client redirected")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NZB: &[u8] =
        b"<?xml version=\"1.0\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\"></nzb>";

    #[test]
    fn an_nzb_is_checked() {
        assert!(Nzb::new(NZB.to_vec(), "x").is_ok());
        assert_eq!(
            Nzb::new(Vec::new(), "x"),
            Err(ClientError::InvalidRequest("the NZB is empty"))
        );
        assert_eq!(
            Nzb::new(b"<html>Unauthorized</html>".to_vec(), "x"),
            Err(ClientError::InvalidRequest("this isn't an NZB file"))
        );
        let mut big = NZB.to_vec();
        big.resize(MAX_NZB_BYTES, b' ');
        assert!(Nzb::new(big.clone(), "x").is_ok());
        big.push(b' ');
        assert_eq!(
            Nzb::new(big, "x"),
            Err(ClientError::InvalidRequest("the NZB is larger than 16 MiB"))
        );
    }

    #[test]
    fn file_names_are_cleaned() {
        let name = |raw: &str| Nzb::new(NZB.to_vec(), raw).unwrap().file_name().to_owned();
        assert_eq!(
            name("Some.Release.2024.1080p"),
            "Some.Release.2024.1080p.nzb"
        );
        assert_eq!(name("Some.Release.NZB"), "Some.Release.nzb");
        assert_eq!(name("../../etc/passwd"), "passwd.nzb");
        assert_eq!(name("C:\\x\\y.zip"), "y.zip.nzb");
        assert_eq!(name("a\"b\nc\u{7}d"), "a b c d.nzb");
        assert_eq!(name(""), "lodestar.nzb");
        assert_eq!(name(" .nzb "), "lodestar.nzb");
        assert_eq!(name("..."), "lodestar.nzb");
        let long = name(&"é".repeat(500));
        assert!(long.ends_with(".nzb"));
        assert!(long.chars().count() <= MAX_NZB_NAME);
    }

    #[test]
    fn debug_shows_no_content() {
        let nzb = Nzb::new(NZB.to_vec(), "Private.Title").unwrap();
        let d = format!("{nzb:?}");
        assert!(!d.contains("Private") && !d.contains("nzb xmlns"), "{d}");
        let config = UsenetConfig::Sabnzbd {
            url: "http://sab.lan:8080/sabnzbd/?apikey=zzz".into(),
            api_key: Secret::new("0123456789abcdef"),
        };
        let d = format!("{config:?}");
        assert!(!d.contains("zzz") && !d.contains("0123456789abcdef"), "{d}");
        assert!(d.contains("http://sab.lan:8080/…"), "{d}");
    }

    fn reason(body: &[u8]) -> Result<u32, ClientError> {
        match std::str::from_utf8(body).ok().and_then(|t| t.parse().ok()) {
            Some(n) => Ok(n),
            None => Err(ClientError::Rejected(super::super::client_reason(body))),
        }
    }

    #[test]
    fn a_credential_never_reaches_a_reason() {
        let key = "SECRETKEY";
        // A success is parsed from the answer as received, even when it
        // holds a credential.
        assert_eq!(parse_guarded(b"912024", &["2024"], reason), Ok(912024));
        assert_eq!(
            parse_guarded(b"echo apikey=SECRETKEY&x=SECRETKEY", &[key], reason),
            Err(ClientError::Rejected("echo apikey=***&x=***".into()))
        );
        // A copy the 200-character cut would split is scrubbed before the cut.
        let long = format!("{}{key}", "y".repeat(195));
        let Err(ClientError::Rejected(why)) = parse_guarded(long.as_bytes(), &[key], reason) else {
            panic!()
        };
        assert!(!why.contains("SECR"), "{why}");
        // Whitespace the reason collapses can't smuggle a spaced password.
        assert_eq!(
            parse_guarded(b"pass\tword here", &["pass word"], reason),
            Err(ClientError::Rejected(WITHHELD.into()))
        );
        // A short credential would match ordinary text, so it isn't looked for.
        assert_eq!(
            parse_guarded(b"abc", &["abc"], reason),
            Err(ClientError::Rejected("abc".into()))
        );
    }

    #[test]
    fn categories_are_checked() {
        let opts = |c: &str| NzbOptions {
            category: Some(c.into()),
            ..NzbOptions::default()
        };
        assert!(opts("tv").check().is_ok());
        assert!(opts(" ").check().is_err());
        assert!(opts("a\nb").check().is_err());
        assert!(opts("a\"b").check().is_err());
        assert!(opts("a\\b").check().is_err());
        assert!(opts(&"x".repeat(257)).check().is_err());
        assert!(NzbOptions::default().check().is_ok());
    }
}
