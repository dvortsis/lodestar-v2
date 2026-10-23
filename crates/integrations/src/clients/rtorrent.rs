//! rTorrent (and ruTorrent setups), over XML-RPC: rTorrent's SCGI port
//! behind a web server's `/RPC2` (§16 says why this path, not ruTorrent's
//! `httprpc` plugin).
//!
//! - **Authentication** is the web server's: optional HTTP basic auth.
//!   `401` is a wrong password, `403` an address the server refuses. There
//!   is no session.
//! - **Answers** are parsed by [`xmlrpc`](super::xmlrpc), which bounds size
//!   and nesting and refuses DTDs. A `<fault>` is the client saying no.
//! - **Adding** is `load.start` / `load.normal` (magnets) or
//!   `load.raw_start` / `load.raw` (`.torrent` bytes). The save path
//!   (`d.directory.set`) and the label (`d.custom1.set`, ruTorrent's label
//!   field, percent-encoded as ruTorrent stores it) go along as commands of
//!   the load: rTorrent keeps those for a magnet and applies them again
//!   when its metadata arrives, which values set afterwards wouldn't
//!   survive. rTorrent parses those commands, so the values are quoted with
//!   `"` and `\` escaped, and a value starting with `$` is refused: rTorrent
//!   evaluates a leading `$` even inside quotes, which would run a command.
//! - **Checking the add:** `load.*` answers 0 whatever happens (a corrupt
//!   file, or a magnet without trackers while DHT is off, is dropped in
//!   rTorrent's log only). So `d.hash` is asked before (an existing torrent
//!   is a duplicate) and polled after (the torrent must appear).
//! - **Selected files** (a `.torrent` only): loaded without starting;
//!   `d.is_meta` and `d.size_files` give the file count (refused past
//!   [`MAX_FILES`](super::MAX_FILES)), one `system.multicall` of
//!   `f.priority.set` 0 skips the rest, `d.update_priorities` applies it,
//!   and `d.start` starts it unless the user asked for paused. A stopped
//!   magnet fetches no metadata, so a magnet's files can't be selected
//!   before it starts: refused as [`Feature::MagnetFileSelection`] before
//!   any request.
//! - **Categories** are the labels in use (`d.custom1` of every download).
//!
//! Recorded against rTorrent 0.9.8 behind nginx 1.24; the fixtures are in
//! `tests/fixtures/rtorrent/`.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, Url};

use super::qbittorrent::upload_timeout;
use super::xmlrpc::{self, Call, Param, Response, Value};
use super::{
    AddOptions, AddOutcome, AuthFailure, BasicAuth, CLIENT_TIMEOUT, Capabilities, Category,
    ClientError, ClientInfo, ClientKind, DownloadClient, METADATA_WAIT, Magnet, POLL_INTERVAL,
    Selection, TorrentFile, already_there, category_name, check_file_count, check_magnets,
    check_request, client_reason, partial, poll, rejected_status, skipped_files,
};
use crate::BoxFuture;
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited, redact};

const CAPABILITIES: Capabilities = Capabilities {
    category: true,
    save_path: true,
    paused: true,
    file_selection: true,
    // A stopped magnet fetches no metadata, so its files can't be selected
    // before it starts (§16).
    magnet_file_selection: false,
    torrent_file: true,
    batch: false,
};

/// Most answers are a few hundred bytes.
const MAX_RESPONSE: usize = 1 << 20;
/// Every download's label.
const MAX_LIST: usize = 16 << 20;
/// File priorities are set this many files per `system.multicall`.
const PRIORITY_BATCH: usize = 1000;
/// rTorrent's fault for a target it doesn't have.
const NO_SUCH_TORRENT: i64 = -501;
/// The longest rTorrent takes to register a load (it's queued for its next
/// main-loop pass, normally milliseconds).
const LOAD_WAIT: Duration = Duration::from_secs(5);

pub struct RTorrent {
    endpoint: Url,
    auth: Option<BasicAuth>,
    http: GuardedClient,
    metadata_wait: Duration,
    poll_interval: Duration,
    timeout: Duration,
}

impl fmt::Debug for RTorrent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RTorrent")
            .field("url", &redact(&self.endpoint))
            .finish_non_exhaustive()
    }
}

/// What the request carried to add.
enum Source<'a> {
    Magnet(&'a str),
    File(&'a [u8]),
}

impl RTorrent {
    /// An adapter for the XML-RPC endpoint at `url`
    /// (`http://localhost/RPC2`; a bare address gets `/RPC2`).
    pub fn new(url: &str, auth: Option<BasicAuth>) -> Result<RTorrent, ClientError> {
        let http = GuardedClient::new(NetPolicy::download_client(), CLIENT_TIMEOUT)?;
        RTorrent::with_client(url, auth, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(
        url: &str,
        auth: Option<BasicAuth>,
        http: GuardedClient,
    ) -> Result<RTorrent, ClientError> {
        let mut endpoint = parse_url(url, http.policy())?;
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        if endpoint.path() == "/" {
            endpoint.set_path("/RPC2");
        }
        Ok(RTorrent {
            endpoint,
            auth,
            http,
            metadata_wait: METADATA_WAIT,
            poll_interval: POLL_INTERVAL,
            timeout: CLIENT_TIMEOUT,
        })
    }

    /// The timeout for one request (default [`CLIENT_TIMEOUT`]).
    pub fn with_timeout(mut self, timeout: Duration) -> RTorrent {
        self.timeout = timeout;
        self
    }

    /// How long to wait for a magnet's metadata before leaving a file
    /// selection [`Selection::Pending`], and how often to look (also how
    /// often a new load is looked for, for at most 5 s).
    pub fn with_metadata_wait(mut self, wait: Duration, poll: Duration) -> RTorrent {
        self.metadata_wait = wait;
        self.poll_interval = poll;
        self
    }

    /// Sends one XML-RPC document; the parsed answer.
    async fn send(
        &self,
        name: &'static str,
        body: String,
        limit: usize,
        timeout: Duration,
    ) -> Result<Response, ClientError> {
        let mut rb = self
            .http
            .request(Method::POST, self.endpoint.clone())
            .header(CONTENT_TYPE, "text/xml")
            .timeout(timeout)
            .body(body);
        if let Some(auth) = &self.auth {
            rb = rb.basic_auth(&auth.username, Some(auth.password.expose()));
        }
        let req = rb
            .build()
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let resp = self.http.send(req).await?;
        let status = resp.status().as_u16();
        lodestar_log::log_event!(
            crate::log_events::CLIENT_REQUEST,
            client = "rtorrent",
            method = name,
            status = lodestar_log::Count(status.into())
        );
        match status {
            401 => return Err(ClientError::Auth(AuthFailure::BadCredentials)),
            403 => return Err(ClientError::Auth(AuthFailure::AddressRefused)),
            _ => {}
        }
        let bytes = read_limited(resp, limit).await?;
        if status != 200 {
            return Err(rejected_status(status, &bytes));
        }
        xmlrpc::parse_response(&bytes).map_err(ClientError::Protocol)
    }

    /// One method call; a fault is a rejection.
    async fn call(
        &self,
        method: &'static str,
        params: &[Param<'_>],
        limit: usize,
    ) -> Result<Value, ClientError> {
        match self.call_raw(method, params, limit).await? {
            Response::Value(v) => Ok(v),
            Response::Fault(fault) => Err(fault_error(&fault)),
        }
    }

    async fn call_raw(
        &self,
        method: &'static str,
        params: &[Param<'_>],
        limit: usize,
    ) -> Result<Response, ClientError> {
        self.call_timeout(method, params, limit, self.timeout).await
    }

    /// [`call_raw`](Self::call_raw) with its own timeout (uploads get more).
    async fn call_timeout(
        &self,
        method: &'static str,
        params: &[Param<'_>],
        limit: usize,
        timeout: Duration,
    ) -> Result<Response, ClientError> {
        let body = xmlrpc::method_call(method, params).map_err(ClientError::InvalidRequest)?;
        self.send(method, body, limit, timeout).await
    }

    async fn int(&self, method: &'static str, hash: &str) -> Result<i64, ClientError> {
        self.call(method, &[Param::Str(hash)], MAX_RESPONSE)
            .await?
            .as_int()
            .ok_or(ClientError::Protocol("expected an integer"))
    }

    async fn exists(&self, hash: &str) -> Result<bool, ClientError> {
        match self
            .call_raw("d.hash", &[Param::Str(hash)], MAX_RESPONSE)
            .await?
        {
            Response::Value(_) => Ok(true),
            Response::Fault(f) if f.code == NO_SUCH_TORRENT => Ok(false),
            Response::Fault(f) => Err(fault_error(&f)),
        }
    }

    async fn text(&self, method: &'static str) -> Result<String, ClientError> {
        let v = self.call(method, &[], MAX_RESPONSE).await?;
        v.as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty() && v.len() <= 64 && !v.chars().any(char::is_control))
            .map(str::to_owned)
            .ok_or(ClientError::Protocol("the version isn't a version"))
    }

    /// Loads one torrent (`hash` uppercase) unless it's already there.
    /// `Ok(false)` for a duplicate.
    async fn add_one(
        &self,
        source: Source<'_>,
        hash: &str,
        opts: &AddOptions,
        start: bool,
    ) -> Result<bool, ClientError> {
        // Checked before any request, so a refused value sends nothing.
        let commands = load_commands(opts)?;
        if self.exists(hash).await? {
            return Ok(false);
        }
        let (method, data) = match (&source, start) {
            (Source::Magnet(uri), true) => ("load.start", Param::Str(uri)),
            (Source::Magnet(uri), false) => ("load.normal", Param::Str(uri)),
            (Source::File(bytes), true) => ("load.raw_start", Param::Base64(bytes)),
            (Source::File(bytes), false) => ("load.raw", Param::Base64(bytes)),
        };
        let timeout = match source {
            Source::File(bytes) => upload_timeout(self.timeout, bytes.len()),
            Source::Magnet(_) => self.timeout,
        };
        let mut params = vec![Param::Str(""), data];
        params.extend(commands.iter().map(|c| Param::Str(c)));
        if let Response::Fault(f) = self
            .call_timeout(method, &params, MAX_RESPONSE, timeout)
            .await?
        {
            return Err(fault_error(&f));
        }
        let wait = self.metadata_wait.min(LOAD_WAIT);
        let appeared = poll(wait, self.poll_interval, || async {
            Ok(self.exists(hash).await?.then_some(()))
        })
        .await?;
        if appeared.is_none() {
            return Err(ClientError::Rejected(match source {
                Source::Magnet(_) => "rTorrent didn't add the magnet; its log says why (without DHT, a magnet needs a tracker)".into(),
                Source::File(_) => "rTorrent didn't add the .torrent file; its log says why".into(),
            }));
        }
        Ok(true)
    }

    /// The file count once a magnet's metadata is there.
    async fn file_count(&self, hash: &str) -> Result<Option<usize>, ClientError> {
        if self.int("d.is_meta", hash).await? != 0 {
            return Ok(None);
        }
        let count = self.int("d.size_files", hash).await?;
        let count = usize::try_from(count)
            .ok()
            .filter(|&n| n > 0)
            .ok_or(ClientError::Protocol("a torrent without files"))?;
        check_file_count(count).map(Some)
    }

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
            lodestar_log::log_event!(crate::log_events::CLIENT_LEFT_PAUSED, client = "rtorrent");
            return Ok(Selection::Pending);
        };
        let skipped = skipped_files(selected, count)?;
        if !skipped.is_empty() {
            let targets: Vec<String> = skipped.iter().map(|i| format!("{hash}:f{i}")).collect();
            for batch in targets.chunks(PRIORITY_BATCH) {
                let calls: Vec<Call<'_>> = batch
                    .iter()
                    .map(|t| ("f.priority.set", vec![Param::Str(t), Param::Int(0)]))
                    .collect();
                let body = xmlrpc::multicall(&calls).map_err(ClientError::InvalidRequest)?;
                let results = match self
                    .send("system.multicall", body, MAX_RESPONSE, self.timeout)
                    .await?
                {
                    Response::Value(v) => v,
                    Response::Fault(f) => return Err(fault_error(&f)),
                };
                let results = results
                    .as_array()
                    .filter(|r| r.len() == batch.len())
                    .ok_or(ClientError::Protocol("system.multicall's results"))?;
                if let Some(fault) = results.iter().find_map(Value::as_fault) {
                    return Err(fault_error(&fault));
                }
            }
            self.call("d.update_priorities", &[Param::Str(hash)], MAX_RESPONSE)
                .await?;
        }
        if start {
            self.call("d.start", &[Param::Str(hash)], MAX_RESPONSE)
                .await?;
        }
        Ok(Selection::Applied)
    }

    async fn add_single(
        &self,
        source: Source<'_>,
        id: &str,
        opts: &AddOptions,
    ) -> Result<AddOutcome, ClientError> {
        let selection = match source {
            Source::Magnet(_) => check_magnets(CAPABILITIES, opts, 1)?,
            Source::File(_) => check_request(CAPABILITIES, opts, 1)?,
        };
        let hash = id.to_ascii_uppercase();
        let start = !opts.paused && selection.is_none();
        if !self.add_one(source, &hash, opts, start).await? {
            return Err(already_there());
        }
        let selection = match selection {
            Some(selected) => self.select_files(&hash, &selected, !opts.paused).await?,
            None => Selection::None,
        };
        Ok(AddOutcome { sent: 1, selection })
    }
}

impl DownloadClient for RTorrent {
    fn kind(&self) -> ClientKind {
        ClientKind::RTorrent
    }

    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }

    fn test(&self) -> BoxFuture<'_, Result<ClientInfo, ClientError>> {
        Box::pin(async move {
            let version = self.text("system.client_version").await?;
            let library = self.text("system.library_version").await?;
            Ok(ClientInfo {
                kind: ClientKind::RTorrent,
                version,
                api_version: Some(library),
            })
        })
    }

    fn add_magnets<'a>(
        &'a self,
        magnets: &'a [Magnet],
        opts: &'a AddOptions,
    ) -> BoxFuture<'a, Result<AddOutcome, ClientError>> {
        Box::pin(async move {
            if let [magnet] = magnets {
                return self
                    .add_single(Source::Magnet(magnet.as_str()), magnet.torrent_id(), opts)
                    .await;
            }
            check_magnets(CAPABILITIES, opts, magnets.len())?;
            // Bulk: one load each. A duplicate is already where the user
            // wants it; only all-duplicates is refused. A failure part-way
            // says how many went in.
            let mut new = 0;
            for (done, magnet) in magnets.iter().enumerate() {
                let hash = magnet.torrent_id().to_ascii_uppercase();
                let added = self
                    .add_one(Source::Magnet(magnet.as_str()), &hash, opts, !opts.paused)
                    .await
                    .map_err(|e| partial(done, e))?;
                new += usize::from(added);
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
            self.add_single(Source::File(&file.bytes), &file.info_hash, opts)
                .await
        })
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>, ClientError>> {
        Box::pin(async move {
            let rows = self
                .call(
                    "d.multicall2",
                    &[Param::Str(""), Param::Str("main"), Param::Str("d.custom1=")],
                    MAX_LIST,
                )
                .await?;
            let rows = rows
                .as_array()
                .ok_or(ClientError::Protocol("d.multicall2's rows"))?;
            let mut labels = BTreeSet::new();
            for row in rows {
                if let Some(label) = row
                    .as_array()
                    .and_then(|r| r.first())
                    .and_then(Value::as_str)
                    .and_then(|l| category_name(&percent_decode(l)))
                {
                    labels.insert(label);
                }
            }
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

fn fault_error(fault: &xmlrpc::Fault) -> ClientError {
    // rTorrent itself has no authentication; a proxy in front of it may
    // answer a refused login as a fault.
    if matches!(fault.code, 401 | 403) || fault.message.eq_ignore_ascii_case("unauthorized") {
        return ClientError::Auth(AuthFailure::BadCredentials);
    }
    ClientError::Rejected(client_reason(fault.message.as_bytes()))
}

/// The commands a load carries for the save path and label.
fn load_commands(opts: &AddOptions) -> Result<Vec<String>, ClientError> {
    let mut commands = Vec::new();
    if let Some(path) = &opts.save_path {
        commands.push(format!("d.directory.set={}", quote(path)?));
    }
    if let Some(label) = &opts.category {
        commands.push(format!("d.custom1.set={}", quote(&percent_encode(label))?));
    }
    Ok(commands)
}

/// A value quoted for rTorrent's command parser: `"…"`, with `\` and `"`
/// escaped. A leading `$` is refused: rTorrent evaluates it as a command
/// even inside quotes (checked against 0.9.8). Control characters were
/// refused by `check_request`.
fn quote(value: &str) -> Result<String, ClientError> {
    if value.starts_with('$') {
        return Err(ClientError::InvalidRequest(
            "rTorrent can't take a save path or label starting with $",
        ));
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Ok(out)
}

/// Percent-encodes all but unreserved characters, as ruTorrent stores
/// labels in `d.custom1` (PHP's `rawurlencode`).
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes a `d.custom1` label; text that isn't percent-encoded UTF-8 is
/// kept as it is.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(b) = value
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_quoted_for_rtorrents_parser() {
        assert_eq!(quote("/data/a,b;c x").unwrap(), r#""/data/a,b;c x""#);
        assert_eq!(quote(r#"a"b\c"#).unwrap(), r#""a\"b\\c""#);
        assert_eq!(quote("/x/$HOME").unwrap(), r#""/x/$HOME""#);
        assert!(matches!(
            quote("$execute=rm"),
            Err(ClientError::InvalidRequest(_))
        ));
        let opts = AddOptions {
            save_path: Some("/data/tv".into()),
            category: Some("TV Shows/HD".into()),
            ..AddOptions::default()
        };
        assert_eq!(
            load_commands(&opts).unwrap(),
            vec![
                r#"d.directory.set="/data/tv""#.to_owned(),
                r#"d.custom1.set="TV%20Shows%2FHD""#.to_owned()
            ]
        );
        // A label can't inject: percent-encoding leaves no $, quote or comma.
        let opts = AddOptions {
            category: Some("$execute=rm,-rf,/".into()),
            ..AddOptions::default()
        };
        assert_eq!(
            load_commands(&opts).unwrap(),
            vec![r#"d.custom1.set="%24execute%3Drm%2C-rf%2C%2F""#.to_owned()]
        );
    }

    #[test]
    fn labels_round_trip_percent_encoding() {
        for label in ["Movies", "TV Shows/HD", "Séries", "100%", "a+b"] {
            assert_eq!(percent_decode(&percent_encode(label)), label);
        }
        assert_eq!(percent_decode("plain label"), "plain label");
        assert_eq!(percent_decode("bad%zz%"), "bad%zz%");
        assert_eq!(percent_decode("%FF"), "%FF");
    }

    #[test]
    fn faults_map_to_errors() {
        let fault = |code, message: &str| xmlrpc::Fault {
            code,
            message: message.into(),
        };
        assert_eq!(
            fault_error(&fault(-506, "Method 'x' not defined")),
            ClientError::Rejected("Method 'x' not defined".into())
        );
        assert_eq!(
            fault_error(&fault(401, "Unauthorized")),
            ClientError::Auth(AuthFailure::BadCredentials)
        );
        assert_eq!(
            fault_error(&fault(1, "unauthorized")),
            ClientError::Auth(AuthFailure::BadCredentials)
        );
    }

    #[test]
    fn urls() {
        let r = |u: &str| RTorrent::new(u, None).unwrap().endpoint.to_string();
        assert_eq!(r("http://10.0.0.5"), "http://10.0.0.5/RPC2");
        assert_eq!(
            r("http://10.0.0.5/rutorrent/RPC2?x"),
            "http://10.0.0.5/rutorrent/RPC2"
        );
    }
}
