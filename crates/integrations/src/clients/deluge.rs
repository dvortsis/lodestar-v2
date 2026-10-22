//! Deluge, through the Web UI's JSON-RPC at `/json`.
//!
//! - **Login** is `auth.login` with the Web UI password; the session is the
//!   `_session_id` cookie it sets. `false` is a wrong password. The Web UI
//!   then has to be connected to a daemon: if `web.connected` says no, it is
//!   connected to the first host from `web.get_hosts` and asked again.
//! - **Errors** come back as `{"error": {"code", "message"}}`. Code 1 ("Not
//!   authenticated") is an expired session: one new login and one retry,
//!   never a loop. Code 2 is an unknown method. Codes 3 and 4 are the
//!   daemon's refusals, whose message is a Python traceback; only the
//!   exception's text is kept. Code 5 means the Web UI couldn't read the
//!   request, and its message echoes the request (password included), so it
//!   is never shown.
//! - **Request ids** are numbers this adapter counts. An answer whose id
//!   doesn't match the request's is ignored as unusable (`Protocol`).
//! - **Adding** is `core.add_torrent_magnet` or `core.add_torrent_file`
//!   with `add_paused` and `download_location`. Labels need the Label
//!   plugin (`core.get_enabled_plugins`). Its labels are lowercase, so the
//!   requested one is lowercased, created if missing (`label.add`) before
//!   the add, and set with `label.set_torrent` after it.
//! - **Selected files:** added paused; `core.get_torrent_status` polls
//!   `files` and `file_priorities` until the metadata is there,
//!   `core.set_torrent_options` sets `file_priorities` (0 for skipped files,
//!   the existing priority for the rest), and `core.resume_torrent` starts
//!   it unless the user asked for paused. Otherwise it stays paused and the
//!   outcome is [`Selection::Pending`].
//!
//! Recorded against Deluge 2.1.2 (libtorrent 2.0.10); the fixtures are in
//! `tests/fixtures/deluge/`.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, COOKIE, HeaderValue, SET_COOKIE};
use reqwest::{Method, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::qbittorrent::session_cookie;
use super::{
    AddOptions, AddOutcome, AuthFailure, CLIENT_TIMEOUT, Capabilities, Category, ClientError,
    ClientInfo, ClientKind, DownloadClient, Feature, METADATA_WAIT, Magnet, POLL_INTERVAL,
    Selection, TorrentFile, already_there, base64, check_request, client_reason, poll,
    rejected_status, skipped_files,
};
use crate::BoxFuture;
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited, redact};
use crate::secret::Secret;

const CAPABILITIES: Capabilities = Capabilities {
    category: true,
    save_path: true,
    paused: true,
    file_selection: true,
    torrent_file: true,
    batch: false,
};

/// Most answers are small; `web.connect` lists every daemon method.
const MAX_RESPONSE: usize = 1 << 20;
/// A file list.
const MAX_LIST: usize = 64 << 20;
/// libtorrent's default ("Normal") priority.
const NORMAL_PRIORITY: u64 = 4;

pub struct Deluge {
    endpoint: Url,
    password: Secret,
    http: GuardedClient,
    /// The session cookie (`_session_id=…`); `None` until the first login.
    session: Mutex<Option<Secret>>,
    next_id: AtomicU64,
    metadata_wait: Duration,
    poll_interval: Duration,
    timeout: Duration,
}

impl fmt::Debug for Deluge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Deluge")
            .field("url", &redact(&self.endpoint))
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<RpcError>,
    #[serde(default)]
    id: Value,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    #[serde(default)]
    message: String,
}

/// One call's outcome before session handling.
enum Reply {
    Ok(Value),
    NotAuthenticated,
}

impl Deluge {
    /// An adapter for the Web UI at `url` (`http://localhost:8112/`, or a
    /// reverse-proxy path).
    pub fn new(url: &str, password: Secret) -> Result<Deluge, ClientError> {
        let http = GuardedClient::new(NetPolicy::download_client(), CLIENT_TIMEOUT)?;
        Deluge::with_client(url, password, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(
        url: &str,
        password: Secret,
        http: GuardedClient,
    ) -> Result<Deluge, ClientError> {
        let mut endpoint = parse_url(url, http.policy())?;
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let path = format!("{}/json", endpoint.path().trim_end_matches('/'));
        endpoint.set_path(&path);
        Ok(Deluge {
            endpoint,
            password,
            http,
            session: Mutex::new(None),
            next_id: AtomicU64::new(1),
            metadata_wait: METADATA_WAIT,
            poll_interval: POLL_INTERVAL,
            timeout: CLIENT_TIMEOUT,
        })
    }

    /// The timeout for one request (default [`CLIENT_TIMEOUT`]).
    pub fn with_timeout(mut self, timeout: Duration) -> Deluge {
        self.timeout = timeout;
        self
    }

    /// How long to wait for a magnet's metadata before leaving a file
    /// selection [`Selection::Pending`], and how often to look.
    pub fn with_metadata_wait(mut self, wait: Duration, poll: Duration) -> Deluge {
        self.metadata_wait = wait;
        self.poll_interval = poll;
        self
    }

    /// Sends one request; returns the answer and any session cookie set.
    async fn once(
        &self,
        method: &str,
        params: &Value,
        cookie: Option<&Secret>,
        limit: usize,
    ) -> Result<(Reply, Option<Secret>), ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = serde_json::to_vec(&json!({ "method": method, "params": params, "id": id }))
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let mut rb = self
            .http
            .request(Method::POST, self.endpoint.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .timeout(self.timeout)
            .body(body);
        if let Some(cookie) = cookie {
            let mut value = HeaderValue::from_str(cookie.expose())
                .map_err(|_| ClientError::Protocol("the session cookie is malformed"))?;
            value.set_sensitive(true);
            rb = rb.header(COOKIE, value);
        }
        let req = rb
            .build()
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let resp = self.http.send(req).await?;
        let status = resp.status().as_u16();
        let set_cookie = resp
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(session_cookie);
        let bytes = read_limited(resp, limit).await?;
        tracing::debug!(method, status, "Deluge request");
        if status != 200 {
            return Err(rejected_status(status, &bytes));
        }
        let answer: Answer = serde_json::from_slice(&bytes)
            .map_err(|_| ClientError::Protocol("the answer isn't Deluge's JSON"))?;
        if let Some(error) = answer.error {
            // Code 5 answers a request the Web UI couldn't read, with id
            // null; its message quotes that request.
            if error.code == 5 {
                return Err(ClientError::Protocol("Deluge couldn't read the request"));
            }
            if answer.id != json!(id) {
                return Err(ClientError::Protocol("the answer is for another request"));
            }
            return match error.code {
                1 => Ok((Reply::NotAuthenticated, set_cookie)),
                2 => Err(ClientError::Rejected(format!(
                    "Deluge doesn't know {}",
                    client_reason(method.as_bytes())
                ))),
                _ => Err(ClientError::Rejected(deluge_reason(&error.message))),
            };
        }
        if answer.id != json!(id) {
            return Err(ClientError::Protocol("the answer is for another request"));
        }
        Ok((Reply::Ok(answer.result), set_cookie))
    }

    /// Logs in and makes sure the Web UI is connected to a daemon.
    async fn login(&self) -> Result<Secret, ClientError> {
        let params = json!([self.password.expose()]);
        let (reply, cookie) = self.once("auth.login", &params, None, MAX_RESPONSE).await?;
        match reply {
            Reply::Ok(Value::Bool(true)) => {}
            Reply::Ok(Value::Bool(false)) => {
                return Err(ClientError::Auth(AuthFailure::BadCredentials));
            }
            _ => return Err(ClientError::Protocol("the login answer")),
        }
        let cookie = cookie.ok_or(ClientError::Protocol("the login set no session cookie"))?;
        if !self.connected(&cookie).await? {
            let hosts = self.logged_in(&cookie, "web.get_hosts", json!([])).await?;
            let host = hosts
                .as_array()
                .and_then(|h| h.first())
                .and_then(|h| h.get(0))
                .and_then(Value::as_str)
                .filter(|id| id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
                .ok_or(ClientError::Rejected(
                    "Deluge's Web UI has no daemon to connect to".into(),
                ))?
                .to_owned();
            self.logged_in(&cookie, "web.connect", json!([host]))
                .await?;
            if !self.connected(&cookie).await? {
                return Err(ClientError::Rejected(
                    "Deluge's Web UI couldn't connect to its daemon".into(),
                ));
            }
        }
        Ok(cookie)
    }

    async fn connected(&self, cookie: &Secret) -> Result<bool, ClientError> {
        self.logged_in(cookie, "web.connected", json!([]))
            .await?
            .as_bool()
            .ok_or(ClientError::Protocol("web.connected isn't true or false"))
    }

    /// A call during login, where "not authenticated" can't be a stale
    /// session.
    async fn logged_in(
        &self,
        cookie: &Secret,
        method: &str,
        params: Value,
    ) -> Result<Value, ClientError> {
        match self
            .once(method, &params, Some(cookie), MAX_RESPONSE)
            .await?
            .0
        {
            Reply::Ok(v) => Ok(v),
            Reply::NotAuthenticated => Err(ClientError::Auth(AuthFailure::SessionRefused)),
        }
    }

    /// The current session, logging in if there's none.
    async fn session(&self) -> Result<Secret, ClientError> {
        let mut session = self.session.lock().await;
        if let Some(cookie) = &*session {
            return Ok(cookie.clone());
        }
        let cookie = self.login().await?;
        *session = Some(cookie.clone());
        Ok(cookie)
    }

    /// Logs in again after `stale` was refused, unless another request
    /// already did.
    async fn relogin(&self, stale: &Secret) -> Result<Secret, ClientError> {
        let mut session = self.session.lock().await;
        if let Some(current) = &*session {
            if current.expose() != stale.expose() {
                return Ok(current.clone());
            }
        }
        *session = None;
        let cookie = self.login().await?;
        *session = Some(cookie.clone());
        Ok(cookie)
    }

    /// One call with the session, logging in again once if it expired.
    async fn call(&self, method: &str, params: Value, limit: usize) -> Result<Value, ClientError> {
        let cookie = self.session().await?;
        if let Reply::Ok(v) = self.once(method, &params, Some(&cookie), limit).await?.0 {
            return Ok(v);
        }
        let cookie = self.relogin(&cookie).await?;
        match self.once(method, &params, Some(&cookie), limit).await?.0 {
            Reply::Ok(v) => Ok(v),
            Reply::NotAuthenticated => Err(ClientError::Auth(AuthFailure::SessionRefused)),
        }
    }

    async fn label_plugin(&self) -> Result<bool, ClientError> {
        let plugins = self
            .call("core.get_enabled_plugins", json!([]), MAX_RESPONSE)
            .await?;
        let plugins = plugins
            .as_array()
            .ok_or(ClientError::Protocol("the plugin list"))?;
        Ok(plugins.iter().any(|p| p.as_str() == Some("Label")))
    }

    async fn labels(&self) -> Result<Vec<String>, ClientError> {
        let labels = self
            .call("label.get_labels", json!([]), MAX_RESPONSE)
            .await?;
        let labels = labels
            .as_array()
            .ok_or(ClientError::Protocol("the label list"))?;
        Ok(labels
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect())
    }

    /// The label to set, created if it doesn't exist; before the add, so a
    /// refused label adds nothing.
    async fn prepare_label(&self, opts: &AddOptions) -> Result<Option<String>, ClientError> {
        let Some(label) = &opts.category else {
            return Ok(None);
        };
        if !self.label_plugin().await? {
            return Err(ClientError::Unsupported(Feature::Category));
        }
        let label = label.to_lowercase();
        if !self.labels().await?.contains(&label) {
            self.call("label.add", json!([label]), MAX_RESPONSE).await?;
        }
        Ok(Some(label))
    }

    /// One add (`method` with the source parameters), then the label.
    async fn add_one(
        &self,
        method: &str,
        mut params: Vec<Value>,
        label: Option<&str>,
        opts: &AddOptions,
        selecting: bool,
    ) -> Result<Option<String>, ClientError> {
        let mut options = serde_json::Map::new();
        if opts.paused || selecting {
            options.insert("add_paused".into(), true.into());
        }
        if let Some(path) = &opts.save_path {
            options.insert("download_location".into(), path.as_str().into());
        }
        params.push(Value::Object(options));
        let id = match self.call(method, Value::Array(params), MAX_RESPONSE).await {
            Ok(v) => v,
            Err(ClientError::Rejected(why)) if why.contains("already in session") => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let id = id
            .as_str()
            .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or(ClientError::Rejected("Deluge didn't add it".into()))?
            .to_ascii_lowercase();
        if let Some(label) = label {
            self.call("label.set_torrent", json!([id, label]), MAX_RESPONSE)
                .await?;
        }
        Ok(Some(id))
    }

    /// The torrent's file priorities, once its metadata is there.
    async fn priorities(&self, id: &str) -> Result<Option<Vec<u64>>, ClientError> {
        let status = self
            .call(
                "core.get_torrent_status",
                json!([id, ["files", "file_priorities"]]),
                MAX_LIST,
            )
            .await?;
        // An unknown torrent is an empty object: not registered yet.
        let Some(files) = status.get("files") else {
            return Ok(None);
        };
        let count = files
            .as_array()
            .ok_or(ClientError::Protocol("the file list"))?
            .len();
        if count == 0 {
            return Ok(None);
        }
        let current: Vec<u64> = status
            .get("file_priorities")
            .and_then(Value::as_array)
            .map(|p| p.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        Ok(Some(
            (0..count)
                .map(|i| match current.get(i) {
                    Some(&p) if p > 0 => p,
                    _ => NORMAL_PRIORITY,
                })
                .collect(),
        ))
    }

    async fn select_files(
        &self,
        id: &str,
        selected: &[u32],
        start: bool,
    ) -> Result<Selection, ClientError> {
        let Some(mut priorities) = poll(self.metadata_wait, self.poll_interval, || {
            self.priorities(id)
        })
        .await?
        else {
            tracing::debug!("Deluge metadata didn't arrive; left paused");
            return Ok(Selection::Pending);
        };
        let skipped = skipped_files(selected, priorities.len())?;
        if !skipped.is_empty() {
            for i in skipped {
                priorities[i as usize] = 0;
            }
            self.call(
                "core.set_torrent_options",
                json!([[id], { "file_priorities": priorities }]),
                MAX_RESPONSE,
            )
            .await?;
        }
        if start {
            self.call("core.resume_torrent", json!([[id]]), MAX_RESPONSE)
                .await?;
        }
        Ok(Selection::Applied)
    }

    async fn finish(
        &self,
        id: Option<String>,
        selection: Option<Vec<u32>>,
        opts: &AddOptions,
    ) -> Result<AddOutcome, ClientError> {
        let Some(id) = id else {
            return Err(already_there());
        };
        let selection = match selection {
            Some(selected) => self.select_files(&id, &selected, !opts.paused).await?,
            None => Selection::None,
        };
        Ok(AddOutcome { sent: 1, selection })
    }
}

impl DownloadClient for Deluge {
    fn kind(&self) -> ClientKind {
        ClientKind::Deluge
    }

    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }

    fn test(&self) -> BoxFuture<'_, Result<ClientInfo, ClientError>> {
        Box::pin(async move {
            let version = self
                .call("daemon.get_version", json!([]), MAX_RESPONSE)
                .await?;
            let version = version
                .as_str()
                .map(str::trim)
                .filter(|v| !v.is_empty() && v.len() <= 64 && !v.chars().any(char::is_control))
                .ok_or(ClientError::Protocol("the version isn't a version"))?;
            Ok(ClientInfo {
                kind: ClientKind::Deluge,
                version: version.to_owned(),
                api_version: None,
            })
        })
    }

    fn add_magnets<'a>(
        &'a self,
        magnets: &'a [Magnet],
        opts: &'a AddOptions,
    ) -> BoxFuture<'a, Result<AddOutcome, ClientError>> {
        Box::pin(async move {
            let selection = check_request(CAPABILITIES, opts, magnets.len())?;
            let label = self.prepare_label(opts).await?;
            let selecting = selection.is_some();
            if let [magnet] = magnets {
                let id = self
                    .add_one(
                        "core.add_torrent_magnet",
                        vec![magnet.as_str().into()],
                        label.as_deref(),
                        opts,
                        selecting,
                    )
                    .await?;
                return self.finish(id, selection, opts).await;
            }
            // Bulk: one add each. A duplicate is already where the user
            // wants it; only all-duplicates is refused.
            let mut new = 0;
            for magnet in magnets {
                let id = self
                    .add_one(
                        "core.add_torrent_magnet",
                        vec![magnet.as_str().into()],
                        label.as_deref(),
                        opts,
                        false,
                    )
                    .await?;
                new += usize::from(id.is_some());
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
            let label = self.prepare_label(opts).await?;
            let id = self
                .add_one(
                    "core.add_torrent_file",
                    vec!["lodestar.torrent".into(), base64(&file.bytes).into()],
                    label.as_deref(),
                    opts,
                    selection.is_some(),
                )
                .await?;
            self.finish(id, selection, opts).await
        })
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>, ClientError>> {
        Box::pin(async move {
            if !self.label_plugin().await? {
                return Err(ClientError::Unsupported(Feature::Category));
            }
            let mut labels = self.labels().await?;
            labels.sort();
            labels.dedup();
            Ok(labels
                .into_iter()
                .filter(|l| !l.is_empty())
                .map(|name| Category {
                    name,
                    save_path: None,
                })
                .collect())
        })
    }
}

/// The daemon's reason out of a Deluge error message: the exception's text
/// after `<class '…'>: `, without the traceback that follows.
fn deluge_reason(message: &str) -> String {
    let text = match message.find("'>: ") {
        Some(at) => &message[at + 4..],
        None => message,
    };
    // The traceback, or the Failure wrapper's closing `]`, follows on the
    // next line; on one line, the wrapper's `]` ends the text.
    let text = match text.split_once('\n') {
        Some((first, _)) => first,
        None if message.starts_with("Failure: [") => text.strip_suffix(']').unwrap_or(text),
        None => text,
    };
    client_reason(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_drop_the_traceback() {
        assert_eq!(
            deluge_reason(
                "Failure: [Failure instance: Traceback (failure with no frames): <class 'deluge.error.AddTorrentError'>: Torrent already in session (c9e1).\n]"
            ),
            "Torrent already in session (c9e1)."
        );
        assert_eq!(
            deluge_reason(
                "Failure: [Failure instance: Traceback (failure with no frames): <class 'deluge.error.WrappedException'>: Unknown Label\nTraceback (most recent call last):\n  File \"x\""
            ),
            "Unknown Label"
        );
        assert_eq!(
            deluge_reason("Failure: [Failure instance: <class 'x.Error'>: bad [a-z]]"),
            "bad [a-z]"
        );
        assert_eq!(deluge_reason("plain [x]"), "plain [x]");
        assert_eq!(deluge_reason(""), "no reason given");
    }

    #[test]
    fn urls_get_the_json_path() {
        let d = |u: &str| Deluge::new(u, "p".into()).unwrap().endpoint.to_string();
        assert_eq!(d("http://10.0.0.5:8112"), "http://10.0.0.5:8112/json");
        assert_eq!(d("http://nas/deluge/?q#f"), "http://nas/deluge/json");
        assert!(matches!(
            Deluge::new("http://[fe80::1]/", "p".into()),
            Err(ClientError::Blocked(_))
        ));
    }
}
