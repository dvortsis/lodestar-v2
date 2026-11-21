//! Extractors shared by every module: `Json`, `Query` and `Path` that
//! reject in the error envelope (house-style messages, the bad field's
//! serde path in `field`, axum's status codes), the client IP, and the
//! CSRF check for writes (see the crate docs).

use std::net::{IpAddr, SocketAddr};

use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{ConnectInfo, FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, StatusCode, header};
use serde::de::DeserializeOwned;

use crate::auth::SessionAuth;
use crate::error::{ApiError, ApiResult};

/// A JSON body; a bad one is an [`ApiError`] whose `field` is the path
/// to the bad value (`ast.field`, `tags[2]`) when there is one.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Json<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        if !json_content_type(req.headers()) {
            return Err(not_json());
        }
        // Within axum's default body limit, as `axum::Json` is.
        let bytes = Bytes::from_request(req, state).await.map_err(|e| {
            if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::new(
                    e.status(),
                    "body_too_large",
                    "The request body is too large.",
                )
            } else {
                ApiError::new(e.status(), "bad_body", "The request body couldn't be read.")
            }
        })?;
        let mut de = serde_json::Deserializer::from_slice(&bytes);
        let value = serde_path_to_error::deserialize(&mut de)
            .map_err(|e| (e.path().to_string(), e.into_inner()))
            // Trailing characters after the value, as `axum::Json` rejects.
            .and_then(|v| de.end().map(|()| v).map_err(|e| (".".to_string(), e)))
            .map_err(|(path, e)| {
                if e.classify() != serde_json::error::Category::Data {
                    // 400, as `axum::Json`'s syntax rejection.
                    return ApiError::bad_request("bad_json", "The request body isn't valid JSON.");
                }
                // 422, as `axum::Json`'s data rejection.
                let err = ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "bad_body",
                    "The request body isn't valid JSON for this endpoint.",
                );
                match bad_field(&path, &e.to_string()) {
                    Some(field) => err.field(field),
                    None => err,
                }
            })?;
        Ok(Json(value))
    }
}

/// The field a deserialization error is about: its path, or for a
/// missing field (whose path is the parent) the parent plus its name.
/// A path can hold a caller's map key, so it's shown capped and escaped
/// as every echoed value is ([`crate::search::shown`]); an ordinary path
/// comes back unchanged.
fn bad_field(path: &str, message: &str) -> Option<String> {
    let missing = message
        .strip_prefix("missing field `")
        .and_then(|m| m.split_once('`'))
        .map(|(name, _)| name);
    let field = match (path, missing) {
        (".", Some(name)) => name.to_string(),
        (".", None) => return None,
        (path, Some(name)) => format!("{path}.{name}"),
        (path, None) => path.to_string(),
    };
    Some(crate::search::shown(&field))
}

/// `application/json` or `application/*+json`, as `axum::Json` accepts.
fn json_content_type(headers: &HeaderMap) -> bool {
    let Some(ct) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let mime = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mime == "application/json"
        || mime
            .strip_prefix("application/")
            .is_some_and(|sub| sub.ends_with("+json"))
}

/// The first parameter `is_param` accepts that `query` sends more than
/// once, read with the same decoder the query extractor uses (so a
/// percent-encoded name or `+` for a space counts as it would there).
pub(crate) fn repeated_param(query: &str, is_param: impl Fn(&str) -> bool) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    form_urlencoded::parse(query.as_bytes())
        .map(|(key, _)| key)
        .filter(|key| is_param(key))
        .find(|key| !seen.insert(key.clone()))
        .map(|key| key.into_owned())
}

/// The one answer to a known query parameter sent more than once, on every
/// route that reads its query: `400 repeated_parameter`, naming it (capped
/// and escaped, as any echoed request value is).
pub(crate) fn repeated_parameter(name: &str) -> ApiError {
    let name = crate::search::shown(name);
    ApiError::bad_request(
        "repeated_parameter",
        format!("{name} was sent more than once: send it once."),
    )
    .field(name)
}

/// The names a query struct's derived `Deserialize` reads, renames and
/// aliases included, as serde itself lists them: so a field added to a plain
/// struct is checked for repeats with nothing else to remember. A type that
/// never asks for a struct has none and gets no repeat check: a map, which
/// is why `/facets` keeps its own layer, and a struct with a
/// `#[serde(flatten)]` field, which serde reads as a map (tested below). A newtype wrapper reads its inner
/// struct's names.
pub(crate) fn field_names<T: DeserializeOwned>() -> &'static [&'static str] {
    use serde::de::{self, Visitor};
    use std::cell::Cell;

    struct Fields<'a>(&'a Cell<&'static [&'static str]>);
    impl<'de> de::Deserializer<'de> for Fields<'_> {
        type Error = de::value::Error;
        fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
            Err(de::Error::custom("not a struct"))
        }
        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Self::Error> {
            self.0.set(fields);
            Err(de::Error::custom("only the field names are read"))
        }
        fn deserialize_newtype_struct<V: Visitor<'de>>(
            self,
            _: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error> {
            // A wrapper reads its inner struct's names.
            visitor.visit_newtype_struct(self)
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
            option unit unit_struct seq tuple tuple_struct map enum identifier
            ignored_any
        }
    }
    let names = Cell::new(&[][..]);
    let _ = T::deserialize(Fields(&names));
    names.get()
}

/// Query parameters; bad ones are an [`ApiError`] naming the parameter. A
/// known parameter sent more than once is [`repeated_parameter`], before
/// any value is read (the struct's fields are the known names); unknown
/// ones are ignored, repeated or not.
#[derive(Debug, Clone, Copy, Default)]
pub struct Query<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for Query<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ApiError> {
        let query = parts.uri.query().unwrap_or("");
        let known = field_names::<T>();
        if let Some(name) = repeated_param(query, |key| known.contains(&key)) {
            return Err(repeated_parameter(&name));
        }
        let de = serde_urlencoded::Deserializer::new(form_urlencoded::parse(query.as_bytes()));
        serde_path_to_error::deserialize(de)
            .map(Query)
            .map_err(|e| {
                let path = e.path().to_string();
                let err = ApiError::bad_request(
                    "bad_parameter",
                    "A query parameter has the wrong type or value.",
                );
                match bad_field(&path, &e.into_inner().to_string()) {
                    Some(field) => err.field(field),
                    None => err,
                }
            })
    }
}

/// Path parameters; bad ones are an [`ApiError`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Path<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for Path<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Path(v)| Path(v))
            .map_err(|e: PathRejection| {
                ApiError::new(e.status(), "bad_path", "The request path isn't valid.")
            })
    }
}

/// The client's address, for per-client login and key limits: the TCP
/// peer (from `ConnectInfo`), or, when it's a trusted proxy and proxy
/// auth is on, the rightmost `X-Forwarded-For` address that isn't one
/// (see [`crate::auth::proxy`]). Outside [`crate::router`] it's the peer.
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let peer = peer_ip(&parts.extensions);
        let ip = match parts.extensions.get::<SessionAuth>() {
            Some(s) => s.auth.client_ip(peer, &parts.headers),
            None => peer,
        };
        Ok(ClientIp(ip))
    }
}

/// The TCP peer itself, as the session guard sees it (for checks that
/// repeat the guard's later, such as an event stream's heartbeats).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Peer(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Peer(peer_ip(&parts.extensions)))
    }
}

/// The TCP peer's address (`0.0.0.0` without `ConnectInfo`).
pub(crate) fn peer_ip(extensions: &Extensions) -> IpAddr {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::from([0, 0, 0, 0]), |c| c.0.ip())
}

fn not_json() -> ApiError {
    ApiError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
        "Send the request body as application/json.",
    )
}

/// Rejects cross-site writes (see the crate docs).
pub fn same_origin_json(headers: &HeaderMap) -> ApiResult<()> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            ct.split(';')
                .next()
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
        });
    if !json {
        return Err(not_json());
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let origin_host = origin.split_once("://").map_or(origin, |(_, h)| h);
        if origin_host != host {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "cross_origin",
                "Cross-origin requests aren't allowed.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn field_names_are_the_structs_own_and_flatten_has_none() {
        use std::collections::BTreeMap;
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct Plain {
            #[serde(rename = "type")]
            kind: Option<String>,
            #[serde(alias = "max")]
            limit: Option<u32>,
        }
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct Wrapper(Plain);
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct Flattened {
            q: Option<String>,
            #[serde(flatten)]
            plain: Plain,
        }
        let mut plain = field_names::<Plain>().to_vec();
        plain.sort_unstable();
        assert_eq!(plain, ["limit", "max", "type"]);
        assert_eq!(field_names::<Wrapper>().len(), 3);
        assert!(field_names::<Flattened>().is_empty());
        assert!(field_names::<BTreeMap<String, String>>().is_empty());
    }

    use super::bad_field;

    #[test]
    fn a_bad_field_is_its_path_capped_and_escaped() {
        // Ordinary paths, a body's or a query's, come back unchanged.
        assert_eq!(bad_field("limit", "invalid digit"), Some("limit".into()));
        assert_eq!(
            bad_field("limits.ext", "invalid type"),
            Some("limits.ext".into())
        );
        assert_eq!(bad_field(".", "missing field `q`"), Some("q".into()));
        assert_eq!(
            bad_field("filter", "missing field `name` at line 1"),
            Some("filter.name".into())
        );
        assert_eq!(bad_field(".", "invalid type"), None);
        // A long map key: cut at 64 characters and marked.
        let long = format!("limits.{}", "k".repeat(5_000));
        assert_eq!(
            bad_field(&long, "invalid type"),
            Some(format!("limits.{}…", "k".repeat(57)))
        );
        // A bidi override or a newline in a key comes back escaped.
        let field = bad_field("limits.a\u{202e}b\nc", "invalid type").unwrap();
        assert_eq!(field, r"limits.a\u{202e}b\nc");
        assert!(!field.chars().any(|c| c.is_control() || c == '\u{202e}'));
    }
}
