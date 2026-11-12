//! SABnzbd, its HTTP API at `<web interface>/api`.
//!
//! - **Every call is a `POST`** with the parameters in the body (a form, or
//!   multipart for an upload), never in the query string, so the API key
//!   doesn't sit in a URL that proxies or the client's access log record.
//!   SABnzbd merges body and query parameters (CherryPy's `kwargs`). It has
//!   no header for the key.
//! - **Authentication** is `apikey`. SABnzbd answers a missing or wrong key
//!   with HTTP 403 and the text `API Key Required` or `API Key Incorrect`
//!   (an empty body when its `api_warnings` is off). Its other 403s are an
//!   address outside `inet_exposure` (`External internet access denied`)
//!   and a host name not in `host_whitelist` (`Access denied - Hostname
//!   verification failed`). There is no session.
//! - **Test** is `mode=version` (no key needed), then `mode=auth&key=…`,
//!   which says whether the key is the API key, the NZB key, or neither,
//!   without changing anything.
//! - **Adding** is `mode=addfile` with the NZB as the multipart `name`
//!   file, plus `cat` and `priority`. The answer is
//!   `{"status": true, "nzo_ids": ["SABnzbd_nzo_…"]}`; `status: false` is a
//!   refusal (a duplicate, an unreadable NZB). Never `addurl`: see
//!   [`super::usenet`].
//! - **Priorities:** -1 low, 0 normal, 1 high, 2 force, and -2 is "paused",
//!   which replaces the priority. No priority sends nothing (SABnzbd's -100,
//!   the category's default).
//! - **Categories** are `mode=get_cats` (`{"categories": ["*", …]}`; `*` is
//!   the default and isn't listed). It needs the full API key; the NZB key
//!   gets 403 `API Key Incorrect`.
//!
//! Written against SABnzbd 4.5.5's source (`sabnzbd/api.py`,
//! `sabnzbd/interface.py`); the fixtures in
//! `fixtures/usenet_clients/sabnzbd/` follow it. The develop branch (5.0)
//! keeps these modes, keys and answers.

use std::fmt;
use std::time::Duration;

use reqwest::header::ACCEPT;
use reqwest::multipart::{Form, Part};
use reqwest::{Method, Url};
use serde::Deserialize;

use super::qbittorrent::upload_timeout;
use super::usenet::{
    Access, JobId, Nzb, NzbOptions, NzbPriority, UsenetClient, UsenetInfo, UsenetKind, redirected,
    version_text,
};
use super::{AuthFailure, CLIENT_TIMEOUT, Category, ClientError, category_name, client_reason};
use crate::BoxFuture;
use crate::clients::rejected_status;
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited, redact};
use crate::secret::Secret;

/// Every SABnzbd answer this adapter reads is a small JSON object.
pub const MAX_ANSWER: usize = 1 << 20;
/// The most categories kept from a list.
pub const MAX_CATEGORIES: usize = 10_000;
/// The longest job id accepted (`SABnzbd_nzo_` and 8 characters today).
const MAX_JOB_ID: usize = 64;

pub struct Sabnzbd {
    endpoint: Url,
    api_key: Secret,
    http: GuardedClient,
    timeout: Duration,
}

impl fmt::Debug for Sabnzbd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sabnzbd")
            .field("url", &redact(&self.endpoint))
            .finish_non_exhaustive()
    }
}

enum Body<'a> {
    Form(Vec<(&'static str, &'a str)>),
    Upload(Vec<(&'static str, String)>, &'a Nzb),
}

impl Sabnzbd {
    /// An adapter for the web interface at `url` (`http://host:8080/`, a
    /// reverse-proxy path such as `http://host/sabnzbd/`, or the full
    /// `…/api` URL). A query string is dropped.
    pub fn new(url: &str, api_key: Secret) -> Result<Sabnzbd, ClientError> {
        let http = GuardedClient::new(NetPolicy::download_client(), CLIENT_TIMEOUT)?;
        Sabnzbd::with_client(url, api_key, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(
        url: &str,
        api_key: Secret,
        http: GuardedClient,
    ) -> Result<Sabnzbd, ClientError> {
        let key = api_key.expose();
        if key.is_empty()
            || key.len() > 128
            || key.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(ClientError::InvalidRequest(
                "the API key is empty, too long, or has spaces",
            ));
        }
        Ok(Sabnzbd {
            endpoint: api_url(url, http.policy())?,
            api_key,
            http,
            timeout: CLIENT_TIMEOUT,
        })
    }

    /// The timeout for one request (default [`CLIENT_TIMEOUT`]). An upload
    /// gets this plus time for its size.
    pub fn with_timeout(mut self, timeout: Duration) -> Sabnzbd {
        self.timeout = timeout;
        self
    }

    /// One `POST` to the API; the status and the body, read within
    /// [`MAX_ANSWER`].
    async fn call(
        &self,
        mode: &'static str,
        body: Body<'_>,
    ) -> Result<(u16, Vec<u8>), ClientError> {
        let rb = self
            .http
            .request(Method::POST, self.endpoint.clone())
            .header(ACCEPT, "application/json")
            .timeout(self.timeout);
        let rb = match body {
            Body::Form(mut fields) => {
                fields.extend([("output", "json"), ("mode", mode)]);
                rb.form(&fields)
            }
            Body::Upload(fields, nzb) => {
                let part = Part::bytes(nzb.bytes().to_vec())
                    .file_name(nzb.file_name().to_owned())
                    .mime_str("application/x-nzb")
                    .map_err(|_| ClientError::Protocol("couldn't build the upload"))?;
                // The name is cleaned (no quotes or control characters), and
                // SABnzbd reads it as raw UTF-8, not percent-encoded.
                let mut form = Form::new()
                    .percent_encode_noop()
                    .text("output", "json")
                    .text("mode", mode);
                for (name, value) in fields {
                    form = form.text(name, value);
                }
                rb.timeout(upload_timeout(self.timeout, nzb.bytes().len()))
                    .multipart(form.part("name", part))
            }
        };
        let req = rb
            .build()
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let resp = self.http.send(req).await?;
        let status = resp.status().as_u16();
        let bytes = read_limited(resp, MAX_ANSWER).await?;
        lodestar_log::log_event!(
            crate::log_events::CLIENT_REQUEST,
            client = "sabnzbd",
            method = mode,
            status = lodestar_log::Count(status.into())
        );
        Ok((status, bytes))
    }

    async fn version(&self) -> Result<String, ClientError> {
        let (status, body) = self.call("version", Body::Form(Vec::new())).await?;
        parse_version(status, &body)
    }

    async fn access(&self) -> Result<Access, ClientError> {
        let fields = vec![("key", self.api_key.expose())];
        let (status, body) = self.call("auth", Body::Form(fields)).await?;
        parse_auth(status, &body)
    }

    async fn add(&self, nzb: &Nzb, opts: &NzbOptions) -> Result<JobId, ClientError> {
        opts.check()?;
        let mut fields = vec![("apikey", self.api_key.expose().to_owned())];
        if let Some(category) = &opts.category {
            fields.push(("cat", category.clone()));
        }
        let priority = if opts.paused {
            Some(-2)
        } else {
            opts.priority.map(|p| match p {
                NzbPriority::Low => -1,
                NzbPriority::Normal => 0,
                NzbPriority::High => 1,
                NzbPriority::Force => 2,
            })
        };
        if let Some(p) = priority {
            fields.push(("priority", p.to_string()));
        }
        let (status, body) = self.call("addfile", Body::Upload(fields, nzb)).await?;
        parse_add(status, &body)
    }

    async fn list_categories(&self) -> Result<Vec<Category>, ClientError> {
        let fields = vec![("apikey", self.api_key.expose())];
        let (status, body) = self.call("get_cats", Body::Form(fields)).await?;
        parse_categories(status, &body)
    }
}

impl UsenetClient for Sabnzbd {
    fn kind(&self) -> UsenetKind {
        UsenetKind::Sabnzbd
    }

    fn test(&self) -> BoxFuture<'_, Result<UsenetInfo, ClientError>> {
        Box::pin(async move {
            let version = self.version().await?;
            let access = self.access().await?;
            Ok(UsenetInfo {
                kind: UsenetKind::Sabnzbd,
                version,
                access,
            })
        })
    }

    fn add_nzb<'a>(
        &'a self,
        nzb: &'a Nzb,
        opts: &'a NzbOptions,
    ) -> BoxFuture<'a, Result<JobId, ClientError>> {
        Box::pin(self.add(nzb, opts))
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>, ClientError>> {
        Box::pin(self.list_categories())
    }
}

fn api_url(url: &str, policy: NetPolicy) -> Result<Url, ClientError> {
    let mut u = parse_url(url, policy)?;
    u.set_query(None);
    u.set_fragment(None);
    let path = u.path().trim_end_matches('/').to_owned();
    if path.ends_with("/api") {
        u.set_path(&path);
    } else {
        u.set_path(&format!("{path}/api"));
    }
    Ok(u)
}

/// Every field any answer this adapter reads may carry; the rest are
/// ignored. A field of the wrong type fails the whole answer.
#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    status: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    auth: Option<String>,
    #[serde(default)]
    categories: Option<Vec<String>>,
    #[serde(default)]
    nzo_ids: Option<Vec<String>>,
}

/// The HTTP status and JSON of an answer, or why it isn't one.
fn reply(status: u16, body: &[u8]) -> Result<Reply, ClientError> {
    match status {
        200 => {}
        300..=399 => return Err(redirected()),
        401 => return Err(ClientError::Auth(AuthFailure::BadCredentials)),
        403 => return Err(forbidden(body)),
        _ => return Err(rejected_status(status, body)),
    }
    let reply: Reply = serde_json::from_slice(body)
        .map_err(|_| ClientError::Protocol("the answer isn't SABnzbd's JSON"))?;
    if let Some(error) = &reply.error {
        return Err(refusal(error));
    }
    Ok(reply)
}

/// SABnzbd's 403s, told apart by their text.
fn forbidden(body: &[u8]) -> ClientError {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() || text.starts_with("API Key") {
        ClientError::Auth(AuthFailure::BadCredentials)
    } else if text.starts_with("External internet access denied") {
        ClientError::Auth(AuthFailure::AddressRefused)
    } else if text.starts_with("Access denied - Hostname verification failed") {
        ClientError::Auth(AuthFailure::OriginRefused)
    } else {
        rejected_status(403, body)
    }
}

fn refusal(error: &str) -> ClientError {
    if error.starts_with("API Key") {
        ClientError::Auth(AuthFailure::BadCredentials)
    } else {
        ClientError::Rejected(client_reason(error.as_bytes()))
    }
}

/// Reads a `mode=version` answer. Public for the fuzz target.
pub fn parse_version(status: u16, body: &[u8]) -> Result<String, ClientError> {
    reply(status, body)?
        .version
        .as_deref()
        .and_then(version_text)
        .ok_or(ClientError::Protocol("the answer has no version"))
}

/// Reads a `mode=auth` answer. Public for the fuzz target.
pub fn parse_auth(status: u16, body: &[u8]) -> Result<Access, ClientError> {
    match reply(status, body)?.auth.as_deref() {
        Some("apikey") => Ok(Access::Full),
        Some("nzbkey") => Ok(Access::AddOnly),
        Some("badkey") => Err(ClientError::Auth(AuthFailure::BadCredentials)),
        _ => Err(ClientError::Protocol("the answer has no key check")),
    }
}

/// Reads a `mode=addfile` answer: exactly one job id. Public for the fuzz
/// target.
pub fn parse_add(status: u16, body: &[u8]) -> Result<JobId, ClientError> {
    let reply = reply(status, body)?;
    match reply.status {
        Some(true) => {}
        Some(false) => return Err(ClientError::Rejected("the client refused the NZB".into())),
        None => return Err(ClientError::Protocol("the add answer has no status")),
    }
    match reply.nzo_ids.as_deref() {
        Some([id]) if valid_job_id(id) => Ok(JobId::new(id.clone())),
        Some([_]) => Err(ClientError::Protocol("the job id is malformed")),
        _ => Err(ClientError::Protocol("the add didn't answer one job id")),
    }
}

fn valid_job_id(id: &str) -> bool {
    (1..=MAX_JOB_ID).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Reads a `mode=get_cats` answer: the names, cleaned, without the default
/// `*`, sorted and de-duplicated, at most [`MAX_CATEGORIES`]. Public for the
/// fuzz target.
pub fn parse_categories(status: u16, body: &[u8]) -> Result<Vec<Category>, ClientError> {
    let names = reply(status, body)?
        .categories
        .ok_or(ClientError::Protocol("the answer has no categories"))?;
    if names.len() > MAX_CATEGORIES {
        return Err(ClientError::Protocol(
            "the client listed too many categories",
        ));
    }
    let mut names: Vec<String> = names
        .iter()
        .filter(|n| n.as_str() != "*")
        .filter_map(|n| category_name(n))
        .collect();
    names.sort();
    names.dedup();
    Ok(names
        .into_iter()
        .map(|name| Category {
            name,
            save_path: None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn the_api_url_is_derived() {
        let url = |raw: &str| {
            api_url(raw, NetPolicy::download_client())
                .unwrap()
                .to_string()
        };
        assert_eq!(url("http://sab.lan:8080"), "http://sab.lan:8080/api");
        assert_eq!(url("http://sab.lan:8080/"), "http://sab.lan:8080/api");
        assert_eq!(url("http://h/sabnzbd/"), "http://h/sabnzbd/api");
        assert_eq!(url("http://h/sabnzbd/api"), "http://h/sabnzbd/api");
        assert_eq!(url("http://h/api/?apikey=x#f"), "http://h/api");
        assert!(api_url("http://169.254.169.254/", NetPolicy::download_client()).is_err());
    }

    #[test]
    fn a_bad_key_is_refused_at_construction() {
        for key in ["", "a b", "a\nb", &"k".repeat(129)] {
            let e = Sabnzbd::new("http://sab.lan:8080/", Secret::new(key)).unwrap_err();
            assert!(matches!(e, ClientError::InvalidRequest(_)), "{key:?}");
        }
    }

    #[test]
    fn answers_are_parsed_strictly() {
        assert_eq!(
            parse_version(200, br#"{"version": "4.5.5"}"#).unwrap(),
            "4.5.5"
        );
        assert!(parse_version(200, br#"{"version": 4}"#).is_err());
        assert!(parse_version(200, br#"{"version": ""}"#).is_err());
        assert_eq!(
            parse_auth(200, br#"{"auth": "nzbkey"}"#),
            Ok(Access::AddOnly)
        );
        assert_eq!(
            parse_auth(200, br#"{"auth": "badkey"}"#),
            Err(ClientError::Auth(AuthFailure::BadCredentials))
        );
        assert_eq!(
            parse_add(
                200,
                br#"{"status": true, "nzo_ids": ["SABnzbd_nzo_p86tgx"]}"#
            )
            .unwrap()
            .as_str(),
            "SABnzbd_nzo_p86tgx"
        );
        assert!(parse_add(200, br#"{"status": true, "nzo_ids": []}"#).is_err());
        assert!(parse_add(200, br#"{"status": true, "nzo_ids": ["a", "b"]}"#).is_err());
        assert!(parse_add(200, br#"{"status": true, "nzo_ids": ["a b"]}"#).is_err());
        assert!(parse_add(200, br#"{"nzo_ids": ["a"]}"#).is_err());
        assert_eq!(
            parse_add(
                200,
                br#"{"status": false, "error": "expects one parameter"}"#
            ),
            Err(ClientError::Rejected("expects one parameter".into()))
        );
        assert_eq!(
            parse_add(
                403,
                b"External internet access denied - https://sabnzbd.org/access-denied"
            ),
            Err(ClientError::Auth(AuthFailure::AddressRefused))
        );
        assert_eq!(
            parse_add(403, b""),
            Err(ClientError::Auth(AuthFailure::BadCredentials))
        );
        assert_eq!(parse_add(302, b""), Err(redirected()));
        let cats = parse_categories(
            200,
            br#"{"categories": ["*", "tv", "movies", "tv", "\u0007"]}"#,
        )
        .unwrap();
        let names: Vec<_> = cats.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["movies", "tv"]);
    }

    #[test]
    fn error_text_is_bounded() {
        let long = format!(r#"{{"status": false, "error": "{}"}}"#, "x".repeat(5000));
        let ClientError::Rejected(why) = parse_add(200, long.as_bytes()).unwrap_err() else {
            panic!()
        };
        assert!(why.len() <= 200);
        let ClientError::Rejected(why) = parse_add(500, "y".repeat(5000).as_bytes()).unwrap_err()
        else {
            panic!()
        };
        assert!(why.len() <= 220, "{}", why.len());
    }

    proptest! {
        #[test]
        fn parsers_never_panic(status in prop_oneof![Just(200u16), Just(403), any::<u16>()],
                               body in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = parse_version(status, &body);
            let _ = parse_auth(status, &body);
            let _ = parse_add(status, &body);
            let _ = parse_categories(status, &body);
        }

        #[test]
        fn job_ids_are_clean(id in ".{0,80}") {
            let body = serde_json::json!({"status": true, "nzo_ids": [id]}).to_string();
            if let Ok(job) = parse_add(200, body.as_bytes()) {
                prop_assert!(job.as_str().len() <= MAX_JOB_ID);
                prop_assert!(job.as_str().bytes().all(|b| b.is_ascii_graphic()));
            }
        }
    }
}
