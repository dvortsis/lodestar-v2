//! HTTP tracker scrape (BEP 48): `GET <scrape url>?info_hash=…&info_hash=…`
//! returning a bencoded dictionary
//!
//! ```text
//! d5:filesd20:<hash>d8:completei5e10:downloadedi50e10:incompletei10eeee
//! ```
//!
//! or `failure reason`. Optional `flags.min_request_interval` asks us to
//! back off. Bodies are capped before they're read, and the bencode is
//! decoded with tight limits.

use std::time::Duration;

use ::url::Url;
use lodestar_bencode::{self as bencode, DecodeOptions, Dict, Value};
use lodestar_metainfo::InfoHash;

use crate::{Counts, Error, Reply, Result, check_batch, message};

/// Most hashes one HTTP request may carry. Many trackers cap multi-hash
/// scrapes well below this; [`HttpConfig::max_hashes`] sets the batch.
pub const MAX_HASHES: usize = 74;
/// Body cap: a full reply is about 70 B per hash, so this leaves ample
/// room for names and padding while stopping a hostile stream early.
pub const MAX_BODY: usize = 64 << 10;
/// Longest wait a tracker may ask for.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(86_400);

/// The deepest legitimate nesting is top → `files` → entry → value.
const LIMITS: bencode::Limits = bencode::Limits {
    max_depth: 4,
    max_items: 4096,
};

#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Hashes per request, at most [`MAX_HASHES`].
    pub max_hashes: usize,
    /// Whole request, connect to last byte.
    pub timeout: Duration,
}

impl Default for HttpConfig {
    fn default() -> Self {
        HttpConfig {
            max_hashes: 32,
            timeout: Duration::from_secs(15),
        }
    }
}

/// The HTTP client shared by every HTTP tracker.
///
/// It ignores `HTTP(S)_PROXY`: crawler traffic leaves through the bound
/// interface or VPN like the DHT does (§5), not through whatever proxy
/// the environment names. Redirects are limited, since a tracker has no
/// business sending us far away. `local` sets the source address.
pub fn client(local: Option<std::net::IpAddr>) -> Result<reqwest::Client> {
    // Several clients may be built; only the first install wins, and any
    // provider installed by the host process is fine too.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .no_proxy()
        .local_address(local)
        .redirect(reqwest::redirect::Policy::limited(2))
        .user_agent(concat!("Lodestar/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| Error::Http(e.to_string()))
}

/// `url` with one `info_hash` parameter per hash, every byte
/// percent-encoded. Parameters already in the URL (passkeys) are kept.
pub fn request_url(scrape_url: &Url, hashes: &[InfoHash]) -> Url {
    let mut query = scrape_url.query().unwrap_or_default().to_owned();
    for h in hashes {
        if !query.is_empty() {
            query.push('&');
        }
        query.push_str("info_hash=");
        for b in h.0 {
            query.push_str(&format!("%{b:02X}"));
        }
    }
    let mut url = scrape_url.clone();
    url.set_query(Some(&query));
    url
}

fn count(entry: &Dict<'_>, key: &[u8]) -> Option<u32> {
    match entry.get(key) {
        // Absent means the tracker didn't say; zero is the honest reading.
        None => Some(0),
        Some(v) => v
            .as_int()
            .and_then(|n| u32::try_from(n.max(0)).ok().or(Some(u32::MAX))),
    }
}

/// Parses a scrape reply for `hashes` (the request, in order). Entries for
/// hashes we didn't ask about, malformed entries and unknown keys are
/// ignored; a negative count reads as 0 and an over-large one as
/// `u32::MAX`.
pub fn parse_scrape(body: &[u8], hashes: &[InfoHash]) -> Result<Reply> {
    let value = bencode::decode_with(body, &DecodeOptions::lenient().with_limits(LIMITS))
        .map_err(|_| Error::Protocol("reply is not bencode"))?;
    let top = value
        .as_dict()
        .ok_or(Error::Protocol("reply is not a dictionary"))?;
    if let Some(reason) = top.get(b"failure reason") {
        return Err(Error::Refused(message(
            reason.as_bytes().unwrap_or_default(),
        )));
    }
    let files = top
        .get(b"files")
        .and_then(Value::as_dict)
        .ok_or(Error::Protocol("reply has no files dictionary"))?;
    let mut counts = vec![None; hashes.len()];
    for (key, entry) in files.iter() {
        let Some(entry) = entry.as_dict() else {
            continue;
        };
        for (i, h) in hashes.iter().enumerate() {
            if h.0[..] != *key {
                continue;
            }
            let parsed = (|| {
                Some(Counts {
                    seeders: count(entry, b"complete")?,
                    completed: count(entry, b"downloaded")?,
                    leechers: count(entry, b"incomplete")?,
                })
            })();
            counts[i] = parsed;
        }
    }
    let retry_after = top
        .get(b"flags")
        .and_then(Value::as_dict)
        .and_then(|f| f.get(b"min_request_interval"))
        .and_then(Value::as_int)
        .filter(|s| *s > 0)
        .map(|s| Duration::from_secs(s as u64).min(MAX_RETRY_AFTER));
    Ok(Reply {
        counts,
        retry_after,
    })
}

/// One HTTP tracker.
#[derive(Debug, Clone)]
pub struct HttpTracker {
    scrape_url: Url,
    cfg: HttpConfig,
    client: reqwest::Client,
}

impl HttpTracker {
    pub fn new(scrape_url: Url, cfg: HttpConfig, client: reqwest::Client) -> HttpTracker {
        HttpTracker {
            scrape_url,
            cfg,
            client,
        }
    }

    pub fn max_hashes(&self) -> usize {
        self.cfg.max_hashes.clamp(1, MAX_HASHES)
    }

    pub async fn scrape(&self, hashes: &[InfoHash]) -> Result<Reply> {
        check_batch(hashes, self.max_hashes())?;
        let url = request_url(&self.scrape_url, hashes);
        let fetch = async {
            let mut resp = self.client.get(url).send().await.map_err(http_error)?;
            if resp.status() != reqwest::StatusCode::OK {
                return Err(Error::Status(resp.status().as_u16()));
            }
            if resp.content_length().is_some_and(|n| n > MAX_BODY as u64) {
                return Err(Error::TooLarge);
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(http_error)? {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(Error::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        };
        let body = tokio::time::timeout(self.cfg.timeout, fetch)
            .await
            .map_err(|_| Error::Timeout)??;
        parse_scrape(&body, hashes)
    }
}

fn http_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout
    } else {
        // Without the URL: it may hold a passkey.
        Error::Http(e.without_url().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: InfoHash = InfoHash([0xaa; 20]);
    const B: InfoHash = InfoHash([0xbb; 20]);

    fn files(entries: &[(&[u8], &str)]) -> Vec<u8> {
        let mut out = b"d5:filesd".to_vec();
        for (k, v) in entries {
            out.extend_from_slice(format!("{}:", k.len()).as_bytes());
            out.extend_from_slice(k);
            out.extend_from_slice(v.as_bytes());
        }
        out.extend_from_slice(b"ee");
        out
    }

    #[test]
    fn parses_counts_in_request_order() {
        let body = files(&[
            (
                &A.0,
                "d8:completei5e10:downloadedi50e10:incompletei10e4:name1:xe",
            ),
            (&B.0, "d8:completei0e10:downloadedi0e10:incompletei0ee"),
            (&[0xcc; 20], "d8:completei9ee"),
        ]);
        let r = parse_scrape(&body, &[B, A, InfoHash([0xdd; 20])]).unwrap();
        assert_eq!(
            r.counts,
            [
                Some(Counts::default()),
                Some(Counts {
                    seeders: 5,
                    completed: 50,
                    leechers: 10
                }),
                None
            ]
        );
        assert_eq!(r.retry_after, None);
    }

    #[test]
    fn failure_reason_and_flags() {
        match parse_scrape(b"d14:failure reason12:rate limitede", &[A]) {
            Err(Error::Refused(m)) => assert_eq!(m, "rate limited"),
            other => panic!("{other:?}"),
        }
        let body = b"d5:filesde5:flagsd20:min_request_intervali900eee";
        let r = parse_scrape(body, &[A]).unwrap();
        assert_eq!(r.counts, [None]);
        assert_eq!(r.retry_after, Some(Duration::from_secs(900)));
        let huge = b"d5:filesde5:flagsd20:min_request_intervali99999999eee";
        assert_eq!(
            parse_scrape(huge, &[A]).unwrap().retry_after,
            Some(MAX_RETRY_AFTER)
        );
    }

    #[test]
    fn odd_values_are_clamped_or_skipped() {
        let body = files(&[(
            &A.0,
            "d8:completei-4e10:downloadedi99999999999e10:incomplete1:xe",
        )]);
        assert_eq!(parse_scrape(&body, &[A]).unwrap().counts, [None]);
        let body = files(&[(&A.0, "d8:completei-4e10:downloadedi99999999999ee")]);
        assert_eq!(
            parse_scrape(&body, &[A]).unwrap().counts,
            [Some(Counts {
                seeders: 0,
                completed: u32::MAX,
                leechers: 0
            })]
        );
        assert_eq!(
            parse_scrape(&files(&[(&A.0, "i1e")]), &[A]).unwrap().counts,
            [None]
        );
    }

    #[test]
    fn rejects_non_replies() {
        for body in [
            &b""[..],
            b"le",
            b"d4:spami1ee",
            b"<html>busy</html>",
            b"d5:filesi1ee",
        ] {
            assert!(parse_scrape(body, &[A]).is_err(), "{body:?}");
        }
        // Too deep for a scrape reply.
        assert!(parse_scrape(b"d5:filesd1:xd1:yd1:zdeeeee", &[A]).is_err());
    }

    #[test]
    fn request_urls_keep_existing_parameters() {
        let base = Url::parse("http://t.example/scrape.php?passkey=k").unwrap();
        let u = request_url(&base, &[InfoHash([0x00; 20]), InfoHash([0xff; 20])]);
        let q = u.query().unwrap();
        assert!(q.starts_with("passkey=k&info_hash=%00%00"), "{q}");
        assert_eq!(q.matches("info_hash=").count(), 2);
        assert!(q.ends_with(&"%FF".repeat(20)));
    }
}
