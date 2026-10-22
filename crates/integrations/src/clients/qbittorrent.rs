//! qBittorrent, WebUI API v2 (the reference adapter).
//!
//! - **Login** is `POST auth/login` with a form; the session is the cookie
//!   it sets (`SID` by default, but the name is taken from the response).
//!   `Ok.` without a cookie means the client bypasses authentication for
//!   this address, which is fine.
//! - **CSRF:** qBittorrent answers `401 Unauthorized` when a request's
//!   `Origin` or `Referer` doesn't match its host, so every request carries
//!   both, set to the configured URL's origin.
//! - **Sessions expire** (`403 Forbidden`). A 403 on an API call logs in
//!   again once and retries once. A 403 on the login itself is a ban after
//!   failed logins, which is never retried.
//! - **Adding** is a multipart `torrents/add`: `urls` (magnets, one per
//!   line, which is why [`Magnet`] refuses whitespace) or a `torrents` file
//!   part, plus `savepath`, `category` and both `paused` (v4) and `stopped`
//!   (v5) when adding paused.
//! - **Selected files:** the torrent is added paused, the file list is
//!   polled until the client has the metadata, unselected files get
//!   priority 0 with `torrents/filePrio`, and the torrent is started unless
//!   the user asked for paused. If the metadata doesn't arrive in time the
//!   torrent stays paused and the outcome says [`Selection::Pending`].
//!   Starting is `torrents/start` (v5), falling back to `torrents/resume`
//!   (v4) on 404.
//!
//! Recorded against qBittorrent 4.6.3 (API 2.9.3); the fixtures are in
//! `tests/fixtures/qbittorrent/`. qBittorrent 5.1 may answer `torrents/add`
//! with a JSON summary instead of `Ok.`; that's accepted too.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use reqwest::header::{COOKIE, HeaderValue, ORIGIN, REFERER, SET_COOKIE};
use reqwest::multipart::{Form, Part};
use reqwest::{Method, Url};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::{
    AddOptions, AddOutcome, AuthFailure, CLIENT_TIMEOUT, Capabilities, Category, ClientError,
    ClientInfo, ClientKind, DownloadClient, Magnet, Selection, TorrentFile, check_request,
    client_reason,
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
    batch: true,
};

/// Most answers are a word or a small JSON object.
const MAX_RESPONSE: usize = 1 << 20;
/// A file list is about 200 bytes per file; this allows 300k files.
const MAX_FILE_LIST: usize = 64 << 20;
/// File priorities are set this many files per request.
const PRIORITY_BATCH: usize = 1000;

pub struct QBittorrent {
    base: Url,
    origin: HeaderValue,
    referer: HeaderValue,
    username: String,
    password: Secret,
    http: GuardedClient,
    /// The session cookie (`name=value`); empty when the client doesn't
    /// require a login. `None` until the first login.
    session: Mutex<Option<Secret>>,
    metadata_wait: Duration,
    poll_interval: Duration,
}

impl fmt::Debug for QBittorrent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QBittorrent")
            .field("url", &redact(&self.base))
            .finish_non_exhaustive()
    }
}

enum Body {
    Empty,
    Form(Vec<(&'static str, String)>),
    Multipart(Vec<(&'static str, String)>, Option<Vec<u8>>),
}

#[derive(Deserialize)]
struct CategoryEntry {
    #[serde(rename = "savePath", default)]
    save_path: Option<String>,
}

impl QBittorrent {
    /// An adapter for the WebUI at `url` (for example
    /// `http://localhost:8080/`, or a reverse-proxy path).
    pub fn new(url: &str, username: String, password: Secret) -> Result<QBittorrent, ClientError> {
        let http = GuardedClient::new(NetPolicy::download_client(), CLIENT_TIMEOUT)?;
        QBittorrent::with_client(url, username, password, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(
        url: &str,
        username: String,
        password: Secret,
        http: GuardedClient,
    ) -> Result<QBittorrent, ClientError> {
        let mut base = parse_url(url, http.policy())?;
        base.set_query(None);
        base.set_fragment(None);
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let origin = base.origin().ascii_serialization();
        let header = |s: &str| {
            HeaderValue::from_str(s).map_err(|_| ClientError::InvalidRequest("the URL's host"))
        };
        Ok(QBittorrent {
            origin: header(&origin)?,
            referer: header(&format!("{origin}/"))?,
            base,
            username,
            password,
            http,
            session: Mutex::new(None),
            metadata_wait: Duration::from_secs(30),
            poll_interval: Duration::from_millis(500),
        })
    }

    /// How long to wait for a magnet's metadata before leaving a file
    /// selection [`Selection::Pending`], and how often to look.
    pub fn with_metadata_wait(mut self, wait: Duration, poll: Duration) -> QBittorrent {
        self.metadata_wait = wait;
        self.poll_interval = poll;
        self
    }

    fn url(&self, endpoint: &str) -> Result<Url, ClientError> {
        self.base
            .join(&format!("api/v2/{endpoint}"))
            .map_err(|_| ClientError::InvalidRequest("the URL can't take an API path"))
    }

    fn request(
        &self,
        method: Method,
        endpoint: &str,
    ) -> Result<reqwest::RequestBuilder, ClientError> {
        Ok(self
            .http
            .http()
            .request(method, self.url(endpoint)?)
            .header(REFERER, self.referer.clone())
            .header(ORIGIN, self.origin.clone()))
    }

    async fn login(&self) -> Result<Secret, ClientError> {
        let form = [
            ("username", self.username.as_str()),
            ("password", self.password.expose()),
        ];
        let req = self
            .request(Method::POST, "auth/login")?
            .form(&form)
            .build()
            .map_err(|_| ClientError::Protocol("couldn't build the login"))?;
        let resp = self.http.send(req).await?;
        let status = resp.status().as_u16();
        let cookie = resp
            .headers()
            .get(SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .map(session_cookie);
        let body = read_limited(resp, MAX_RESPONSE).await?;
        tracing::debug!(status, "qBittorrent login");
        if body.trim_ascii() == b"Fails." {
            return Err(ClientError::Auth(AuthFailure::BadCredentials));
        }
        match status {
            200 if body.trim_ascii() == b"Ok." => match cookie {
                Some(Some(cookie)) => Ok(cookie),
                Some(None) => Err(ClientError::Protocol("the session cookie is malformed")),
                None => Ok(Secret::new("")),
            },
            401 => Err(ClientError::Auth(AuthFailure::OriginRefused)),
            403 => Err(ClientError::Auth(AuthFailure::Banned)),
            _ => Err(ClientError::Protocol("the login answer")),
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

    async fn once(
        &self,
        method: &Method,
        endpoint: &str,
        query: &[(&str, &str)],
        body: &Body,
        cookie: &Secret,
        limit: usize,
    ) -> Result<(u16, Vec<u8>), ClientError> {
        let mut rb = self.request(method.clone(), endpoint)?.query(query);
        if !cookie.is_empty() {
            let mut value = HeaderValue::from_str(cookie.expose())
                .map_err(|_| ClientError::Protocol("the session cookie is malformed"))?;
            value.set_sensitive(true);
            rb = rb.header(COOKIE, value);
        }
        rb = match body {
            Body::Empty => rb,
            Body::Form(fields) => rb.form(fields),
            Body::Multipart(fields, file) => {
                let mut form = Form::new();
                for (name, value) in fields {
                    form = form.text(*name, value.clone());
                }
                if let Some(bytes) = file {
                    let part = Part::bytes(bytes.clone())
                        .file_name("lodestar.torrent")
                        .mime_str("application/x-bittorrent")
                        .map_err(|_| ClientError::Protocol("couldn't build the upload"))?;
                    form = form.part("torrents", part);
                }
                rb.multipart(form)
            }
        };
        let req = rb
            .build()
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let resp = self.http.send(req).await?;
        let status = resp.status().as_u16();
        let bytes = read_limited(resp, limit).await?;
        tracing::debug!(endpoint, status, "qBittorrent request");
        Ok((status, bytes))
    }

    /// One API call with the session, logging in again once on 403.
    async fn api(
        &self,
        method: Method,
        endpoint: &str,
        query: &[(&str, &str)],
        body: Body,
        limit: usize,
    ) -> Result<(u16, Vec<u8>), ClientError> {
        let cookie = self.session().await?;
        let mut answer = self
            .once(&method, endpoint, query, &body, &cookie, limit)
            .await?;
        if answer.0 == 403 {
            let cookie = self.relogin(&cookie).await?;
            answer = self
                .once(&method, endpoint, query, &body, &cookie, limit)
                .await?;
            if answer.0 == 403 {
                return Err(ClientError::Auth(AuthFailure::SessionRefused));
            }
        }
        if answer.0 == 401 {
            return Err(ClientError::Auth(AuthFailure::OriginRefused));
        }
        Ok(answer)
    }

    async fn text(&self, endpoint: &str) -> Result<Option<String>, ClientError> {
        let (status, body) = self
            .api(Method::GET, endpoint, &[], Body::Empty, MAX_RESPONSE)
            .await?;
        match status {
            200 => short_text(&body).map(Some),
            404 => Ok(None),
            _ => Err(rejected(status, &body)),
        }
    }

    async fn add(
        &self,
        mut fields: Vec<(&'static str, String)>,
        file: Option<Vec<u8>>,
        opts: &AddOptions,
        selecting: bool,
    ) -> Result<(), ClientError> {
        if let Some(path) = &opts.save_path {
            fields.push(("savepath", path.clone()));
        }
        if let Some(category) = &opts.category {
            fields.push(("category", category.clone()));
        }
        if opts.paused || selecting {
            fields.push(("paused", "true".into()));
            fields.push(("stopped", "true".into()));
        }
        let (status, body) = self
            .api(
                Method::POST,
                "torrents/add",
                &[],
                Body::Multipart(fields, file),
                MAX_RESPONSE,
            )
            .await?;
        match status {
            200 => add_answer(&body),
            _ => Err(rejected(status, &body)),
        }
    }

    /// Applies a file selection to the torrent `id`, which was added paused.
    async fn select_files(
        &self,
        id: &str,
        selected: &[u32],
        start: bool,
    ) -> Result<Selection, ClientError> {
        let deadline = Instant::now() + self.metadata_wait;
        let count = loop {
            let (status, body) = self
                .api(
                    Method::GET,
                    "torrents/files",
                    &[("hash", id)],
                    Body::Empty,
                    MAX_FILE_LIST,
                )
                .await?;
            match status {
                200 => {
                    let files: Vec<serde::de::IgnoredAny> = serde_json::from_slice(&body)
                        .map_err(|_| ClientError::Protocol("the file list"))?;
                    if !files.is_empty() {
                        break files.len();
                    }
                }
                // Not registered yet: the add is processed asynchronously.
                404 => {}
                _ => return Err(rejected(status, &body)),
            }
            if Instant::now() + self.poll_interval > deadline {
                tracing::debug!("qBittorrent metadata didn't arrive; left paused");
                return Ok(Selection::Pending);
            }
            tokio::time::sleep(self.poll_interval).await;
        };
        if selected.last().is_some_and(|&last| last as usize >= count) {
            return Err(ClientError::InvalidRequest(
                "a selected file isn't in the torrent; it was added paused",
            ));
        }
        let skipped: Vec<String> = (0..count as u32)
            .filter(|i| selected.binary_search(i).is_err())
            .map(|i| i.to_string())
            .collect();
        for batch in skipped.chunks(PRIORITY_BATCH) {
            let form = vec![
                ("hash", id.to_owned()),
                ("id", batch.join("|")),
                ("priority", "0".to_owned()),
            ];
            let (status, body) = self
                .api(
                    Method::POST,
                    "torrents/filePrio",
                    &[],
                    Body::Form(form),
                    MAX_RESPONSE,
                )
                .await?;
            if status != 200 {
                return Err(rejected(status, &body));
            }
        }
        if start {
            self.start(id).await?;
        }
        Ok(Selection::Applied)
    }

    async fn start(&self, id: &str) -> Result<(), ClientError> {
        for endpoint in ["torrents/start", "torrents/resume"] {
            let form = vec![("hashes", id.to_owned())];
            let (status, body) = self
                .api(Method::POST, endpoint, &[], Body::Form(form), MAX_RESPONSE)
                .await?;
            match status {
                200 => return Ok(()),
                404 => continue,
                _ => return Err(rejected(status, &body)),
            }
        }
        Err(ClientError::Protocol(
            "neither torrents/start nor torrents/resume exists",
        ))
    }

    async fn finish(
        &self,
        id: &str,
        selection: Option<Vec<u32>>,
        opts: &AddOptions,
        sent: usize,
    ) -> Result<AddOutcome, ClientError> {
        let selection = match selection {
            Some(selected) => self.select_files(id, &selected, !opts.paused).await?,
            None => Selection::None,
        };
        Ok(AddOutcome { sent, selection })
    }
}

impl DownloadClient for QBittorrent {
    fn kind(&self) -> ClientKind {
        ClientKind::QBittorrent
    }

    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }

    fn test(&self) -> BoxFuture<'_, Result<ClientInfo, ClientError>> {
        Box::pin(async move {
            let version = self
                .text("app/version")
                .await?
                .ok_or(ClientError::Protocol(
                    "no app/version: not qBittorrent's API v2",
                ))?;
            let api_version = self.text("app/webapiVersion").await?;
            Ok(ClientInfo {
                kind: ClientKind::QBittorrent,
                version,
                api_version,
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
            let urls = magnets
                .iter()
                .map(Magnet::as_str)
                .collect::<Vec<_>>()
                .join("\n");
            self.add(vec![("urls", urls)], None, opts, selection.is_some())
                .await?;
            self.finish(magnets[0].torrent_id(), selection, opts, magnets.len())
                .await
        })
    }

    fn add_torrent_file<'a>(
        &'a self,
        file: &'a TorrentFile,
        opts: &'a AddOptions,
    ) -> BoxFuture<'a, Result<AddOutcome, ClientError>> {
        Box::pin(async move {
            let selection = check_request(CAPABILITIES, opts, 1)?;
            self.add(
                Vec::new(),
                Some(file.bytes.clone()),
                opts,
                selection.is_some(),
            )
            .await?;
            self.finish(&file.info_hash, selection, opts, 1).await
        })
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>, ClientError>> {
        Box::pin(async move {
            let (status, body) = self
                .api(
                    Method::GET,
                    "torrents/categories",
                    &[],
                    Body::Empty,
                    MAX_RESPONSE,
                )
                .await?;
            if status != 200 {
                return Err(rejected(status, &body));
            }
            let map: BTreeMap<String, CategoryEntry> = serde_json::from_slice(&body)
                .map_err(|_| ClientError::Protocol("the category list"))?;
            Ok(map
                .into_iter()
                .map(|(name, entry)| Category {
                    name,
                    save_path: entry.save_path.filter(|p| !p.is_empty()),
                })
                .collect())
        })
    }
}

/// The `name=value` part of a `Set-Cookie` header, when it's a plausible
/// session cookie.
fn session_cookie(header: &str) -> Option<Secret> {
    let pair = header.split(';').next()?.trim();
    let (name, value) = pair.split_once('=')?;
    let token = |s: &str, extra: &[u8]| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
    };
    (pair.len() <= 512 && token(name, b"_-.") && token(value, b"_-.+/=%:"))
        .then(|| Secret::new(pair))
}

/// A version string: one short printable line.
fn short_text(body: &[u8]) -> Result<String, ClientError> {
    let text = std::str::from_utf8(body)
        .map_err(|_| ClientError::Protocol("the version isn't text"))?
        .trim();
    let allowed = |b: u8| b.is_ascii_alphanumeric() || b"._+-".contains(&b);
    if text.is_empty() || text.len() > 64 || !text.bytes().all(allowed) {
        return Err(ClientError::Protocol("the version isn't a version"));
    }
    Ok(text.to_owned())
}

/// `torrents/add`'s 200 answer: `Ok.`, `Fails.`, or (5.1+) a JSON summary.
fn add_answer(body: &[u8]) -> Result<(), ClientError> {
    match body.trim_ascii() {
        b"Ok." => Ok(()),
        b"Fails." => Err(ClientError::Rejected(
            "nothing was added: the link or file is invalid, or the torrent is already there"
                .into(),
        )),
        json if json.starts_with(b"{") => {
            let v: serde_json::Value = serde_json::from_slice(json)
                .map_err(|_| ClientError::Protocol("the add answer"))?;
            let count = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
            if count("failure_count") > 0
                && count("success_count") == 0
                && count("pending_count") == 0
            {
                Err(ClientError::Rejected(
                    "nothing was added: the link or file is invalid, or the torrent is already there"
                        .into(),
                ))
            } else {
                Ok(())
            }
        }
        _ => Err(ClientError::Protocol("the add answer")),
    }
}

fn rejected(status: u16, body: &[u8]) -> ClientError {
    ClientError::Rejected(format!("HTTP {status}: {}", client_reason(body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookies() {
        let c = session_cookie(
            "SID=I2qCXYtOVgSWxVHIylT1bZt5a/Hf5Bvr; HttpOnly; path=/; SameSite=Strict",
        )
        .unwrap();
        assert_eq!(c.expose(), "SID=I2qCXYtOVgSWxVHIylT1bZt5a/Hf5Bvr");
        assert_eq!(
            session_cookie("QBT_SID_8080=abc").unwrap().expose(),
            "QBT_SID_8080=abc"
        );
        for bad in ["", "SID", "=x", "SID=", "SID=a b", "SID=a\r\nX: y", "S D=x"] {
            assert!(session_cookie(bad).is_none(), "{bad:?}");
        }
        assert!(session_cookie(&format!("SID={}", "a".repeat(600))).is_none());
    }

    #[test]
    fn add_answers() {
        assert_eq!(add_answer(b"Ok."), Ok(()));
        assert!(matches!(
            add_answer(b"Fails."),
            Err(ClientError::Rejected(_))
        ));
        assert_eq!(
            add_answer(br#"{"success_count":1,"failure_count":0,"pending_count":0}"#),
            Ok(())
        );
        assert!(matches!(
            add_answer(br#"{"success_count":0,"failure_count":2,"pending_count":0}"#),
            Err(ClientError::Rejected(_))
        ));
        assert_eq!(
            add_answer(b"<html>"),
            Err(ClientError::Protocol("the add answer"))
        );
        assert_eq!(
            add_answer(b"{oops"),
            Err(ClientError::Protocol("the add answer"))
        );
    }

    #[test]
    fn versions() {
        assert_eq!(short_text(b"v4.6.3").unwrap(), "v4.6.3");
        assert_eq!(short_text(b" 2.9.3\n").unwrap(), "2.9.3");
        assert!(short_text(b"").is_err());
        assert!(short_text(b"<html><body>").is_err());
        assert!(short_text(&[b'1'; 65]).is_err());
        assert!(short_text(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn urls_are_checked_and_normalised() {
        let q = QBittorrent::new("http://10.0.0.5:8080/qbt?x=1#y", "u".into(), "p".into()).unwrap();
        assert_eq!(q.base.as_str(), "http://10.0.0.5:8080/qbt/");
        assert_eq!(
            q.url("app/version").unwrap().as_str(),
            "http://10.0.0.5:8080/qbt/api/v2/app/version"
        );
        assert_eq!(q.origin, "http://10.0.0.5:8080");
        assert_eq!(q.referer, "http://10.0.0.5:8080/");
        assert!(QBittorrent::new("http://localhost:8080", "u".into(), "p".into()).is_ok());
        assert!(matches!(
            QBittorrent::new("http://169.254.169.254/", "u".into(), "p".into()),
            Err(ClientError::Blocked(crate::AddrClass::Metadata))
        ));
        assert!(matches!(
            QBittorrent::new("http://[fe80::1]:8080/", "u".into(), "p".into()),
            Err(ClientError::Blocked(crate::AddrClass::LinkLocal))
        ));
        assert!(matches!(
            QBittorrent::new("http://admin:pw@10.0.0.5/", "u".into(), "p".into()),
            Err(ClientError::InvalidRequest(_))
        ));
        let debug = format!("{q:?}");
        assert!(!debug.contains("qbt") && debug.contains("10.0.0.5"));
    }
}
