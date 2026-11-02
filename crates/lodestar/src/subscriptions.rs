//! Blocklist subscriptions (architecture §20.13): a curated catalog of
//! popular lists, scheduled refreshes with conditional requests, size
//! caps and sanity gates, and the step that turns stored lists into
//! filter layers and the shared IP set.
//!
//! Content lists sit in the pack layer, or join the locked floor as CS-1b
//! when you marked one as a vetted child-abuse list. IP lists are merged
//! into one set that the DHT, the fetch pool and the scraper consult
//! before talking to anyone.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_filter::lists::{self, Format, IpSet, SharedIps};
use lodestar_filter::{Action, Handle};
use lodestar_integrations::net::{GuardedClient, NetError, NetPolicy};
use lodestar_log::{Count, Error, Url, log_event};
use lodestar_store::{NewSubscription, Refresh, Store, Subscription};
use tokio::sync::watch;

use crate::config::{ListAction, ListKind, Subscriptions};
use crate::logging::events;

/// `subscriptions.kind`.
const CONTENT: u8 = 0;
const IP: u8 = 1;

pub use lodestar_filter::strictness::{CATALOG, catalog};

/// A new version that shrinks below this share of the last one is held.
const MAX_SHRINK: f64 = 0.5;
/// Or that grows more than this many times.
const MAX_GROWTH: i64 = 10;
/// How often due subscriptions are looked for.
const TICK: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_download: usize,
    pub max_expanded: usize,
    /// What a list URL, a redirect or a name's addresses may reach (the
    /// §16 SSRF guard): public addresses only, since any signed-in session
    /// can add a list. Tests allow loopback for their fake list server.
    pub policy: NetPolicy,
    /// One whole fetch, redirects and body included: a slow or dripping
    /// server can hold a refresh up only this long, and refreshes run one
    /// list at a time.
    pub fetch_timeout: Duration,
}

impl Limits {
    pub fn from_config(c: &Subscriptions) -> Limits {
        Limits {
            max_download: (c.max_download_mib.max(1) << 20) as usize,
            max_expanded: (c.max_expanded_mib.max(1) << 20) as usize,
            policy: NetPolicy::notification(false),
            fetch_timeout: FETCH_TIMEOUT,
        }
    }
}

/// Adds the configured lists to the store (existing ones are kept as they
/// are; removing a list is a weakening change made through the API).
pub fn sync_config(store: &mut Store, c: &Subscriptions) -> Result<(), String> {
    let interval = |h: Option<u64>| (h.unwrap_or(c.refresh_hours).max(1) * 3600) as i64;
    for key in &c.catalog {
        let e = catalog(key).ok_or_else(|| format!("subscriptions: no catalog list {key:?}"))?;
        store
            .add_subscription(&NewSubscription {
                url: e.url,
                name: e.name,
                kind: IP,
                format: Some(e.format.as_str()),
                action: Action::Drop.code(),
                floor: false,
                interval_secs: interval(None),
            })
            .map_err(|e| e.to_string())?;
    }
    for l in &c.lists {
        if let Some(f) = &l.format {
            let format = Format::from_name(f)
                .ok_or_else(|| format!("subscriptions: unknown format {f:?}"))?;
            if format.is_ip() != (l.kind == ListKind::Ip) {
                return Err(format!(
                    "subscriptions: {f} doesn't fit a {:?} list",
                    l.kind
                ));
            }
        }
        if l.floor && l.kind != ListKind::Content {
            return Err("subscriptions: only content lists can join the floor".into());
        }
        let action = match l.action {
            ListAction::Drop => Action::Drop,
            ListAction::Quarantine => Action::Quarantine,
        };
        // An existing list is kept as it is, so the config can't mark it
        // as part of the floor: that's done in the app, with the password.
        if l.floor {
            let existing = store.subscriptions().map_err(|e| e.to_string())?;
            if existing.iter().any(|s| s.url == l.url && !s.floor) {
                lodestar_log::log_event!(
                    events::FILTERS_SUBSCRIPTION_FLOOR_IGNORED,
                    url = Url::new(&l.url)
                );
            }
        }
        store
            .add_subscription(&NewSubscription {
                url: &l.url,
                name: l.name.as_deref().unwrap_or(&l.url),
                kind: if l.kind == ListKind::Ip { IP } else { CONTENT },
                format: l.format.as_deref(),
                action: action.code(),
                floor: l.floor,
                interval_secs: interval(l.refresh_hours),
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub use lodestar_filter::lists::apply;

/// Redirects a list download follows, each checked like the first URL.
const MAX_REDIRECTS: usize = 5;
/// A whole list download, at most.
const FETCH_TIMEOUT: Duration = Duration::from_secs(300);

/// The HTTP client: the §16 SSRF guard (one checked resolution per
/// connection, no system proxy, no automatic redirects), on the
/// workspace's rustls with ring.
fn client(limits: &Limits) -> Result<GuardedClient, NetError> {
    GuardedClient::new(limits.policy, FETCH_TIMEOUT)
}

/// A parsed list, encoded for the store.
struct Parsed {
    data: Vec<u8>,
    entries: i64,
}

fn parse(sub: &Subscription, text: &str) -> Result<Parsed, String> {
    let format = match &sub.format {
        Some(f) => Format::from_name(f).ok_or("unknown format")?,
        None => lists::detect(text).ok_or("couldn't tell the list's format")?,
    };
    if format.is_ip() != (sub.kind == IP) {
        return Err(format!(
            "a {} list in a subscription of the other kind",
            format.as_str()
        ));
    }
    if format.is_ip() {
        let p = lists::parse_ips(text, format);
        let set = IpSet::new(p.entries);
        let entries = set.len() as i64;
        Ok(Parsed {
            data: set.encode(),
            entries,
        })
    } else {
        let p = lists::parse_hashes(text);
        let entries = p.entries.len() as i64;
        Ok(Parsed {
            data: p.entries.concat(),
            entries,
        })
    }
}

/// The sanity gates: a version that's empty, shrinks by half or grows
/// tenfold waits for a decision instead of applying.
fn gate(previous: i64, next: i64) -> Option<String> {
    if previous == 0 {
        return (next == 0).then(|| "the list is empty".to_owned());
    }
    if (next as f64) < previous as f64 * MAX_SHRINK {
        let pct = 100 - next * 100 / previous;
        return Some(format!(
            "it shrank by {pct}% ({previous} to {next} entries)"
        ));
    }
    if next > previous * MAX_GROWTH {
        return Some(format!(
            "it grew {}-fold ({previous} to {next} entries)",
            next / previous
        ));
    }
    None
}

/// What a fetch brought back, before it's recorded.
enum Fetched {
    New {
        parsed: Parsed,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    Unchanged,
    Failed(String),
}

/// A download: the body and its validators, or "not modified".
enum Download {
    NotModified,
    Body {
        body: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// Downloads one list with a conditional request, capped in size and in
/// time: [`Limits::fetch_timeout`] covers every hop and the whole body.
async fn download(
    client: &GuardedClient,
    limits: &Limits,
    sub: &Subscription,
) -> Result<Download, String> {
    tokio::time::timeout(limits.fetch_timeout, download_within(client, limits, sub))
        .await
        .unwrap_or_else(|_| Err("download failed: it took longer than the time limit".into()))
}

async fn download_within(
    client: &GuardedClient,
    limits: &Limits,
    sub: &Subscription,
) -> Result<Download, String> {
    use reqwest::header::{HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH, LOCATION};
    let mut url =
        reqwest::Url::parse(&sub.url).map_err(|_| "the list's URL doesn't parse".to_owned())?;
    let mut hops = 0;
    // Every hop goes through the guard's `send`, which checks the URL and
    // every address its name resolves to; a GET is safe to repeat.
    let mut resp = loop {
        let mut req = reqwest::Request::new(reqwest::Method::GET, url.clone());
        let validators = [
            (IF_NONE_MATCH, &sub.etag),
            (IF_MODIFIED_SINCE, &sub.last_modified),
        ];
        for (name, value) in validators {
            if let Some(v) = value.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
                req.headers_mut().insert(name, v);
            }
        }
        let resp = client.send(req).await.map_err(|e| match e {
            // A hop's URL came from the server's Location header, so
            // the guard's advice on credential fields doesn't apply.
            NetError::InvalidUrl(_) if hops > 0 => {
                "download failed: a redirect without a usable location".to_owned()
            }
            e => format!("download failed: {e}"),
        })?;
        if !matches!(resp.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            break resp;
        }
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err("download failed: too many redirects".into());
        }
        let target = resp
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|loc| resp.url().join(loc).ok())
            .ok_or("download failed: a redirect without a usable location")?;
        if resp.url().scheme() == "https" && target.scheme() != "https" {
            return Err("download failed: refused a redirect from https to http".into());
        }
        url = target;
    };
    match resp.status().as_u16() {
        304 => return Ok(Download::NotModified),
        200 => {}
        code => return Err(format!("the server answered HTTP {code}")),
    }
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let (etag, last_modified) = (header("etag"), header("last-modified"));
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        // reqwest's text can quote the URL, which can carry a key.
        .map_err(|_| "download failed: the connection broke while reading the list".to_owned())?
    {
        if body.len() + chunk.len() > limits.max_download {
            return Err("the list is larger than the download limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Download::Body {
        body,
        etag,
        last_modified,
    })
}

/// Decodes and parses a downloaded list (CPU-bound).
fn parse_download(sub: &Subscription, limits: &Limits, download: Download) -> Fetched {
    let (body, etag, last_modified) = match download {
        Download::NotModified => return Fetched::Unchanged,
        Download::Body {
            body,
            etag,
            last_modified,
        } => (body, etag, last_modified),
    };
    let text = match lists::decode(&body, limits.max_expanded) {
        Ok(t) => t,
        Err(e) => return Fetched::Failed(e.to_string()),
    };
    match parse(sub, &text) {
        Ok(parsed) => Fetched::New {
            parsed,
            etag,
            last_modified,
        },
        Err(e) => Fetched::Failed(e),
    }
}

/// Records what a fetch brought back. Returns whether the applied
/// version changed.
fn record(store: &mut Store, sub: &Subscription, fetched: Fetched, now: i64) -> bool {
    let held_why;
    let (refresh, changed) = match &fetched {
        Fetched::Unchanged => (Refresh::Unchanged, false),
        Fetched::Failed(why) => {
            // `why` can quote the URL; the list's status in the app has it.
            log_event!(
                events::FILTERS_SUBSCRIPTION_REFRESH_FAILED,
                list = Count(sub.id as u64),
                url = Url::new(&sub.url)
            );
            (Refresh::Failed(why), false)
        }
        Fetched::New {
            parsed,
            etag,
            last_modified,
        } => {
            let (etag, last_modified) = (etag.as_deref(), last_modified.as_deref());
            held_why = gate(sub.entries, parsed.entries);
            match &held_why {
                Some(why) => {
                    log_event!(
                        events::FILTERS_SUBSCRIPTION_HELD,
                        list = Count(sub.id as u64),
                        url = Url::new(&sub.url)
                    );
                    let r = Refresh::Held {
                        data: &parsed.data,
                        entries: parsed.entries,
                        etag,
                        last_modified,
                        why,
                    };
                    (r, false)
                }
                None => {
                    let r = Refresh::Updated {
                        data: &parsed.data,
                        entries: parsed.entries,
                        etag,
                        last_modified,
                    };
                    (r, true)
                }
            }
        }
    };
    match store.record_refresh(sub.id, now, &refresh) {
        Ok(0) => {}
        Ok(kept) => {
            // A floor-marked list only adds (§2.7).
            let source = lodestar_store::FloorSource::List(sub.id);
            crate::floor::log_pending(&source, kept);
            crate::floor::KEPT.fetch_add(kept as u64, std::sync::atomic::Ordering::Relaxed);
        }
        Err(e) => {
            log_event!(
                events::FILTERS_SUBSCRIPTION_RECORD_FAILED,
                list = Count(sub.id as u64),
                error = Error::new(&e)
            );
            return false;
        }
    }
    changed
}

fn due(s: &Subscription, now: i64) -> bool {
    s.enabled && s.checked_at.is_none_or(|t| now - t >= s.interval_secs)
}

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// Refreshes every due subscription, then re-applies if anything changed.
/// Returns how many were refreshed. Downloads are async; parsing and the
/// store run on the blocking pool.
pub async fn refresh_due(
    store: &Arc<Mutex<Store>>,
    client: &GuardedClient,
    limits: &Limits,
    filter: &Arc<Handle>,
    ips: &Arc<SharedIps>,
    now: i64,
) -> usize {
    let st = store.clone();
    let subs = match tokio::task::spawn_blocking(move || lock(&st).subscriptions()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            log_event!(
                events::FILTERS_SUBSCRIPTIONS_READ_FAILED,
                error = Error::new(&e)
            );
            return 0;
        }
        Err(_) => return 0,
    };
    let due: Vec<Subscription> = subs.into_iter().filter(|s| due(s, now)).collect();
    let mut changed = false;
    for sub in &due {
        let download = download(client, limits, sub).await;
        let (st, sub, limits) = (store.clone(), sub.clone(), limits.clone());
        let recorded = tokio::task::spawn_blocking(move || {
            let fetched = match download {
                Ok(d) => parse_download(&sub, &limits, d),
                Err(e) => Fetched::Failed(e),
            };
            record(&mut lock(&st), &sub, fetched, now)
        })
        .await;
        changed |= recorded.unwrap_or(false);
    }
    if changed {
        let (st, filter, ips) = (store.clone(), filter.clone(), ips.clone());
        let applied = tokio::task::spawn_blocking(move || apply(&lock(&st), &filter, &ips)).await;
        if let Ok(Err(e)) = applied {
            log_event!(
                events::FILTERS_SUBSCRIPTIONS_APPLY_FAILED,
                error = Error::new(&e)
            );
        }
    }
    due.len()
}

/// Checks for due subscriptions every minute until shutdown.
pub async fn run(
    store: Arc<Mutex<Store>>,
    limits: Limits,
    filter: Arc<Handle>,
    ips: Arc<SharedIps>,
    mut shutdown: watch::Receiver<bool>,
) {
    // Without a client (no usable TLS roots, say) the lists can't be
    // fetched: say so once and stop, rather than taking the task down.
    let client = match client(&limits) {
        Ok(client) => client,
        Err(e) => {
            log_event!(
                events::FILTERS_SUBSCRIPTIONS_CLIENT_FAILED,
                error = Error::new(&e)
            );
            return;
        }
    };
    loop {
        tokio::select! {
            _ = refresh_due(&store, &client, &limits, &filter, &ips, crate::unix_now()) => {}
            _ = shutdown.changed() => return,
        }
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = shutdown.changed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SubscribedList;
    use lodestar_filter::{CoreRule, Engine, Item, RuleRef};
    use lodestar_store::StoreConfig;
    use std::collections::HashSet;
    use std::net::IpAddr;

    #[test]
    fn gates() {
        assert_eq!(gate(0, 5), None);
        assert!(gate(0, 0).is_some());
        assert_eq!(gate(100, 60), None);
        assert!(gate(100, 40).unwrap().contains("shrank by 60%"));
        assert!(gate(100, 1001).unwrap().contains("10-fold"));
        assert_eq!(gate(100, 1000), None);
    }

    #[test]
    fn catalog_is_well_formed() {
        let mut keys = HashSet::new();
        for e in CATALOG {
            assert!(keys.insert(e.key), "duplicate {}", e.key);
            assert!(e.url.starts_with("https://"), "{}", e.key);
            assert!(e.format.is_ip());
        }
    }

    /// A tiny HTTP server for one test: serves `body` with an ETag, and
    /// 304 when asked with it.
    fn serve(responses: Vec<(u16, Vec<u8>)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/list", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in responses {
                let (mut sock, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(sock.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    head.push_str(&line);
                }
                seen.push(head);
                // A 302's body is where it redirects to.
                if status == 302 {
                    let to = String::from_utf8(body).unwrap();
                    write!(
                        sock,
                        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                    continue;
                }
                let reason = if status == 304 { "Not Modified" } else { "OK" };
                write!(
                    sock,
                    "HTTP/1.1 {status} {reason}\r\nETag: \"v{}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    seen.len(),
                    body.len()
                )
                .unwrap();
                sock.write_all(&body).unwrap();
            }
            seen
        });
        (url, handle)
    }

    #[allow(clippy::type_complexity)]
    fn setup(
        lists: Vec<SubscribedList>,
    ) -> (Arc<Mutex<Store>>, Arc<Handle>, Arc<SharedIps>, Limits) {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let config = Subscriptions {
            lists,
            ..Subscriptions::default()
        };
        sync_config(&mut store, &config).unwrap();
        let limits = Limits {
            // The fake list server is on loopback.
            policy: NetPolicy::download_client(),
            ..Limits::from_config(&config)
        };
        (
            Arc::new(Mutex::new(store)),
            Arc::new(lodestar_pipeline::default_filter()),
            Arc::new(SharedIps::default()),
            limits,
        )
    }

    fn list(url: &str, kind: ListKind, floor: bool) -> SubscribedList {
        SubscribedList {
            url: url.to_owned(),
            name: None,
            kind,
            format: None,
            action: ListAction::Drop,
            floor,
            refresh_hours: None,
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn ip_lists_refresh_conditionally_and_hold_suspicious_versions() {
        let v1 = b"# test\n10.0.0.0/8\n172.16.0.0/12\n192.168.0.0/16\n".to_vec();
        let shrunk = b"10.0.0.0/8\n".to_vec();
        let (url, server) = serve(vec![(200, v1), (304, Vec::new()), (200, shrunk)]);
        let (store, filter, ips, limits) = setup(vec![list(&url, ListKind::Ip, false)]);
        let agent = client(&limits).unwrap();
        assert_eq!(
            refresh_due(&store, &agent, &limits, &filter, &ips, 1_000).await,
            1
        );
        assert!(ips.contains(ip("10.1.2.3")) && ips.contains(ip("192.168.4.5")));
        assert!(!ips.contains(ip("11.0.0.1")));
        // Not due again until the interval passes.
        assert_eq!(
            refresh_due(&store, &agent, &limits, &filter, &ips, 2_000).await,
            0
        );
        let later = 1_000 + 86_400;
        assert_eq!(
            refresh_due(&store, &agent, &limits, &filter, &ips, later).await,
            1
        );
        let sub = store.lock().unwrap().subscriptions().unwrap().remove(0);
        assert_eq!(sub.status, "unchanged");
        // Two thirds of the list vanished: held, and the applied version
        // stays.
        refresh_due(&store, &agent, &limits, &filter, &ips, later + 86_400).await;
        let sub = store.lock().unwrap().subscriptions().unwrap().remove(0);
        assert_eq!(sub.status, "held");
        assert!(sub.error.unwrap().contains("shrank by 67%"));
        assert!(ips.contains(ip("192.168.4.5")));
        let requests = server.join().unwrap();
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("if-none-match: \"v1\"")
        );
    }

    #[tokio::test]
    async fn content_lists_drop_or_join_the_floor() {
        let a = [0xaa; 20];
        let b = [0xbb; 20];
        let body = |h: [u8; 20]| {
            let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
            format!("{hex}\n").into_bytes()
        };
        let (url_a, sa) = serve(vec![(200, body(a))]);
        let (url_b, sb) = serve(vec![(200, body(b))]);
        let (store, filter, ips, limits) = setup(vec![
            list(&url_a, ListKind::Content, false),
            list(&url_b, ListKind::Content, true),
        ]);
        let agent = client(&limits).unwrap();
        assert_eq!(
            refresh_due(&store, &agent, &limits, &filter, &ips, 5).await,
            2
        );
        sa.join().unwrap();
        sb.join().unwrap();
        let e: Arc<Engine> = filter.current();
        let item = |h: &'static [[u8; 20]]| Item {
            hashes: h,
            name: "x",
            files: &[],
            content_type: lodestar_classify::ContentType::Other,
        };
        let da = e.judge(&item(&[[0xaa; 20]]), None, 0).decision.unwrap();
        assert!(matches!(da.rule, RuleRef::List(_)));
        assert_eq!(da.action, Action::Drop);
        let db = e.blocked_hash(&[[0xbb; 20]]).unwrap();
        assert_eq!(db.rule, RuleRef::Core(CoreRule::Cs1b));
        // Disabled lists stop applying.
        {
            let mut s = store.lock().unwrap();
            for sub in s.subscriptions().unwrap() {
                s.set_subscription_enabled(sub.id, false).unwrap();
            }
            apply(&s, &filter, &ips).unwrap();
        }
        let e = filter.current();
        assert!(e.judge(&item(&[[0xaa; 20]]), None, 0).decision.is_none());
        assert!(e.blocked_hash(&[[0xbb; 20]]).is_none());
    }

    /// Owner's decision §2.7: a floor-marked list's upstream removals wait
    /// for a decision in the app, still blocked.
    #[tokio::test]
    async fn floor_lists_keep_upstream_removals() {
        let hashes = |hs: &[[u8; 20]]| -> Vec<u8> {
            hs.iter()
                .map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>() + "\n")
                .collect::<String>()
                .into_bytes()
        };
        let (a, b, c) = ([0xa1; 20], [0xb2; 20], [0xc3; 20]);
        let (url, server) = serve(vec![(200, hashes(&[a, b, c])), (200, hashes(&[a, b]))]);
        let (store, filter, ips, limits) = setup(vec![list(&url, ListKind::Content, true)]);
        let agent = client(&limits).unwrap();
        refresh_due(&store, &agent, &limits, &filter, &ips, 5).await;
        let before = crate::floor::KEPT.load(std::sync::atomic::Ordering::Relaxed);
        refresh_due(&store, &agent, &limits, &filter, &ips, 5 + 86_400).await;
        server.join().unwrap();
        assert!(crate::floor::KEPT.load(std::sync::atomic::Ordering::Relaxed) > before);
        let e = filter.current();
        for h in [a, c] {
            let d = e.blocked_hash(&[h]).unwrap();
            assert_eq!(d.rule, RuleRef::Core(CoreRule::Cs1b));
        }
        let mut s = store.lock().unwrap();
        let id = s.subscriptions().unwrap()[0].id;
        let source = lodestar_store::FloorSource::List(id);
        assert_eq!(s.pending_floor_of(&source).unwrap(), [c]);
        // Reported again at the next start.
        let lines = crate::floor::report(&s).unwrap();
        assert!(
            lines[0].starts_with("1 floor entry was removed upstream from"),
            "{lines:?}"
        );
        // Removed in the app: unblocked.
        s.resolve_pending_floor(&source, None, true, 10).unwrap();
        apply(&s, &filter, &ips).unwrap();
        let e = filter.current();
        assert!(e.blocked_hash(&[c]).is_none());
        assert!(e.blocked_hash(&[a]).is_some());
    }

    #[tokio::test]
    async fn failures_are_recorded_not_applied() {
        let (url, server) = serve(vec![(404, Vec::new()), (200, b"\0\0binary".to_vec())]);
        let (store, filter, ips, limits) = setup(vec![list(&url, ListKind::Ip, false)]);
        let agent = client(&limits).unwrap();
        refresh_due(&store, &agent, &limits, &filter, &ips, 10).await;
        let sub = store.lock().unwrap().subscriptions().unwrap().remove(0);
        assert_eq!(sub.status, "error");
        assert!(sub.error.unwrap().contains("404"));
        refresh_due(&store, &agent, &limits, &filter, &ips, 10 + 86_400).await;
        let sub = store.lock().unwrap().subscriptions().unwrap().remove(0);
        assert!(sub.error.unwrap().contains("isn't text"));
        server.join().unwrap();
        assert!(ips.current().is_empty());
    }

    /// The §16 SSRF guard holds for list fetches: a literal private or
    /// loopback address, a name that resolves to one, and a redirect to
    /// the metadata service are all refused before any connection, and
    /// recorded as the list's error.
    #[tokio::test]
    async fn list_fetches_reach_only_what_the_guard_allows() {
        struct Private;
        impl lodestar_integrations::Resolve for Private {
            fn resolve<'a>(
                &'a self,
                _host: &'a str,
            ) -> lodestar_integrations::BoxFuture<'a, std::io::Result<Vec<IpAddr>>> {
                Box::pin(async { Ok(vec!["10.1.2.3".parse().unwrap()]) })
            }
        }
        let error = |store: &Arc<Mutex<Store>>| {
            let sub = store.lock().unwrap().subscriptions().unwrap().remove(0);
            assert_eq!(sub.status, "error");
            sub.error.unwrap()
        };
        let public = NetPolicy::notification(false);
        for (url, class) in [
            ("http://127.0.0.1:9/list", "loopback"),
            ("http://192.168.1.1/list", "private-network"),
            ("http://169.254.169.254/latest/meta-data", "cloud metadata"),
        ] {
            let (store, filter, ips, mut limits) = setup(vec![list(url, ListKind::Ip, false)]);
            limits.policy = public;
            let agent = client(&limits).unwrap();
            refresh_due(&store, &agent, &limits, &filter, &ips, 10).await;
            assert!(error(&store).contains(class), "{url}");
        }
        // A name that resolves to a private address.
        let (store, filter, ips, limits) =
            setup(vec![list("http://lists.example/l", ListKind::Ip, false)]);
        let agent =
            GuardedClient::with_resolver(public, Duration::from_secs(10), Arc::new(Private))
                .unwrap();
        refresh_due(&store, &agent, &limits, &filter, &ips, 10).await;
        assert!(error(&store).contains("private-network"));
        // A redirect to the metadata service, from a server the policy
        // allows (loopback, in tests).
        let meta = b"http://169.254.169.254/latest/meta-data".to_vec();
        let (url, server) = serve(vec![(302, meta)]);
        let (store, filter, ips, limits) = setup(vec![list(&url, ListKind::Ip, false)]);
        let agent = client(&limits).unwrap();
        refresh_due(&store, &agent, &limits, &filter, &ips, 10).await;
        server.join().unwrap();
        assert!(error(&store).contains("cloud metadata"));
        assert!(ips.current().is_empty());
    }

    /// The time limit covers the whole fetch: a server that sends its
    /// headers and then drips (or stalls) its body is cut off at the
    /// limit, though no single read times out.
    #[tokio::test]
    async fn a_stalled_download_is_cut_off_at_the_time_limit() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/list", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).unwrap();
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n10.0.0.0/8\n")
                .unwrap();
            // Hold the body open until the client gives up and hangs up.
            while sock.read(&mut buf).is_ok_and(|n| n > 0) {}
        });
        let (store, filter, ips, mut limits) = setup(vec![list(&url, ListKind::Ip, false)]);
        limits.fetch_timeout = Duration::from_millis(300);
        let agent = client(&limits).unwrap();
        refresh_due(&store, &agent, &limits, &filter, &ips, 10).await;
        server.join().unwrap();
        let sub = store.lock().unwrap().subscriptions().unwrap().remove(0);
        assert!(sub.error.unwrap().contains("time limit"));
        assert!(ips.current().is_empty());
    }

    /// Owner's decision §2.9: subscriptions are off by default, so a
    /// default config connects no list.
    #[test]
    fn no_list_is_connected_by_default() {
        let c = crate::config::Config::default().subscriptions;
        assert!(c.catalog.is_empty() && c.lists.is_empty());
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        sync_config(&mut store, &c).unwrap();
        assert!(store.subscriptions().unwrap().is_empty());
    }

    #[test]
    fn config_validation() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let bad_key = Subscriptions {
            catalog: vec!["nope".into()],
            ..Subscriptions::default()
        };
        assert!(sync_config(&mut store, &bad_key).is_err());
        let mut l = list("https://x/y", ListKind::Ip, true);
        let floor_ip = Subscriptions {
            lists: vec![l.clone()],
            ..Subscriptions::default()
        };
        assert!(sync_config(&mut store, &floor_ip).is_err());
        l.floor = false;
        l.format = Some("hashes".into());
        let mismatched = Subscriptions {
            lists: vec![l],
            ..Subscriptions::default()
        };
        assert!(sync_config(&mut store, &mismatched).is_err());
        let catalog = Subscriptions {
            catalog: vec!["firehol-level1".into()],
            ..Subscriptions::default()
        };
        sync_config(&mut store, &catalog).unwrap();
        assert_eq!(store.subscriptions().unwrap().len(), 1);
    }
}
