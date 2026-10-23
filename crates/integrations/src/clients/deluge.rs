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
//! - **Selected files** (a `.torrent` only): added paused;
//!   `core.get_torrent_status` polls `files` and `file_priorities` until the
//!   torrent is registered, `core.set_torrent_options` sets
//!   `file_priorities` (0 for skipped files, the existing priority for the
//!   rest), and `core.resume_torrent` starts it unless the user asked for
//!   paused. A paused magnet fetches no metadata, so a magnet's files can't
//!   be selected before it starts: refused as
//!   [`Feature::MagnetFileSelection`] before any request.
//! - **Answers** are typed; file lists are counted, never stored, and refused
//!   past [`MAX_FILES`](super::MAX_FILES).
//!
//! Recorded against Deluge 2.1.2 (libtorrent 2.0.10); the fixtures are in
//! `tests/fixtures/deluge/`.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, COOKIE, HeaderValue, SET_COOKIE};
use reqwest::{Method, Url};
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::qbittorrent::session_cookie;
use super::qbittorrent::upload_timeout;
use super::{
    AddOptions, AddOutcome, AuthFailure, CLIENT_TIMEOUT, Capabilities, Category, ClientError,
    ClientInfo, ClientKind, DownloadClient, Feature, FileCount, METADATA_WAIT, Magnet,
    POLL_INTERVAL, Priorities, Selection, TorrentFile, already_there, base64, category_name,
    check_file_count, check_magnets, check_request, client_reason, partial, poll, rejected_status,
    skipped_files,
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
struct Answer<T> {
    #[serde(default = "none")]
    result: Option<T>,
    #[serde(default)]
    error: Option<RpcError>,
    #[serde(default)]
    id: Option<u64>,
}

fn none<T>() -> Option<T> {
    None
}

/// `core.get_torrent_status` for `files` and `file_priorities`; an unknown
/// torrent is an empty object.
#[derive(Deserialize)]
struct TorrentStatus {
    files: Option<FileCount>,
    file_priorities: Option<Priorities>,
}

/// A `web.get_hosts` row: the host id, then details this adapter skips.
struct HostId(String);

impl<'de> Deserialize<'de> for HostId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<HostId, D::Error> {
        struct Row;
        impl<'de> Visitor<'de> for Row {
            type Value = HostId;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a host row")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<HostId, A::Error> {
                let id: String = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::custom("an empty host row"))?;
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(HostId(id))
            }
        }
        d.deserialize_seq(Row)
    }
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    #[serde(default)]
    message: String,
}

/// One call's outcome before session handling: `None` for a JSON null.
enum Reply<T> {
    Ok(Option<T>),
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
    async fn once<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: &Value,
        cookie: Option<&Secret>,
        limit: usize,
        timeout: Duration,
    ) -> Result<(Reply<T>, Option<Secret>), ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = serde_json::to_vec(&json!({ "method": method, "params": params, "id": id }))
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let mut rb = self
            .http
            .request(Method::POST, self.endpoint.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .timeout(timeout)
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
        lodestar_log::log_event!(
            crate::log_events::CLIENT_REQUEST,
            client = "deluge",
            method = method,
            status = lodestar_log::Count(status.into())
        );
        if status != 200 {
            return Err(rejected_status(status, &bytes));
        }
        let answer: Answer<T> = serde_json::from_slice(&bytes)
            .map_err(|_| ClientError::Protocol("the answer isn't Deluge's JSON"))?;
        if let Some(error) = answer.error {
            // Code 5 answers a request the Web UI couldn't read, with id
            // null; its message quotes that request.
            if error.code == 5 {
                return Err(ClientError::Protocol("Deluge couldn't read the request"));
            }
            if answer.id != Some(id) {
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
        if answer.id != Some(id) {
            return Err(ClientError::Protocol("the answer is for another request"));
        }
        Ok((Reply::Ok(answer.result), set_cookie))
    }

    /// Logs in and makes sure the Web UI is connected to a daemon.
    async fn login(&self) -> Result<Secret, ClientError> {
        let params = json!([self.password.expose()]);
        let (reply, cookie) = self
            .once::<bool>("auth.login", &params, None, MAX_RESPONSE, self.timeout)
            .await?;
        match reply {
            Reply::Ok(Some(true)) => {}
            Reply::Ok(Some(false)) => {
                return Err(ClientError::Auth(AuthFailure::BadCredentials));
            }
            _ => return Err(ClientError::Protocol("the login answer")),
        }
        let cookie = cookie.ok_or(ClientError::Protocol("the login set no session cookie"))?;
        if !self.connected(&cookie).await? {
            let hosts: Vec<HostId> = self
                .logged_in(&cookie, "web.get_hosts", json!([]))
                .await?
                .unwrap_or_default();
            let host = hosts
                .into_iter()
                .next()
                .map(|h| h.0)
                .filter(|id| id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
                .ok_or(ClientError::Rejected(
                    "Deluge's Web UI has no daemon to connect to".into(),
                ))?;
            self.logged_in::<IgnoredAny>(&cookie, "web.connect", json!([host]))
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
            .ok_or(ClientError::Protocol("web.connected isn't true or false"))
    }

    /// A call during login, where "not authenticated" can't be a stale
    /// session.
    async fn logged_in<T: DeserializeOwned>(
        &self,
        cookie: &Secret,
        method: &'static str,
        params: Value,
    ) -> Result<Option<T>, ClientError> {
        match self
            .once(method, &params, Some(cookie), MAX_RESPONSE, self.timeout)
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
    /// `None` is a JSON null result.
    async fn call<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Value,
        limit: usize,
    ) -> Result<Option<T>, ClientError> {
        self.call_timeout(method, params, limit, self.timeout).await
    }

    /// [`call`](Self::call) with its own timeout (uploads get more).
    async fn call_timeout<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Value,
        limit: usize,
        timeout: Duration,
    ) -> Result<Option<T>, ClientError> {
        let cookie = self.session().await?;
        if let Reply::Ok(v) = self
            .once(method, &params, Some(&cookie), limit, timeout)
            .await?
            .0
        {
            return Ok(v);
        }
        let cookie = self.relogin(&cookie).await?;
        match self
            .once(method, &params, Some(&cookie), limit, timeout)
            .await?
            .0
        {
            Reply::Ok(v) => Ok(v),
            Reply::NotAuthenticated => Err(ClientError::Auth(AuthFailure::SessionRefused)),
        }
    }

    /// A call whose result must not be null.
    async fn value<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Value,
        what: &'static str,
    ) -> Result<T, ClientError> {
        self.call(method, params, MAX_RESPONSE)
            .await?
            .ok_or(ClientError::Protocol(what))
    }

    async fn label_plugin(&self) -> Result<bool, ClientError> {
        let plugins: Vec<String> = self
            .value("core.get_enabled_plugins", json!([]), "the plugin list")
            .await?;
        Ok(plugins.iter().any(|p| p == "Label"))
    }

    async fn labels(&self) -> Result<Vec<String>, ClientError> {
        self.value("label.get_labels", json!([]), "the label list")
            .await
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
            self.call::<IgnoredAny>("label.add", json!([label]), MAX_RESPONSE)
                .await?;
        }
        Ok(Some(label))
    }

    /// One add (`method` with the source parameters), then the label.
    async fn add_one(
        &self,
        method: &'static str,
        mut params: Vec<Value>,
        label: Option<&str>,
        opts: &AddOptions,
        selecting: bool,
        timeout: Duration,
    ) -> Result<Option<String>, ClientError> {
        let mut options = serde_json::Map::new();
        if opts.paused || selecting {
            options.insert("add_paused".into(), true.into());
        }
        if let Some(path) = &opts.save_path {
            options.insert("download_location".into(), path.as_str().into());
        }
        params.push(Value::Object(options));
        let id: Option<String> = match self
            .call_timeout(method, Value::Array(params), MAX_RESPONSE, timeout)
            .await
        {
            Ok(v) => v,
            Err(ClientError::Rejected(why)) if why.contains("already in session") => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let id = id
            .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or(ClientError::Rejected("Deluge didn't add it".into()))?
            .to_ascii_lowercase();
        if let Some(label) = label {
            self.call::<IgnoredAny>("label.set_torrent", json!([id, label]), MAX_RESPONSE)
                .await?;
        }
        Ok(Some(id))
    }

    /// The priorities to keep for each file, once the torrent is
    /// registered: the existing one, or libtorrent's default where it's
    /// missing, zero or not a number.
    async fn priorities(&self, id: &str) -> Result<Option<Vec<u64>>, ClientError> {
        let status: Option<TorrentStatus> = self
            .call(
                "core.get_torrent_status",
                json!([id, ["files", "file_priorities"]]),
                MAX_LIST,
            )
            .await?;
        // An unknown torrent is an empty object: not registered yet.
        let Some(TorrentStatus {
            files: Some(files),
            file_priorities,
        }) = status
        else {
            return Ok(None);
        };
        let count = check_file_count(files.0)?;
        if count == 0 {
            return Ok(None);
        }
        let current = file_priorities.unwrap_or_default().0;
        Ok(Some(
            (0..count)
                .map(|i| match current.get(i) {
                    Some(&Some(p)) if p > 0 => p,
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
            lodestar_log::log_event!(crate::log_events::CLIENT_LEFT_PAUSED, client = "deluge");
            return Ok(Selection::Pending);
        };
        let skipped = skipped_files(selected, priorities.len())?;
        if !skipped.is_empty() {
            for i in skipped {
                priorities[i as usize] = 0;
            }
            self.call::<IgnoredAny>(
                "core.set_torrent_options",
                json!([[id], { "file_priorities": priorities }]),
                MAX_RESPONSE,
            )
            .await?;
        }
        if start {
            self.call::<IgnoredAny>("core.resume_torrent", json!([[id]]), MAX_RESPONSE)
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
            let version: String = self
                .value(
                    "daemon.get_version",
                    json!([]),
                    "the version isn't a version",
                )
                .await?;
            let version = Some(version.trim())
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
            let selection = check_magnets(CAPABILITIES, opts, magnets.len())?;
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
                        self.timeout,
                    )
                    .await?;
                return self.finish(id, selection, opts).await;
            }
            // Bulk: one add each. A duplicate is already where the user
            // wants it; only all-duplicates is refused. A failure part-way
            // says how many went in.
            let mut new = 0;
            for (done, magnet) in magnets.iter().enumerate() {
                let id = self
                    .add_one(
                        "core.add_torrent_magnet",
                        vec![magnet.as_str().into()],
                        label.as_deref(),
                        opts,
                        false,
                        self.timeout,
                    )
                    .await
                    .map_err(|e| partial(done, e))?;
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
                    upload_timeout(self.timeout, file.bytes.len()),
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
            let mut labels: Vec<String> = self
                .labels()
                .await?
                .iter()
                .filter_map(|l| category_name(l))
                .collect();
            labels.sort();
            labels.dedup();
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
