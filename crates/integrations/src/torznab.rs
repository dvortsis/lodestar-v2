//! A Torznab endpoint's connection test (Settings → Integrations, §10.10.8):
//! `GET <url>?t=caps&apikey=<key>` through the SSRF guard, the way
//! Prowlarr, Jackett and the *arr apps check an indexer.
//!
//! - The policy is the download clients': an indexer manager is usually
//!   on this machine or the LAN (a companion, §18.10), never on
//!   link-local, metadata or reserved addresses.
//! - Nothing the server answered is repeated: the result is a version at
//!   most, or a [`ClientError`] whose text is this module's own.

use std::time::Duration;

use reqwest::{Method, Request, Url};

use crate::clients::{AuthFailure, ClientError};
use crate::net::{GuardedClient, NetPolicy, parse_url, read_limited};
use crate::secret::Secret;

/// The longest the test waits for the answer.
pub const TORZNAB_TIMEOUT: Duration = Duration::from_secs(15);

/// A caps document is a few kilobytes.
const MAX_ANSWER_BYTES: usize = 256 * 1024;

/// What a working endpoint said about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorznabInfo {
    /// `<server version="…">`, when it gives one (digits and dots only).
    pub version: Option<String>,
}

/// A client for [`caps`] under the download-client policy.
pub fn client() -> Result<GuardedClient, ClientError> {
    Ok(GuardedClient::new(
        NetPolicy::download_client(),
        TORZNAB_TIMEOUT,
    )?)
}

/// Asks `url` for its capabilities with `api_key`.
pub async fn caps(
    http: &GuardedClient,
    url: &str,
    api_key: Option<&Secret>,
) -> Result<TorznabInfo, ClientError> {
    let mut u: Url = parse_url(url, http.policy())?;
    if !u.username().is_empty() || u.password().is_some() {
        return Err(ClientError::InvalidRequest(
            "the address has a user name or password in it",
        ));
    }
    // The form's address, with only the test's own parameters.
    u.set_query(None);
    u.set_fragment(None);
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("t", "caps");
        if let Some(k) = api_key.filter(|k| !k.is_empty()) {
            q.append_pair("apikey", k.expose());
        }
    }
    let resp = http.send(Request::new(Method::GET, u)).await?;
    let status = resp.status().as_u16();
    if matches!(status, 401 | 403) {
        return Err(ClientError::Auth(AuthFailure::BadCredentials));
    }
    let body = read_limited(resp, MAX_ANSWER_BYTES).await?;
    let text = String::from_utf8_lossy(&body);
    // Torznab's "Incorrect user credentials" and "missing parameter"
    // errors (codes 100 and 200-ish), often with HTTP 200.
    if let Some(code) = error_code(&text) {
        return Err(if (100..200).contains(&code) {
            ClientError::Auth(AuthFailure::BadCredentials)
        } else {
            ClientError::Rejected(format!("Torznab error {code}"))
        });
    }
    if !(200..300).contains(&status) {
        return Err(ClientError::Rejected(format!("HTTP {status}")));
    }
    if !text.contains("<caps") {
        return Err(ClientError::Protocol(
            "the answer isn't a Torznab caps document",
        ));
    }
    Ok(TorznabInfo {
        version: server_version(&text),
    })
}

/// The code of an `<error code="…">` answer.
fn error_code(text: &str) -> Option<u16> {
    let at = text.find("<error")?;
    attr(&text[at..], "code")?.parse().ok()
}

/// `<server version="…">`, kept only when it's a plain version number.
fn server_version(text: &str) -> Option<String> {
    let at = text.find("<server")?;
    let end = text[at..].find('>')? + at;
    let v = attr(&text[at..end], "version")?;
    let plain =
        !v.is_empty() && v.len() <= 32 && v.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    plain.then(|| v.to_string())
}

/// An attribute's value in the start of `tag`.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let end = tag.find('>').unwrap_or(tag.len());
    let tag = &tag[..end];
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let len = tag[start..].find('"')?;
    Some(&tag[start..start + len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_answers_are_read_without_quoting_them() {
        let caps = r#"<?xml version="1.0"?><caps><server version="1.2.3" title="x"/></caps>"#;
        assert_eq!(server_version(caps).as_deref(), Some("1.2.3"));
        let odd = r#"<caps><server version="1.0<script>" /></caps>"#;
        assert_eq!(server_version(odd), None);
        let err = r#"<error code="100" description="Incorrect user credentials"/>"#;
        assert_eq!(error_code(err), Some(100));
        assert_eq!(error_code("<caps/>"), None);
    }
}
