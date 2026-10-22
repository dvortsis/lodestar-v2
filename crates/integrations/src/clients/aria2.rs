//! Aria2, JSON-RPC over HTTP at `/jsonrpc`.
//!
//! - **Authentication** is `--rpc-secret`, sent as the first parameter of
//!   every call (`token:<secret>`), in the body only. A wrong one is
//!   `{"error": {"message": "Unauthorized"}}` with HTTP 400, after a delay
//!   aria2 adds itself. There is no session.
//! - **Errors** are `{"error": {"code", "message"}}`; the message is the
//!   client's reason. Every request has a new id, and an answer for another
//!   id is unusable (`Protocol`).
//! - **Adding** is `aria2.addUri` (a magnet) or `aria2.addTorrent` (base64
//!   `.torrent`), with the options `dir`, `pause` and `select-file`
//!   (1-based). aria2 applies the selection itself, at add time for a
//!   `.torrent` and when a magnet's metadata arrives, so nothing unwanted
//!   downloads and no paused flow is needed. A magnet's indices can't be
//!   checked against its files before then; a `.torrent`'s are checked with
//!   `aria2.getFiles` (it's added paused for that, and started after).
//! - **Duplicates** fail when the download starts (`errorCode` 12), so a
//!   started add is followed by `aria2.tellStatus`. A paused duplicate
//!   only fails when it's started, in aria2.
//! - **No categories:** `Unsupported`.
//!
//! Recorded against aria2 1.37.0; the fixtures are in
//! `tests/fixtures/aria2/`.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, Url};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    AddOptions, AddOutcome, AuthFailure, CLIENT_TIMEOUT, Capabilities, Category, ClientError,
    ClientInfo, ClientKind, DownloadClient, Feature, Magnet, Selection, TorrentFile, already_there,
    base64, check_request, client_reason, rejected_status,
};
use crate::BoxFuture;
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited, redact};
use crate::secret::Secret;

const CAPABILITIES: Capabilities = Capabilities {
    category: false,
    save_path: true,
    paused: true,
    file_selection: true,
    torrent_file: true,
    batch: false,
};

/// Most answers are a few hundred bytes.
const MAX_RESPONSE: usize = 1 << 20;
/// A file list.
const MAX_LIST: usize = 64 << 20;
/// aria2's error code for an info-hash it's already downloading.
const DUPLICATE: &str = "12";

pub struct Aria2 {
    endpoint: Url,
    secret: Option<Secret>,
    http: GuardedClient,
    next_id: AtomicU64,
    timeout: Duration,
}

impl fmt::Debug for Aria2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Aria2")
            .field("url", &redact(&self.endpoint))
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    id: Value,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    #[serde(default)]
    message: String,
}

impl Aria2 {
    /// An adapter for the RPC endpoint at `url`
    /// (`http://localhost:6800/jsonrpc`; a bare address gets `/jsonrpc`).
    pub fn new(url: &str, secret: Option<Secret>) -> Result<Aria2, ClientError> {
        let http = GuardedClient::new(NetPolicy::download_client(), CLIENT_TIMEOUT)?;
        Aria2::with_client(url, secret, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(
        url: &str,
        secret: Option<Secret>,
        http: GuardedClient,
    ) -> Result<Aria2, ClientError> {
        let mut endpoint = parse_url(url, http.policy())?;
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        if endpoint.path() == "/" {
            endpoint.set_path("/jsonrpc");
        }
        Ok(Aria2 {
            endpoint,
            secret,
            http,
            next_id: AtomicU64::new(1),
            timeout: CLIENT_TIMEOUT,
        })
    }

    /// The timeout for one request (default [`CLIENT_TIMEOUT`]).
    pub fn with_timeout(mut self, timeout: Duration) -> Aria2 {
        self.timeout = timeout;
        self
    }

    async fn call(
        &self,
        method: &str,
        params: Vec<Value>,
        limit: usize,
    ) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let mut all = Vec::with_capacity(params.len() + 1);
        if let Some(secret) = &self.secret {
            all.push(Value::String(format!("token:{}", secret.expose())));
        }
        all.extend(params);
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": all,
        }))
        .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        drop(all);
        let req = self
            .http
            .request(Method::POST, self.endpoint.clone())
            .header(CONTENT_TYPE, "application/json")
            .timeout(self.timeout)
            .body(body)
            .build()
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let resp = self.http.send(req).await?;
        let status = resp.status().as_u16();
        let bytes = read_limited(resp, limit).await?;
        tracing::debug!(method, status, "Aria2 request");
        if !matches!(status, 200 | 400) {
            return Err(rejected_status(status, &bytes));
        }
        let answer: Answer = serde_json::from_slice(&bytes)
            .map_err(|_| ClientError::Protocol("the answer isn't aria2's JSON"))?;
        if answer.id != Value::String(id) {
            return Err(ClientError::Protocol("the answer is for another request"));
        }
        match answer.error {
            Some(e) if e.message == "Unauthorized" => {
                Err(ClientError::Auth(AuthFailure::BadCredentials))
            }
            Some(e) => Err(ClientError::Rejected(client_reason(e.message.as_bytes()))),
            None if status == 200 => Ok(answer.result),
            None => Err(rejected_status(status, &bytes)),
        }
    }

    /// A new download's id: 16 hex digits.
    fn gid(v: Value) -> Result<String, ClientError> {
        v.as_str()
            .filter(|g| g.len() == 16 && g.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_owned)
            .ok_or(ClientError::Protocol("the add didn't answer a download id"))
    }

    /// `Ok(false)` when the started download failed as a duplicate.
    async fn started(&self, gid: &str) -> Result<bool, ClientError> {
        let status = self
            .call(
                "aria2.tellStatus",
                vec![gid.into(), json!(["status", "errorCode", "errorMessage"])],
                MAX_RESPONSE,
            )
            .await?;
        if status.get("status").and_then(Value::as_str) != Some("error") {
            return Ok(true);
        }
        if status.get("errorCode").and_then(Value::as_str) == Some(DUPLICATE) {
            return Ok(false);
        }
        let why = status
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or("");
        Err(ClientError::Rejected(client_reason(why.as_bytes())))
    }

    /// Adds one magnet; `Ok(false)` for a duplicate.
    async fn add_magnet_one(
        &self,
        magnet: &Magnet,
        opts: &AddOptions,
        selection: Option<&[u32]>,
    ) -> Result<bool, ClientError> {
        let options = options(opts, opts.paused, selection);
        let gid = Self::gid(
            self.call(
                "aria2.addUri",
                vec![json!([magnet.as_str()]), options],
                MAX_RESPONSE,
            )
            .await?,
        )?;
        if opts.paused {
            return Ok(true);
        }
        self.started(&gid).await
    }

    /// How many files a download has.
    async fn file_count(&self, gid: &str) -> Result<usize, ClientError> {
        let files = self
            .call("aria2.getFiles", vec![gid.into()], MAX_LIST)
            .await?;
        files
            .as_array()
            .map(Vec::len)
            .ok_or(ClientError::Protocol("the file list"))
    }
}

/// The add's options. aria2 takes every value as a string.
fn options(opts: &AddOptions, pause: bool, selection: Option<&[u32]>) -> Value {
    let mut map = serde_json::Map::new();
    if let Some(path) = &opts.save_path {
        map.insert("dir".into(), path.as_str().into());
    }
    if pause {
        map.insert("pause".into(), "true".into());
    }
    if let Some(selected) = selection {
        let list: Vec<String> = selected
            .iter()
            .map(|i| (u64::from(*i) + 1).to_string())
            .collect();
        map.insert("select-file".into(), list.join(",").into());
    }
    Value::Object(map)
}

impl DownloadClient for Aria2 {
    fn kind(&self) -> ClientKind {
        ClientKind::Aria2
    }

    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }

    fn test(&self) -> BoxFuture<'_, Result<ClientInfo, ClientError>> {
        Box::pin(async move {
            let info = self.call("aria2.getVersion", vec![], MAX_RESPONSE).await?;
            let version = info
                .get("version")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty() && v.len() <= 64 && !v.chars().any(char::is_control))
                .ok_or(ClientError::Protocol("the version isn't a version"))?;
            Ok(ClientInfo {
                kind: ClientKind::Aria2,
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
            // One add each. A duplicate is already where the user wants it;
            // only all-duplicates is refused.
            let mut new = 0;
            for magnet in magnets {
                new += usize::from(
                    self.add_magnet_one(magnet, opts, selection.as_deref())
                        .await?,
                );
            }
            if new == 0 {
                return Err(already_there());
            }
            Ok(AddOutcome {
                sent: magnets.len(),
                selection: if selection.is_some() {
                    Selection::Applied
                } else {
                    Selection::None
                },
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
            // Paused while the selection is checked against the files.
            let pause = opts.paused || selection.is_some();
            let options = options(opts, pause, selection.as_deref());
            let gid = Self::gid(
                self.call(
                    "aria2.addTorrent",
                    vec![base64(&file.bytes).into(), json!([]), options],
                    MAX_RESPONSE,
                )
                .await?,
            )?;
            if let Some(selected) = &selection {
                let count = self.file_count(&gid).await?;
                if selected.last().is_some_and(|&last| last as usize >= count) {
                    return Err(ClientError::InvalidRequest(
                        "a selected file isn't in the torrent; it was added paused",
                    ));
                }
                if !opts.paused {
                    self.call("aria2.unpause", vec![gid.as_str().into()], MAX_RESPONSE)
                        .await?;
                }
            }
            if !opts.paused && !self.started(&gid).await? {
                return Err(already_there());
            }
            Ok(AddOutcome {
                sent: 1,
                selection: if selection.is_some() {
                    Selection::Applied
                } else {
                    Selection::None
                },
            })
        })
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>, ClientError>> {
        Box::pin(async { Err(ClientError::Unsupported(Feature::Category)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_strings_and_selection_is_one_based() {
        let opts = AddOptions {
            save_path: Some("/data".into()),
            ..AddOptions::default()
        };
        assert_eq!(
            options(&opts, true, Some(&[0, 2])),
            json!({ "dir": "/data", "pause": "true", "select-file": "1,3" })
        );
        assert_eq!(options(&AddOptions::default(), false, None), json!({}));
        assert_eq!(
            options(&AddOptions::default(), false, Some(&[u32::MAX])),
            json!({ "select-file": "4294967296" })
        );
    }

    #[test]
    fn gids() {
        assert_eq!(
            Aria2::gid(json!("2089b05ecca3d829")).unwrap(),
            "2089b05ecca3d829"
        );
        assert!(Aria2::gid(json!("short")).is_err());
        assert!(Aria2::gid(json!(12)).is_err());
    }

    #[test]
    fn urls() {
        let a = |u: &str| Aria2::new(u, None).unwrap().endpoint.to_string();
        assert_eq!(a("http://10.0.0.5:6800"), "http://10.0.0.5:6800/jsonrpc");
        assert_eq!(
            a("http://10.0.0.5:6800/jsonrpc"),
            "http://10.0.0.5:6800/jsonrpc"
        );
    }
}
