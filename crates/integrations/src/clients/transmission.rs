//! Transmission, RPC over HTTP POST with JSON bodies (`rpc-spec.md`).
//!
//! - **Session id:** every request carries `X-Transmission-Session-Id`, the
//!   daemon's CSRF token. A `409 Conflict` carries a new id in that header;
//!   the request is sent once more with it, never in a loop. A second 409
//!   is [`AuthFailure::SessionRefused`]. The 409 body repeats the id, so it
//!   is never read into an error.
//! - **Authentication** is optional HTTP basic auth (`reqwest` marks the
//!   header sensitive). `401` is a wrong password, `403` an address refused
//!   by `rpc-whitelist` or banned after failed logins (Transmission answers
//!   both with the same bare page), and `421` a host name refused by
//!   `rpc-host-whitelist`.
//! - **Adding** is `torrent-add` with `filename` (a magnet) or `metainfo`
//!   (base64 `.torrent`), plus `paused` and `download-dir`. Labels go in
//!   `torrent-add` from RPC 17 (4.0) and through `torrent-set` on RPC 16
//!   (3.00); older daemons have no labels, so the add goes ahead without
//!   one. `torrent-duplicate` means the torrent was already there.
//! - **Selected files** (a `.torrent` only): added paused; `torrent-get`
//!   polls the file list until the torrent is registered, `torrent-set`
//!   marks the rest `files-unwanted`, and `torrent-start` starts it unless
//!   the user asked for paused. A paused magnet fetches no metadata, so a
//!   magnet's files can't be selected before it starts: refused as
//!   [`Feature::MagnetFileSelection`] before any request. File counts past
//!   [`MAX_FILES`](super::MAX_FILES) are refused.
//! - **Categories** are the labels in use on the daemon's torrents: it keeps
//!   no list of its own. Before RPC 16 there are none, so `Unsupported`.
//!
//! Recorded against Transmission 4.0.5 (RPC 17); the fixtures are in
//! `tests/fixtures/transmission/`.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderValue};
use reqwest::{Method, Url};
use serde::Deserialize;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::qbittorrent::upload_timeout;
use super::{
    AddOptions, AddOutcome, AuthFailure, BasicAuth, CLIENT_TIMEOUT, Capabilities, Category,
    ClientError, ClientInfo, ClientKind, DownloadClient, Feature, FileCount, METADATA_WAIT, Magnet,
    POLL_INTERVAL, Selection, TorrentFile, already_there, base64, category_name, check_file_count,
    check_magnets, check_request, client_reason, partial, poll, rejected_status, skipped_files,
};
use crate::BoxFuture;
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited, redact};
use crate::secret::Secret;

const CAPABILITIES: Capabilities = Capabilities {
    category: true,
    save_path: true,
    paused: true,
    file_selection: true,
    // A paused magnet fetches no metadata, so its files can't be selected
    // before it starts (§16).
    magnet_file_selection: false,
    torrent_file: true,
    batch: false,
};

const SESSION_HEADER: &str = "x-transmission-session-id";
/// Most answers are a few hundred bytes.
const MAX_RESPONSE: usize = 1 << 20;
/// A file list.
const MAX_LIST: usize = 64 << 20;
/// Every torrent's labels.
const MAX_LABELS: usize = 16 << 20;
/// The first RPC version with labels (`torrent-set`), and the first that
/// takes them in `torrent-add`.
const LABELS_SET: u32 = 16;
const LABELS_ADD: u32 = 17;

pub struct Transmission {
    endpoint: Url,
    auth: Option<BasicAuth>,
    http: GuardedClient,
    /// The daemon's current session id; `None` until the first 409.
    session: Mutex<Option<Secret>>,
    /// `rpc-version`, once `session-get` has been asked.
    rpc_version: Mutex<Option<u32>>,
    metadata_wait: Duration,
    poll_interval: Duration,
    timeout: Duration,
}

impl fmt::Debug for Transmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transmission")
            .field("url", &redact(&self.endpoint))
            .finish_non_exhaustive()
    }
}

/// An answer's `result`, read first; the arguments are skipped.
#[derive(Deserialize)]
struct Status {
    result: String,
}

/// A successful answer's arguments.
#[derive(Deserialize)]
struct Answer<T> {
    arguments: T,
}

#[derive(Deserialize)]
struct AddAnswer {
    #[serde(rename = "torrent-added")]
    added: Option<Hashed>,
    #[serde(rename = "torrent-duplicate")]
    duplicate: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct Hashed {
    #[serde(rename = "hashString")]
    hash: String,
}

#[derive(Deserialize)]
struct Torrents<T> {
    torrents: Vec<T>,
}

#[derive(Deserialize)]
struct FilesOf {
    files: FileCount,
}

#[derive(Deserialize)]
struct LabelsOf {
    #[serde(default)]
    labels: Vec<String>,
}

#[derive(Deserialize)]
struct SessionInfo {
    version: String,
    #[serde(rename = "rpc-version")]
    rpc_version: u32,
}

/// What `torrent-add` did with one torrent.
enum Added {
    New(String),
    Duplicate,
}

impl Transmission {
    /// An adapter for the daemon at `url`: its address
    /// (`http://localhost:9091/`, `/transmission/rpc` is added) or the full
    /// RPC URL (any path ending in `/rpc`, for a reverse proxy).
    pub fn new(url: &str, auth: Option<BasicAuth>) -> Result<Transmission, ClientError> {
        let http = GuardedClient::new(NetPolicy::download_client(), CLIENT_TIMEOUT)?;
        Transmission::with_client(url, auth, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(
        url: &str,
        auth: Option<BasicAuth>,
        http: GuardedClient,
    ) -> Result<Transmission, ClientError> {
        let mut endpoint = parse_url(url, http.policy())?;
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        if !endpoint.path().ends_with("/rpc") {
            let path = format!("{}/transmission/rpc", endpoint.path().trim_end_matches('/'));
            endpoint.set_path(&path);
        }
        Ok(Transmission {
            endpoint,
            auth,
            http,
            session: Mutex::new(None),
            rpc_version: Mutex::new(None),
            metadata_wait: METADATA_WAIT,
            poll_interval: POLL_INTERVAL,
            timeout: CLIENT_TIMEOUT,
        })
    }

    /// The timeout for one request (default [`CLIENT_TIMEOUT`]).
    pub fn with_timeout(mut self, timeout: Duration) -> Transmission {
        self.timeout = timeout;
        self
    }

    /// How long to wait for a magnet's metadata before leaving a file
    /// selection [`Selection::Pending`], and how often to look.
    pub fn with_metadata_wait(mut self, wait: Duration, poll: Duration) -> Transmission {
        self.metadata_wait = wait;
        self.poll_interval = poll;
        self
    }

    /// One RPC call: `arguments` of a `success`, the daemon's `result` text
    /// as a rejection otherwise.
    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        arguments: Value,
        limit: usize,
    ) -> Result<T, ClientError> {
        self.call_timeout(method, arguments, limit, self.timeout)
            .await
    }

    /// [`call`](Self::call) with its own timeout (uploads get more).
    async fn call_timeout<T: DeserializeOwned>(
        &self,
        method: &str,
        arguments: Value,
        limit: usize,
        timeout: Duration,
    ) -> Result<T, ClientError> {
        let body = serde_json::to_vec(&json!({ "method": method, "arguments": arguments }))
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let mut retried = false;
        loop {
            let session = self.session.lock().await.clone();
            let mut rb = self
                .http
                .request(Method::POST, self.endpoint.clone())
                .header(CONTENT_TYPE, "application/json")
                .timeout(timeout)
                .body(body.clone());
            if let Some(auth) = &self.auth {
                rb = rb.basic_auth(&auth.username, Some(auth.password.expose()));
            }
            if let Some(id) = &session {
                let mut value = HeaderValue::from_str(id.expose())
                    .map_err(|_| ClientError::Protocol("the session id"))?;
                value.set_sensitive(true);
                rb = rb.header(SESSION_HEADER, value);
            }
            let req = rb
                .build()
                .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
            let resp = self.http.send(req).await?;
            let status = resp.status().as_u16();
            tracing::debug!(method, status, "Transmission request");
            match status {
                409 => {
                    let id = resp
                        .headers()
                        .get(SESSION_HEADER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(session_id)
                        .ok_or(ClientError::Protocol("a 409 without a usable session id"))?;
                    if retried {
                        return Err(ClientError::Auth(AuthFailure::SessionRefused));
                    }
                    retried = true;
                    *self.session.lock().await = Some(id);
                    continue;
                }
                401 => return Err(ClientError::Auth(AuthFailure::BadCredentials)),
                403 => return Err(ClientError::Auth(AuthFailure::AddressRefused)),
                421 => return Err(ClientError::Auth(AuthFailure::OriginRefused)),
                _ => {}
            }
            let bytes = read_limited(resp, limit).await?;
            if status != 200 {
                return Err(rejected_status(status, &bytes));
            }
            let status: Status = serde_json::from_slice(&bytes)
                .map_err(|_| ClientError::Protocol("the answer isn't Transmission's JSON"))?;
            if status.result != "success" {
                return Err(ClientError::Rejected(client_reason(
                    status.result.as_bytes(),
                )));
            }
            let answer: Answer<T> = serde_json::from_slice(&bytes)
                .map_err(|_| ClientError::Protocol("the answer's arguments"))?;
            return Ok(answer.arguments);
        }
    }

    async fn session_info(&self) -> Result<SessionInfo, ClientError> {
        let info: SessionInfo = self
            .call(
                "session-get",
                json!({ "fields": ["version", "rpc-version"] }),
                MAX_RESPONSE,
            )
            .await?;
        *self.rpc_version.lock().await = Some(info.rpc_version);
        Ok(info)
    }

    async fn rpc_version(&self) -> Result<u32, ClientError> {
        if let Some(v) = *self.rpc_version.lock().await {
            return Ok(v);
        }
        Ok(self.session_info().await?.rpc_version)
    }

    /// One `torrent-add`; `source` is the `filename` or `metainfo` field.
    async fn add_one(
        &self,
        source: (&str, String),
        opts: &AddOptions,
        selecting: bool,
    ) -> Result<Added, ClientError> {
        let timeout = match source.0 {
            "metainfo" => upload_timeout(self.timeout, source.1.len()),
            _ => self.timeout,
        };
        let mut args = serde_json::Map::new();
        args.insert(source.0.into(), source.1.into());
        if opts.paused || selecting {
            args.insert("paused".into(), true.into());
        }
        if let Some(path) = &opts.save_path {
            args.insert("download-dir".into(), path.as_str().into());
        }
        let label_later = match &opts.category {
            Some(label) => {
                let version = self.rpc_version().await?;
                if version >= LABELS_ADD {
                    args.insert("labels".into(), json!([label]));
                    None
                } else if version >= LABELS_SET {
                    Some(label)
                } else {
                    tracing::debug!(version, "Transmission has no labels; added without one");
                    None
                }
            }
            None => None,
        };
        let answer: AddAnswer = self
            .call_timeout("torrent-add", Value::Object(args), MAX_RESPONSE, timeout)
            .await?;
        if answer.duplicate.is_some() {
            return Ok(Added::Duplicate);
        }
        let hash = answer
            .added
            .map(|t| t.hash)
            .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or(ClientError::Protocol(
                "torrent-add didn't say what it added",
            ))?
            .to_ascii_lowercase();
        if let Some(label) = label_later {
            // The torrent is in; a refused label doesn't undo that.
            let set: Result<IgnoredAny, _> = self
                .call(
                    "torrent-set",
                    json!({ "ids": [hash], "labels": [label] }),
                    MAX_RESPONSE,
                )
                .await;
            if let Err(e) = set {
                tracing::debug!(error = %e, "Transmission added the torrent but not its label");
            }
        }
        Ok(Added::New(hash))
    }

    /// How many files the torrent has, once its metadata is there.
    async fn file_count(&self, hash: &str) -> Result<Option<usize>, ClientError> {
        let answer: Torrents<FilesOf> = self
            .call(
                "torrent-get",
                json!({ "ids": [hash], "fields": ["files"] }),
                MAX_LIST,
            )
            .await?;
        let Some(torrent) = answer.torrents.first() else {
            return Ok(None);
        };
        let count = check_file_count(torrent.files.0)?;
        Ok((count > 0).then_some(count))
    }

    /// Applies a file selection to `hash`, which was added paused.
    async fn select_files(
        &self,
        hash: &str,
        selected: &[u32],
        start: bool,
    ) -> Result<Selection, ClientError> {
        let Some(count) = poll(self.metadata_wait, self.poll_interval, || {
            self.file_count(hash)
        })
        .await?
        else {
            tracing::debug!("Transmission metadata didn't arrive; left paused");
            return Ok(Selection::Pending);
        };
        let skipped = skipped_files(selected, count)?;
        if !skipped.is_empty() {
            self.call::<IgnoredAny>(
                "torrent-set",
                json!({ "ids": [hash], "files-unwanted": skipped }),
                MAX_RESPONSE,
            )
            .await?;
        }
        if start {
            self.call::<IgnoredAny>("torrent-start", json!({ "ids": [hash] }), MAX_RESPONSE)
                .await?;
        }
        Ok(Selection::Applied)
    }

    async fn finish(
        &self,
        added: Added,
        selection: Option<Vec<u32>>,
        opts: &AddOptions,
    ) -> Result<AddOutcome, ClientError> {
        let Added::New(hash) = added else {
            return Err(already_there());
        };
        let selection = match selection {
            Some(selected) => self.select_files(&hash, &selected, !opts.paused).await?,
            None => Selection::None,
        };
        Ok(AddOutcome { sent: 1, selection })
    }
}

impl DownloadClient for Transmission {
    fn kind(&self) -> ClientKind {
        ClientKind::Transmission
    }

    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }

    fn test(&self) -> BoxFuture<'_, Result<ClientInfo, ClientError>> {
        Box::pin(async move {
            let info = self.session_info().await?;
            Ok(ClientInfo {
                kind: ClientKind::Transmission,
                version: version_text(&info.version)?,
                api_version: Some(info.rpc_version.to_string()),
            })
        })
    }

    fn add_magnets<'a>(
        &'a self,
        magnets: &'a [Magnet],
        opts: &'a AddOptions,
    ) -> BoxFuture<'a, Result<AddOutcome, ClientError>> {
        Box::pin(async move {
            let selection = check_magnets(CAPABILITIES, opts, magnets.len())?;
            if let [magnet] = magnets {
                let added = self
                    .add_one(
                        ("filename", magnet.as_str().to_owned()),
                        opts,
                        selection.is_some(),
                    )
                    .await?;
                return self.finish(added, selection, opts).await;
            }
            // Bulk: no batch call, so one add each. A duplicate is already
            // where the user wants it; only all-duplicates is refused. A
            // failure part-way says how many went in.
            let mut new = 0;
            for (done, magnet) in magnets.iter().enumerate() {
                match self
                    .add_one(("filename", magnet.as_str().to_owned()), opts, false)
                    .await
                {
                    Ok(Added::New(_)) => new += 1,
                    Ok(Added::Duplicate) => {}
                    Err(e) => return Err(partial(done, e)),
                }
            }
            if new == 0 {
                return Err(already_there());
            }
            Ok(AddOutcome {
                sent: magnets.len(),
                selection: Selection::None,
            })
        })
    }

    fn add_torrent_file<'a>(
        &'a self,
        file: &'a TorrentFile,
        opts: &'a AddOptions,
    ) -> BoxFuture<'a, Result<AddOutcome, ClientError>> {
        Box::pin(async move {
            let selection = check_request(CAPABILITIES, opts, 1)?;
            let added = self
                .add_one(("metainfo", base64(&file.bytes)), opts, selection.is_some())
                .await?;
            self.finish(added, selection, opts).await
        })
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>, ClientError>> {
        Box::pin(async move {
            if self.rpc_version().await? < LABELS_SET {
                return Err(ClientError::Unsupported(Feature::Category));
            }
            let answer: Torrents<LabelsOf> = self
                .call("torrent-get", json!({ "fields": ["labels"] }), MAX_LABELS)
                .await?;
            let labels: BTreeSet<String> = answer
                .torrents
                .iter()
                .flat_map(|t| &t.labels)
                .filter_map(|l| category_name(l))
                .collect();
            Ok(labels
                .into_iter()
                .map(|name| Category {
                    name,
                    save_path: None,
                })
                .collect())
        })
    }
}

/// A session id from a 409: printable ASCII token characters, so it can't
/// smuggle anything into the next request's headers.
fn session_id(value: &str) -> Option<Secret> {
    let ok = !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.+/=".contains(&b));
    ok.then(|| Secret::new(value))
}

/// The version as Transmission reports it (`4.0.5 (a6fe2a64aa)`): one short
/// printable line.
fn version_text(v: &str) -> Result<String, ClientError> {
    let v = v.trim();
    if v.is_empty() || v.len() > 64 || v.chars().any(char::is_control) {
        return Err(ClientError::Protocol("the version isn't a version"));
    }
    Ok(v.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_get_the_rpc_path() {
        let t = |u: &str| Transmission::new(u, None).unwrap().endpoint.to_string();
        assert_eq!(
            t("http://10.0.0.5:9091"),
            "http://10.0.0.5:9091/transmission/rpc"
        );
        assert_eq!(
            t("http://10.0.0.5:9091/"),
            "http://10.0.0.5:9091/transmission/rpc"
        );
        assert_eq!(
            t("http://nas/torrents/?x#y"),
            "http://nas/torrents/transmission/rpc"
        );
        assert_eq!(t("https://nas/tr/rpc"), "https://nas/tr/rpc");
        assert!(matches!(
            Transmission::new("http://169.254.169.254/", None),
            Err(ClientError::Blocked(_))
        ));
        let debug = format!("{:?}", Transmission::new("http://u:p@nas:9091/x", None));
        assert!(!debug.contains("u:p"), "{debug}");
    }

    #[test]
    fn session_ids_are_tokens() {
        assert_eq!(
            session_id("W4jID2xVA70rBnUAXQAdROUAhZxK6ttDnirkOEKMDzJTqK7K")
                .map(|s| s.expose().len()),
            Some(48)
        );
        for bad in ["", "a b", "a\r\nX: y", "ä", &"a".repeat(300)] {
            assert!(session_id(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn versions() {
        assert_eq!(
            version_text(" 4.0.5 (a6fe2a64aa) ").unwrap(),
            "4.0.5 (a6fe2a64aa)"
        );
        assert!(version_text("").is_err());
        assert!(version_text("4\n0").is_err());
        assert!(version_text(&"1".repeat(65)).is_err());
    }
}
