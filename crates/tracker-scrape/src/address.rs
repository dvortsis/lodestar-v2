//! Tracker URLs: parsing, a canonical form for identity, and the BEP 48
//! announce → scrape convention.

use std::fmt;
use std::str::FromStr;

use ::url::Url;

/// Longest tracker URL accepted.
pub const MAX_URL: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    Udp,
    Http,
    Https,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UrlError {
    TooLong,
    Invalid,
    /// Not `udp`, `http` or `https` (e.g. WebTorrent's `wss`).
    Unsupported,
    NoHost,
    /// UDP trackers have no default port.
    NoPort,
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UrlError::TooLong => "tracker URL too long",
            UrlError::Invalid => "not a valid URL",
            UrlError::Unsupported => "only udp, http and https trackers are supported",
            UrlError::NoHost => "tracker URL has no host",
            UrlError::NoPort => "udp tracker URL needs a port",
        })
    }
}

impl std::error::Error for UrlError {}

/// A parsed tracker URL. Two URLs naming the same tracker compare equal
/// through [`TrackerUrl::as_str`], the canonical form used as its identity:
/// `udp://host:port` for UDP (BEP 41 paths are dropped; trackers ignore
/// them for scrapes), and the normalised announce URL without its
/// fragment for HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackerUrl {
    scheme: Scheme,
    /// Without brackets, lowercase.
    host: String,
    port: u16,
    canonical: String,
}

impl TrackerUrl {
    pub fn parse(s: &str) -> Result<TrackerUrl, UrlError> {
        let s = s.trim();
        if s.len() > MAX_URL {
            return Err(UrlError::TooLong);
        }
        let mut url = Url::parse(s).map_err(|_| UrlError::Invalid)?;
        let scheme = match url.scheme() {
            "udp" => Scheme::Udp,
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            _ => return Err(UrlError::Unsupported),
        };
        let host = url
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or(UrlError::NoHost)?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        let (port, canonical) = match scheme {
            Scheme::Udp => {
                let port = url.port().filter(|p| *p != 0).ok_or(UrlError::NoPort)?;
                let canonical = if host.contains(':') {
                    format!("udp://[{host}]:{port}")
                } else {
                    format!("udp://{host}:{port}")
                };
                (port, canonical)
            }
            Scheme::Http | Scheme::Https => {
                url.set_fragment(None);
                let port = url.port_or_known_default().ok_or(UrlError::NoPort)?;
                (port, url.as_str().to_owned())
            }
        };
        Ok(TrackerUrl {
            scheme,
            host,
            port,
            canonical,
        })
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    /// The HTTP scrape URL by the BEP 48 convention: the last path segment
    /// must start with `announce`, which becomes `scrape`
    /// (`/announce.php?k=v` → `/scrape.php?k=v`). `None` for UDP, and for
    /// trackers whose URL doesn't follow the convention (they don't
    /// support scraping).
    pub fn scrape_url(&self) -> Option<Url> {
        if self.scheme == Scheme::Udp {
            return None;
        }
        let mut url = Url::parse(&self.canonical).ok()?;
        let path = url.path().to_owned();
        let cut = path.rfind('/')? + 1;
        let rest = path[cut..].strip_prefix("announce")?;
        url.set_path(&format!("{}scrape{rest}", &path[..cut]));
        Some(url)
    }
}

impl FromStr for TrackerUrl {
    type Err = UrlError;

    fn from_str(s: &str) -> Result<TrackerUrl, UrlError> {
        TrackerUrl::parse(s)
    }
}

impl fmt::Display for TrackerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> TrackerUrl {
        TrackerUrl::parse(s).unwrap()
    }

    #[test]
    fn udp_urls_are_canonical() {
        let t = p(" udp://Tracker.Example.org:1337/announce ");
        assert_eq!(t.scheme(), Scheme::Udp);
        assert_eq!(t.host(), "tracker.example.org");
        assert_eq!(t.port(), 1337);
        assert_eq!(t.as_str(), "udp://tracker.example.org:1337");
        assert_eq!(t, p("udp://tracker.example.org:1337"));
        assert_eq!(t.scrape_url(), None);

        let v6 = p("udp://[2001:DB8::1]:6969/announce");
        assert_eq!(v6.host(), "2001:db8::1");
        assert_eq!(v6.as_str(), "udp://[2001:db8::1]:6969");
        assert_eq!(p("udp://10.0.0.1:80").host(), "10.0.0.1");
    }

    #[test]
    fn rejects_what_we_cannot_scrape() {
        let e = |s: &str| TrackerUrl::parse(s).unwrap_err();
        assert_eq!(e("udp://tracker.example.org/announce"), UrlError::NoPort);
        assert_eq!(e("udp://tracker.example.org:0"), UrlError::NoPort);
        assert_eq!(e("wss://tracker.example.org"), UrlError::Unsupported);
        assert_eq!(e("magnet:?xt=urn:btih:00"), UrlError::Unsupported);
        assert_eq!(e("not a url"), UrlError::Invalid);
        assert_eq!(
            e(&format!("http://a/{}", "x".repeat(600))),
            UrlError::TooLong
        );
    }

    #[test]
    fn http_scrape_convention() {
        let s = |u: &str| p(u).scrape_url().map(|u| u.to_string());
        assert_eq!(
            s("http://t.example/announce").as_deref(),
            Some("http://t.example/scrape")
        );
        assert_eq!(
            s("https://t.example/x/announce.php?passkey=1#frag").as_deref(),
            Some("https://t.example/x/scrape.php?passkey=1")
        );
        assert_eq!(
            s("http://t.example:8080/announce").as_deref(),
            Some("http://t.example:8080/scrape")
        );
        // BEP 48: no `announce` in the last segment means no scrape.
        assert_eq!(s("http://t.example/a"), None);
        assert_eq!(s("http://t.example/announce/x"), None);
        assert_eq!(s("http://t.example/x_announce"), None);
        assert_eq!(p("https://t.example/announce").port(), 443);
    }
}
