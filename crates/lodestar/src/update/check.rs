//! The single signed update check (§12.2 "Checking"): one `GET` of
//! `https://github.com/<owner>/<repo>/releases/download/update-manifest/<channel>.json`.
//!
//! - **The source** comes from `CARGO_PKG_REPOSITORY`, which must be a
//!   `https://github.com/<owner>/<repo>` URL, else updates report
//!   `no_release_source`. The rolling `update-manifest` release is a
//!   contract with E-M6's `release.yml`.
//! - **How:** `GuardedClient` with the notification policy (public
//!   addresses only). Each hop goes through `send`; redirects (301, 302,
//!   303, 307, 308) are followed here, at most 5, `https` only, to
//!   [`DOWNLOAD_HOSTS`] only: GitHub's release downloads answer 302,
//!   which `send_following` doesn't follow. Bodies are read through
//!   `read_limited` (512 KiB). 10 s to connect, 60 s overall.
//! - **What it sends:** `User-Agent: Lodestar-Updater` (never the
//!   versioned client agent), no cookies, no conditional headers, no
//!   query string. A redirect's own URL (GitHub's signed asset URLs carry
//!   one) is never logged.
//! - **A 404** is "nothing published for this channel yet", not an error.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use lodestar_integrations::net::{GuardedClient, NetError, NetPolicy, read_limited};
use lodestar_update::doc::{DOWNLOAD_HOSTS, MAX_ENVELOPE};
use reqwest::Url;

/// The one user agent the updater sends.
pub const USER_AGENT: &str = "Lodestar-Updater";
/// Redirects followed, at most.
pub const MAX_HOPS: usize = 5;

/// One response: its status, its `Location`, and (for a 200) its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub status: u16,
    pub location: Option<String>,
    pub body: Vec<u8>,
}

/// What a check's fetch can fail with. Stable codes, never a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckError {
    /// `CARGO_PKG_REPOSITORY` isn't a GitHub repository URL.
    NoReleaseSource,
    /// A redirect to `http`, to a host off the allowlist, without a
    /// usable `Location`, or past [`MAX_HOPS`].
    Redirect,
    /// The body was larger than the envelope's limit.
    TooLarge,
    Timeout,
    /// Couldn't connect, resolve, or speak HTTP.
    Network,
    /// The address is one the policy refuses.
    Blocked,
    /// Any other status.
    Status(u16),
}

impl CheckError {
    pub fn code(self) -> &'static str {
        match self {
            CheckError::NoReleaseSource => "no_release_source",
            CheckError::Redirect => "redirect_refused",
            CheckError::TooLarge => "too_large",
            CheckError::Timeout => "timeout",
            CheckError::Network => "network",
            CheckError::Blocked => "blocked",
            CheckError::Status(_) => "http_status",
        }
    }
}

impl From<NetError> for CheckError {
    fn from(e: NetError) -> CheckError {
        match e {
            NetError::Timeout => CheckError::Timeout,
            NetError::TooLarge => CheckError::TooLarge,
            NetError::Redirect | NetError::InvalidUrl(_) => CheckError::Redirect,
            NetError::Blocked(_) => CheckError::Blocked,
            _ => CheckError::Network,
        }
    }
}

/// One `GET`, no redirects followed: the check's only way out, so tests
/// can serve releases from memory.
pub trait Fetch: Send + Sync {
    fn get<'a>(
        &'a self,
        url: &'a Url,
    ) -> Pin<Box<dyn Future<Output = Result<Hop, CheckError>> + Send + 'a>>;
}

/// The manifest URL for `channel`, from the repository URL.
pub fn source(repository: &str, channel: lodestar_update::Channel) -> Result<Url, CheckError> {
    let rest = repository
        .strip_prefix("https://github.com/")
        .ok_or(CheckError::NoReleaseSource)?
        .trim_end_matches('/');
    let mut parts = rest.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(CheckError::NoReleaseSource);
    };
    let name_ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    };
    if !name_ok(owner) || !name_ok(repo) {
        return Err(CheckError::NoReleaseSource);
    }
    Url::parse(&format!(
        "https://github.com/{owner}/{repo}/releases/download/update-manifest/{}.json",
        channel.name()
    ))
    .map_err(|_| CheckError::NoReleaseSource)
}

/// Whether a hop may go to `url`: `https`, an allowlisted host, the
/// default port, no credentials.
pub fn hop_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|h| DOWNLOAD_HOSTS.contains(&h))
}

/// Fetches `url`, following allowed redirects: the body, or `None` for a
/// 404 (nothing published for this channel yet).
pub async fn fetch(f: &dyn Fetch, url: Url) -> Result<Option<Vec<u8>>, CheckError> {
    let mut at = url;
    for _ in 0..=MAX_HOPS {
        if !hop_allowed(&at) {
            return Err(CheckError::Redirect);
        }
        let hop = f.get(&at).await?;
        match hop.status {
            200 => return Ok(Some(hop.body)),
            404 => return Ok(None),
            301 | 302 | 303 | 307 | 308 => {
                at = hop
                    .location
                    .as_deref()
                    .and_then(|l| at.join(l).ok())
                    .ok_or(CheckError::Redirect)?;
            }
            s => return Err(CheckError::Status(s)),
        }
    }
    Err(CheckError::Redirect)
}

/// The real source: `GuardedClient`, public addresses only.
pub struct Guarded {
    client: GuardedClient,
}

impl Guarded {
    pub fn new() -> Result<Guarded, CheckError> {
        let client = GuardedClient::new(NetPolicy::notification(false), Duration::from_secs(60))
            .map_err(CheckError::from)?;
        Ok(Guarded { client })
    }
}

/// The request a check sends: a bare `GET` with the updater's agent.
pub fn request(url: &Url) -> reqwest::Request {
    let mut req = reqwest::Request::new(reqwest::Method::GET, url.clone());
    req.headers_mut().insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(USER_AGENT),
    );
    req
}

impl Fetch for Guarded {
    fn get<'a>(
        &'a self,
        url: &'a Url,
    ) -> Pin<Box<dyn Future<Output = Result<Hop, CheckError>> + Send + 'a>> {
        Box::pin(async move {
            let resp = self.client.send(request(url)).await?;
            let status = resp.status().as_u16();
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let body = if status == 200 {
                read_limited(resp, MAX_ENVELOPE).await?
            } else {
                Vec::new()
            };
            Ok(Hop {
                status,
                location,
                body,
            })
        })
    }
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// An in-memory release source: URL → response; every request is
    /// recorded.
    #[derive(Default)]
    pub struct Fake {
        pub routes: Mutex<HashMap<String, Hop>>,
        pub asked: Mutex<Vec<String>>,
    }

    impl Fake {
        pub fn serve(&self, url: &str, status: u16, location: Option<&str>, body: &[u8]) {
            self.routes.lock().unwrap().insert(
                url.to_string(),
                Hop {
                    status,
                    location: location.map(str::to_owned),
                    body: body.to_vec(),
                },
            );
        }
    }

    impl Fetch for Fake {
        fn get<'a>(
            &'a self,
            url: &'a Url,
        ) -> Pin<Box<dyn Future<Output = Result<Hop, CheckError>> + Send + 'a>> {
            self.asked.lock().unwrap().push(url.to_string());
            let hop = self.routes.lock().unwrap().get(url.as_str()).cloned();
            Box::pin(async move {
                Ok(hop.unwrap_or(Hop {
                    status: 404,
                    location: None,
                    body: vec![],
                }))
            })
        }
    }

    const STABLE: &str =
        "https://github.com/dvortsis/lodestar-v2/releases/download/update-manifest/stable.json";

    #[test]
    fn the_source_is_the_repositorys_rolling_release() {
        let s = source(
            "https://github.com/dvortsis/lodestar-v2",
            lodestar_update::Channel::Stable,
        )
        .unwrap();
        assert_eq!(s.as_str(), STABLE);
        for bad in [
            "",
            "http://github.com/a/b",
            "https://gitlab.com/a/b",
            "https://github.com/a",
            "https://github.com/a/b/c",
            "https://github.com/a/b?x",
            "https://github.com/../b",
        ] {
            assert_eq!(
                source(bad, lodestar_update::Channel::Stable),
                Err(CheckError::NoReleaseSource),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn follows_allowed_redirects_by_hand() {
        let f = Fake::default();
        let asset = "https://release-assets.githubusercontent.com/x/stable.json?sig=abc&exp=1";
        f.serve(STABLE, 302, Some(asset), b"");
        f.serve(asset, 200, None, b"envelope");
        let got = fetch(&f, Url::parse(STABLE).unwrap()).await.unwrap();
        assert_eq!(got.as_deref(), Some(&b"envelope"[..]));
        // A 404 is nothing published yet.
        let none =
            "https://github.com/dvortsis/lodestar-v2/releases/download/update-manifest/beta.json";
        assert_eq!(fetch(&f, Url::parse(none).unwrap()).await, Ok(None));
    }

    #[tokio::test]
    async fn refuses_hops_off_the_allowlist_http_and_too_many() {
        let f = Fake::default();
        f.serve(STABLE, 302, Some("https://evil.example/m.json"), b"");
        assert_eq!(
            fetch(&f, Url::parse(STABLE).unwrap()).await,
            Err(CheckError::Redirect)
        );
        assert_eq!(
            f.asked.lock().unwrap().len(),
            1,
            "the off-list hop isn't asked"
        );
        f.serve(STABLE, 301, Some("http://github.com/m.json"), b"");
        assert_eq!(
            fetch(&f, Url::parse(STABLE).unwrap()).await,
            Err(CheckError::Redirect)
        );
        f.serve(STABLE, 307, Some("https://github.com:8443/m.json"), b"");
        assert_eq!(
            fetch(&f, Url::parse(STABLE).unwrap()).await,
            Err(CheckError::Redirect)
        );
        // Six redirects in a row.
        for i in 0..6 {
            let from = if i == 0 {
                STABLE.to_string()
            } else {
                format!("https://github.com/hop/{i}")
            };
            let to = format!("https://github.com/hop/{}", i + 1);
            f.serve(&from, 302, Some(&to), b"");
        }
        f.serve("https://github.com/hop/6", 200, None, b"late");
        assert_eq!(
            fetch(&f, Url::parse(STABLE).unwrap()).await,
            Err(CheckError::Redirect)
        );
        f.serve(STABLE, 500, None, b"");
        assert_eq!(
            fetch(&f, Url::parse(STABLE).unwrap()).await,
            Err(CheckError::Status(500))
        );
    }

    /// The request carries exactly the updater's agent: no identifier, no
    /// cookie, no conditional header, no query.
    #[test]
    fn the_request_sends_nothing_that_identifies() {
        let req = request(&Url::parse(STABLE).unwrap());
        let names: Vec<&str> = req.headers().keys().map(|k| k.as_str()).collect();
        assert_eq!(names, ["user-agent"]);
        assert_eq!(req.headers()["user-agent"], USER_AGENT);
        assert!(req.url().query().is_none());
        assert_eq!(req.method(), reqwest::Method::GET);
        assert!(req.body().is_none());
    }
}
