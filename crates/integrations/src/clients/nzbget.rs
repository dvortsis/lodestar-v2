//! NZBGet, JSON-RPC over HTTP at `<web interface>/jsonrpc`.
//!
//! - **Authentication** is HTTP Basic, the header marked sensitive. NZBGet
//!   also takes the credentials in the URL's path (`/user:pass/jsonrpc`),
//!   which this adapter never uses: a path ends up in logs. A wrong user
//!   name or password is HTTP 401 with an empty body. NZBGet has three
//!   users: control (everything), restricted (no passwords in `config`)
//!   and add (only `append`, `appendurl` and `version`; anything else is
//!   the JSON-RPC error 401 `Access denied`). Empty credentials send no
//!   header, for an NZBGet without a password.
//! - **Answers** are `{"version": "1.1", "id": …, "result": …}` or
//!   `{"error": {"name": "JSONRPCError", "code": …, "message": …}}`. NZBGet
//!   echoes the request's `id`; an answer for another id is unusable.
//!   NZBGet finds `method` and `params` by searching the request text, so
//!   the body is written with `id`, `method` and `params` in that order.
//! - **Test** is `version`, which every user may call, so it checks the
//!   credentials too.
//! - **Adding** is `append` with the v13+ parameters: file name, the NZB as
//!   base64, category, priority, add-to-top (false), paused, dupe key (""),
//!   dupe score (0) and dupe mode (`SCORE`). The answer is the new `NZBID`,
//!   or 0 or less when NZBGet refused the file. NZBGet reads content that
//!   starts with `http://` or `https://` as a URL to fetch; base64 never
//!   does, and this adapter never sends a URL (see [`super::usenet`]).
//! - **Priorities:** -50 low, 0 normal, 50 high, 900 force. No priority
//!   sends 0.
//! - **Categories** are the `CategoryN.Name` options of `config`. That
//!   answer is the whole configuration, including the client's own server
//!   passwords for the control user, so the parser keeps only category
//!   names, and the adapter's copy of the answer is zeroized once parsed
//!   (the HTTP stack's own buffers aren't).
//!
//! Written against nzbget-ng v24.8's source (`daemon/remote/XmlRpc.cpp`,
//! `WebServer.cpp`), checked against v26.3, which only adds an optional
//! trailing `autoCategory` to `append`; the fixtures in
//! `tests/fixtures/usenet_clients/nzbget/` follow it.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Method, Url};
use serde::de::{DeserializeOwned, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::qbittorrent::upload_timeout;
use super::usenet::{
    Access, JobId, Nzb, NzbOptions, NzbPriority, UsenetClient, UsenetInfo, UsenetKind, redirected,
    scrub, version_text,
};
use super::{
    AuthFailure, CLIENT_TIMEOUT, Category, ClientError, base64, category_name, client_reason,
    rejected_status,
};
use crate::BoxFuture;
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited, redact};
use crate::secret::{Secret, Zeroizing};

/// `version` and `append` answers are a few dozen bytes.
pub const MAX_ANSWER: usize = 1 << 20;
/// A `config` answer: a few hundred options, plus scripts' own.
pub const MAX_CONFIG: usize = 4 << 20;
/// The most categories kept from a configuration.
pub const MAX_CATEGORIES: usize = 10_000;
/// The longest `user:password` sent. NZBGet decodes Basic credentials into
/// 257 bytes and refuses base64 longer than that (256 characters is 192
/// bytes); it then drops the request without an answer.
const MAX_CREDENTIALS: usize = 192;

pub struct Nzbget {
    endpoint: Url,
    username: Secret,
    password: Secret,
    http: GuardedClient,
    next_id: AtomicU64,
    timeout: Duration,
}

impl fmt::Debug for Nzbget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Nzbget")
            .field("url", &redact(&self.endpoint))
            .finish_non_exhaustive()
    }
}

/// A request, its fields in the order NZBGet needs.
#[derive(Serialize)]
struct Call<'a> {
    id: u64,
    method: &'static str,
    params: &'a [Value],
}

impl Nzbget {
    /// An adapter for the web interface at `url` (`http://host:6789/`, a
    /// reverse-proxy path, or the full `…/jsonrpc` URL). A query string is
    /// dropped. Empty credentials send no `Authorization` header.
    pub fn new(url: &str, username: Secret, password: Secret) -> Result<Nzbget, ClientError> {
        let http = GuardedClient::new(NetPolicy::download_client(), CLIENT_TIMEOUT)?;
        Nzbget::with_client(url, username, password, http)
    }

    /// The same, over a given client (tests inject a resolver this way).
    pub fn with_client(
        url: &str,
        username: Secret,
        password: Secret,
        http: GuardedClient,
    ) -> Result<Nzbget, ClientError> {
        let (user, pass) = (username.expose(), password.expose());
        if user.len() + 1 + pass.len() > MAX_CREDENTIALS
            || user.contains(':')
            || user.contains(char::is_control)
            || pass.contains(char::is_control)
        {
            return Err(ClientError::InvalidRequest(
                "the user name and password are over 191 bytes together, or have a colon or control characters",
            ));
        }
        Ok(Nzbget {
            endpoint: rpc_url(url, http.policy())?,
            username,
            password,
            http,
            next_id: AtomicU64::new(1),
            timeout: CLIENT_TIMEOUT,
        })
    }

    /// The timeout for one request (default [`CLIENT_TIMEOUT`]). An upload
    /// gets this plus time for its size.
    pub fn with_timeout(mut self, timeout: Duration) -> Nzbget {
        self.timeout = timeout;
        self
    }

    /// One call; its id, the status and the body read within `limit`.
    async fn call(
        &self,
        method: &'static str,
        params: &[Value],
        limit: usize,
    ) -> Result<(u64, u16, Zeroizing<Vec<u8>>), ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = serde_json::to_vec(&Call { id, method, params })
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let mut rb = self
            .http
            .request(Method::POST, self.endpoint.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .timeout(upload_timeout(self.timeout, body.len()));
        if !self.username.is_empty() || !self.password.is_empty() {
            // reqwest marks the header sensitive.
            rb = rb.basic_auth(self.username.expose(), Some(self.password.expose()));
        }
        let req = rb
            .body(body)
            .build()
            .map_err(|_| ClientError::Protocol("couldn't build the request"))?;
        let resp = self.http.send(req).await?;
        let status = resp.status().as_u16();
        let mut bytes = Zeroizing::new(read_limited(resp, limit).await?);
        scrub(&mut bytes, self.password.expose());
        scrub(&mut bytes, self.username.expose());
        lodestar_log::log_event!(
            crate::log_events::CLIENT_REQUEST,
            client = "nzbget",
            method = method,
            status = lodestar_log::Count(status.into())
        );
        Ok((id, status, bytes))
    }

    async fn version(&self) -> Result<String, ClientError> {
        let (id, status, body) = self.call("version", &[], MAX_ANSWER).await?;
        parse_version(id, status, &body)
    }

    async fn add(&self, nzb: &Nzb, opts: &NzbOptions) -> Result<JobId, ClientError> {
        opts.check()?;
        let priority = match opts.priority {
            Some(NzbPriority::Low) => -50,
            None | Some(NzbPriority::Normal) => 0,
            Some(NzbPriority::High) => 50,
            Some(NzbPriority::Force) => 900,
        };
        let params = [
            json!(nzb.file_name()),
            Value::String(base64(nzb.bytes())),
            json!(opts.category.as_deref().unwrap_or("")),
            json!(priority),
            json!(false),
            json!(opts.paused),
            json!(""),
            json!(0),
            json!("SCORE"),
        ];
        let (id, status, body) = self.call("append", &params, MAX_ANSWER).await?;
        parse_append(id, status, &body)
    }

    async fn list_categories(&self) -> Result<Vec<Category>, ClientError> {
        let (id, status, body) = self.call("config", &[], MAX_CONFIG).await?;
        parse_categories(id, status, &body)
    }
}

impl UsenetClient for Nzbget {
    fn kind(&self) -> UsenetKind {
        UsenetKind::Nzbget
    }

    fn test(&self) -> BoxFuture<'_, Result<UsenetInfo, ClientError>> {
        Box::pin(async move {
            Ok(UsenetInfo {
                kind: UsenetKind::Nzbget,
                version: self.version().await?,
                access: Access::Unknown,
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

fn rpc_url(url: &str, policy: NetPolicy) -> Result<Url, ClientError> {
    let mut u = parse_url(url, policy)?;
    u.set_query(None);
    u.set_fragment(None);
    let path = u.path().trim_end_matches('/').to_owned();
    if path.ends_with("/jsonrpc") {
        u.set_path(&path);
    } else {
        u.set_path(&format!("{path}/jsonrpc"));
    }
    Ok(u)
}

#[derive(Deserialize)]
struct Answer<T> {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default = "none")]
    result: Option<T>,
    #[serde(default)]
    error: Option<RpcError>,
}

fn none<T>() -> Option<T> {
    None
}

#[derive(Deserialize)]
struct RpcError {
    #[serde(default)]
    message: Option<String>,
}

/// The result of an answer to call `id`, or why there's none.
fn answer<T: DeserializeOwned>(id: u64, status: u16, body: &[u8]) -> Result<T, ClientError> {
    match status {
        200 => {}
        300..=399 => return Err(redirected()),
        401 => return Err(ClientError::Auth(AuthFailure::BadCredentials)),
        _ => return Err(rejected_status(status, body)),
    }
    let answer: Answer<T> = serde_json::from_slice(body)
        .map_err(|_| ClientError::Protocol("the answer isn't NZBGet's JSON"))?;
    if answer.id.is_some_and(|got| got != json!(id)) {
        return Err(ClientError::Protocol("the answer is for another request"));
    }
    if let Some(e) = answer.error {
        let why = e.message.unwrap_or_default();
        return Err(ClientError::Rejected(client_reason(why.as_bytes())));
    }
    answer
        .result
        .ok_or(ClientError::Protocol("the answer has no result"))
}

/// Reads a `version` answer to call `id`. Public for the fuzz target.
pub fn parse_version(id: u64, status: u16, body: &[u8]) -> Result<String, ClientError> {
    let v: String = answer(id, status, body)?;
    version_text(&v).ok_or(ClientError::Protocol("the answer has no version"))
}

/// Reads an `append` answer to call `id`: a positive `NZBID`. Anything
/// that isn't an integer in `i64` fails to parse; NZBGet's ids are `int`,
/// so one past `i32::MAX` is refused too. Public for the fuzz target.
pub fn parse_append(id: u64, status: u16, body: &[u8]) -> Result<JobId, ClientError> {
    let nzb_id: i64 = answer(id, status, body)?;
    if nzb_id <= 0 {
        return Err(ClientError::Rejected("the client refused the NZB".into()));
    }
    if nzb_id > i64::from(i32::MAX) {
        return Err(ClientError::Protocol("the job id is out of range"));
    }
    Ok(JobId::new(nzb_id.to_string()))
}

/// Reads a `config` answer to call `id`: the `CategoryN.Name` values,
/// cleaned, sorted and de-duplicated, at most [`MAX_CATEGORIES`]. Every
/// other option's value is skipped unread. Public for the fuzz target.
pub fn parse_categories(id: u64, status: u16, body: &[u8]) -> Result<Vec<Category>, ClientError> {
    let CategoryNames(mut names) = answer(id, status, body)?;
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

/// Whether an option is a category's name: `Category<n>.Name`.
fn is_category_name(option: &str) -> bool {
    let Some(rest) = option
        .get(..8)
        .filter(|p| p.eq_ignore_ascii_case("category"))
        .and_then(|_| option.get(8..))
    else {
        return false;
    };
    let Some(n) = rest.len().checked_sub(5).and_then(|i| {
        rest.get(i..)
            .filter(|s| s.eq_ignore_ascii_case(".name"))
            .and(rest.get(..i))
    }) else {
        return false;
    };
    (1..=5).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())
}

/// The category names in a `config` list.
struct CategoryNames(Vec<String>);

impl<'de> Deserialize<'de> for CategoryNames {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<CategoryNames, D::Error> {
        struct List;
        impl<'de> Visitor<'de> for List {
            type Value = CategoryNames;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an option list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<CategoryNames, A::Error> {
                let mut names = Vec::new();
                while let Some(OptionEntry(name)) = seq.next_element()? {
                    if let Some(name) = name {
                        if names.len() == MAX_CATEGORIES {
                            return Err(serde::de::Error::custom("too many categories"));
                        }
                        names.push(name);
                    }
                }
                Ok(CategoryNames(names))
            }
        }
        d.deserialize_seq(List)
    }
}

/// One `{"Name": …, "Value": …}` option: `Some` cleaned value for a
/// category's name. NZBGet writes `Name` first; a `Value` before it is
/// skipped, as is every other option's.
struct OptionEntry(Option<String>);

impl<'de> Deserialize<'de> for OptionEntry {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<OptionEntry, D::Error> {
        struct Entry;
        impl<'de> Visitor<'de> for Entry {
            type Value = OptionEntry;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an option")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OptionEntry, A::Error> {
                let mut wanted = false;
                let mut value = None;
                while let Some(key) = map.next_key::<Key>()? {
                    match key {
                        Key::Name => {
                            let name: String = map.next_value()?;
                            wanted = is_category_name(&name);
                        }
                        Key::Value if wanted && value.is_none() => {
                            let v: String = map.next_value()?;
                            value = category_name(&v);
                        }
                        Key::Value | Key::Other => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(OptionEntry(value.filter(|_| wanted)))
            }
        }
        d.deserialize_map(Entry)
    }
}

enum Key {
    Name,
    Value,
    Other,
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Key, D::Error> {
        struct K;
        impl<'de> Visitor<'de> for K {
            type Value = Key;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a field name")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Key, E> {
                Ok(match v {
                    "Name" => Key::Name,
                    "Value" => Key::Value,
                    _ => Key::Other,
                })
            }
        }
        d.deserialize_identifier(K)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn the_rpc_url_is_derived() {
        let url = |raw: &str| {
            rpc_url(raw, NetPolicy::download_client())
                .unwrap()
                .to_string()
        };
        assert_eq!(
            url("http://nzbget.lan:6789"),
            "http://nzbget.lan:6789/jsonrpc"
        );
        assert_eq!(url("http://h/nzbget/"), "http://h/nzbget/jsonrpc");
        assert_eq!(url("http://h/jsonrpc/?x=1#y"), "http://h/jsonrpc");
        assert!(rpc_url("http://u:p@h/", NetPolicy::download_client()).is_err());
        assert!(rpc_url("http://169.254.169.254/", NetPolicy::download_client()).is_err());
    }

    #[test]
    fn bad_credentials_are_refused_at_construction() {
        let mk = |u: &str, p: &str| Nzbget::new("http://h:6789/", Secret::new(u), Secret::new(p));
        assert!(mk("", "").is_ok());
        assert!(mk("nzbget", "tegbzn6789").is_ok());
        assert!(mk("a:b", "p").is_err());
        assert!(mk("a", "p\r\nX: y").is_err());
        assert!(mk("a", &"p".repeat(190)).is_ok());
        assert!(mk("a", &"p".repeat(191)).is_err());
        assert!(mk(&"u".repeat(150), &"p".repeat(100)).is_err());
    }

    #[test]
    fn the_request_keeps_its_field_order() {
        let body = serde_json::to_string(&Call {
            id: 7,
            method: "append",
            params: &[json!("a\"params\".nzb")],
        })
        .unwrap();
        assert!(
            body.starts_with(r#"{"id":7,"method":"append","params":["#),
            "{body}"
        );
        // A quote in a value is escaped, so NZBGet's search for `"params"`
        // finds the real field first.
        assert_eq!(body.find("\"params\""), Some(26));
    }

    #[test]
    fn answers_are_parsed_strictly() {
        let ok = |r: &str| format!(r#"{{"version" : "1.1", "id" : 1, "result" : {r}}}"#);
        assert_eq!(
            parse_version(1, 200, ok(r#""24.8""#).as_bytes()).unwrap(),
            "24.8"
        );
        assert_eq!(
            parse_append(1, 200, ok("42").as_bytes()).unwrap().as_str(),
            "42"
        );
        assert_eq!(
            parse_append(1, 200, ok("0").as_bytes()),
            Err(ClientError::Rejected("the client refused the NZB".into()))
        );
        assert_eq!(
            parse_append(1, 200, ok("-1").as_bytes()),
            Err(ClientError::Rejected("the client refused the NZB".into()))
        );
        assert!(parse_append(1, 200, ok("2147483648").as_bytes()).is_err());
        assert!(parse_append(1, 200, ok("99999999999999999999999").as_bytes()).is_err());
        assert!(parse_append(1, 200, ok("1.5").as_bytes()).is_err());
        assert!(parse_append(1, 200, ok("\"42\"").as_bytes()).is_err());
        assert_eq!(
            parse_append(2, 200, ok("42").as_bytes()),
            Err(ClientError::Protocol("the answer is for another request"))
        );
        // No id at all is NZBGet's answer to a request without one.
        assert!(parse_append(1, 200, br#"{"result": 3}"#).is_ok());
        assert_eq!(
            parse_append(1, 200, br#"{"version": "1.1", "error": {"name": "JSONRPCError", "code": 2, "message": "Invalid parameter (AddPaused)"}}"#),
            Err(ClientError::Rejected("Invalid parameter (AddPaused)".into()))
        );
        assert_eq!(
            parse_version(1, 401, b""),
            Err(ClientError::Auth(AuthFailure::BadCredentials))
        );
        assert_eq!(parse_version(1, 307, b""), Err(redirected()));
        assert!(parse_version(1, 200, b"<html>").is_err());
    }

    #[test]
    fn categories_skip_every_other_option() {
        let body = br#"{"id": 1, "result": [
            {"Name": "ControlPassword", "Value": "tegbzn6789"},
            {"Name": "Category1.Name", "Value": "Movies"},
            {"Name": "Category1.DestDir", "Value": "/movies"},
            {"Value": "Sneaky", "Name": "Category2.Name"},
            {"Name": "Category3.Name", "Value": "Series"},
            {"Name": "Category4.Name", "Value": ""},
            {"Name": "Category5.Name", "Value": "Movies"},
            {"Name": "Server1.Password", "Value": 3},
            {"Name": "Categoryx.Name", "Value": "No"}
        ]}"#;
        let cats = parse_categories(1, 200, body).unwrap();
        let names: Vec<_> = cats.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Movies", "Series"]);
        assert!(is_category_name("Category12.Name"));
        assert!(!is_category_name("Category.Name"));
        assert!(!is_category_name("Category1.Names"));
        assert!(!is_category_name("Category123456.Name"));
        assert!(!is_category_name("Categoryé.Name"));
    }

    proptest! {
        #[test]
        fn parsers_never_panic(id in 0u64..4,
                               status in prop_oneof![Just(200u16), Just(401), any::<u16>()],
                               body in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = parse_version(id, status, &body);
            let _ = parse_append(id, status, &body);
            let _ = parse_categories(id, status, &body);
        }

        #[test]
        fn option_names_never_panic(name in ".{0,24}") {
            let _ = is_category_name(&name);
        }
    }
}
