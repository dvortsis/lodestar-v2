//! Blocklist subscriptions (architecture §20.13): a curated catalog of
//! popular lists, scheduled refreshes with conditional requests, size
//! caps and sanity gates, and the step that turns stored lists into
//! filter layers and the shared IP set.
//!
//! Content lists sit in the pack layer, or join the locked floor as CS-1b
//! when you marked one as a vetted child-abuse list. IP lists are merged
//! into one set that the DHT, the fetch pool and the scraper consult
//! before talking to anyone.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_filter::lists::{self, Format, IpSet, SharedIps};
use lodestar_filter::{Action, Handle, ListLayer};
use lodestar_store::{NewSubscription, Refresh, Store, Subscription};
use tokio::sync::watch;

use crate::config::{ListAction, ListKind, Subscriptions};

/// `subscriptions.kind`.
const CONTENT: u8 = 0;
const IP: u8 = 1;

/// A list in the one-click catalog. Every entry was checked to be live
/// and in the stated format before it was added; check them again before
/// each release.
pub struct CatalogEntry {
    pub key: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub format: Format,
    pub about: &'static str,
}

pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        key: "bt-blocklists",
        name: "BT_BlockLists (Naunter)",
        url: "https://raw.githubusercontent.com/Naunter/BT_BlockLists/master/bt_blocklists.gz",
        format: Format::P2p,
        about: "Combined P2P blocklist of anti-P2P, monitoring and known-bad peer \
                ranges, popular with BitTorrent clients. Very broad: it blocks whole \
                provider allocations and covered about half of the IPv4 space when \
                checked, which noticeably shrinks the DHT the crawler can see. About \
                34 MB expanded.",
    },
    CatalogEntry {
        key: "firehol-level1",
        name: "FireHOL level 1",
        url: "https://raw.githubusercontent.com/firehol/blocklist-ipsets/master/firehol_level1.netset",
        format: Format::Cidr,
        about: "Networks that are attacking or abusing right now, chosen for the \
                fewest false positives. Small.",
    },
    CatalogEntry {
        key: "firehol-level2",
        name: "FireHOL level 2",
        url: "https://raw.githubusercontent.com/firehol/blocklist-ipsets/master/firehol_level2.netset",
        format: Format::Cidr,
        about: "Addresses seen attacking in the last 48 hours. Broader than level 1.",
    },
];

/// A new version that shrinks below this share of the last one is held.
const MAX_SHRINK: f64 = 0.5;
/// Or that grows more than this many times.
const MAX_GROWTH: i64 = 10;
/// IP lists covering more than this share of IPv4 get a warning.
const BROAD: f64 = 0.10;
/// How often due subscriptions are looked for.
const TICK: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_download: usize,
    pub max_expanded: usize,
    /// Use the `HTTPS_PROXY`-style environment, as the rest of the egress
    /// does (§5).
    pub proxy_from_env: bool,
}

impl Limits {
    pub fn from_config(c: &Subscriptions) -> Limits {
        Limits {
            max_download: (c.max_download_mib.max(1) << 20) as usize,
            max_expanded: (c.max_expanded_mib.max(1) << 20) as usize,
            proxy_from_env: true,
        }
    }
}

/// Adds the configured lists to the store (existing ones are kept as they
/// are; removing a list is a weakening change made through the API).
pub fn sync_config(store: &mut Store, c: &Subscriptions) -> Result<(), String> {
    let interval = |h: Option<u64>| (h.unwrap_or(c.refresh_hours).max(1) * 3600) as i64;
    for key in &c.catalog {
        let e = CATALOG
            .iter()
            .find(|e| e.key == key)
            .ok_or_else(|| format!("subscriptions: no catalog list {key:?}"))?;
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

/// Rebuilds the filter's list layers and the shared IP set from every
/// enabled subscription's applied version.
pub fn apply(store: &Store, filter: &Handle, ips: &SharedIps) -> Result<(), String> {
    let subs = store.subscriptions().map_err(|e| e.to_string())?;
    let mut ip_sets = Vec::new();
    let mut content = Vec::new();
    for s in &subs {
        let data = if s.enabled {
            store
                .subscription_data(s.id, false)
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        match s.kind {
            IP => {
                if let Some(set) = data.as_deref().and_then(IpSet::decode) {
                    ip_sets.push(set);
                }
            }
            _ => {
                let hashes = data.map(|d| {
                    Arc::new(
                        d.chunks_exact(20)
                            .map(|c| <[u8; 20]>::try_from(c).expect("20 bytes"))
                            .collect::<HashSet<_>>(),
                    )
                });
                content.push((s, hashes));
            }
        }
    }
    let set = IpSet::union(&ip_sets);
    let coverage = set.v4_coverage();
    if coverage > BROAD {
        tracing::warn!(
            "subscribed IP lists block {:.0}% of IPv4 addresses; the crawler will see \
             correspondingly less of the DHT",
            coverage * 100.0
        );
    }
    ips.replace(set);
    filter.update_engine(|e| {
        for (s, hashes) in content {
            let id = s.id as u32;
            if s.floor {
                e.core.cs.set_list(id, hashes);
                e.set_list(None, id);
            } else {
                let action = Action::from_code(s.action)
                    .filter(|a| a.protective())
                    .unwrap_or(Action::Drop);
                e.set_list(hashes.map(|hashes| ListLayer { id, action, hashes }), id);
                e.core.cs.set_list(id, None);
            }
        }
    });
    Ok(())
}

fn agent(limits: &Limits) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .timeout_connect(Some(Duration::from_secs(20)))
        .user_agent(concat!("Lodestar/", env!("CARGO_PKG_VERSION")))
        .http_status_as_error(false)
        .proxy(if limits.proxy_from_env {
            ureq::Proxy::try_from_env()
        } else {
            None
        })
        .build();
    ureq::Agent::new_with_config(config)
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

/// Downloads and parses one list (blocking).
fn fetch(agent: &ureq::Agent, limits: &Limits, sub: &Subscription) -> Fetched {
    let mut req = agent.get(&sub.url);
    if let Some(e) = &sub.etag {
        req = req.header("If-None-Match", e);
    }
    if let Some(m) = &sub.last_modified {
        req = req.header("If-Modified-Since", m);
    }
    let mut resp = match req.call() {
        Ok(r) => r,
        Err(e) => return Fetched::Failed(format!("download failed: {e}")),
    };
    match resp.status().as_u16() {
        304 => return Fetched::Unchanged,
        200 => {}
        code => return Fetched::Failed(format!("the server answered HTTP {code}")),
    }
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let (etag, last_modified) = (header("etag"), header("last-modified"));
    let body = match resp
        .body_mut()
        .with_config()
        .limit(limits.max_download as u64)
        .read_to_vec()
    {
        Ok(b) => b,
        Err(e) => return Fetched::Failed(format!("download failed: {e}")),
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
            tracing::warn!(list = %sub.name, "refresh failed: {why}");
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
                    tracing::warn!(list = %sub.name, "held a new version back: {why}");
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
    if let Err(e) = store.record_refresh(sub.id, now, &refresh) {
        tracing::warn!(list = %sub.name, "recording a refresh failed: {e}");
        return false;
    }
    changed
}

fn due(s: &Subscription, now: i64) -> bool {
    s.enabled && s.checked_at.is_none_or(|t| now - t >= s.interval_secs)
}

/// Refreshes every due subscription, then re-applies if anything changed.
/// Returns how many were refreshed (blocking).
pub fn refresh_due(
    store: &Mutex<Store>,
    agent: &ureq::Agent,
    limits: &Limits,
    filter: &Handle,
    ips: &SharedIps,
    now: i64,
) -> usize {
    let lock = || store.lock().unwrap_or_else(|e| e.into_inner());
    let subs = match lock().subscriptions() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("reading subscriptions failed: {e}");
            return 0;
        }
    };
    let due: Vec<&Subscription> = subs.iter().filter(|s| due(s, now)).collect();
    let mut changed = false;
    for s in &due {
        // The download happens outside the lock; only the record takes it.
        let fetched = fetch(agent, limits, s);
        changed |= record(&mut lock(), s, fetched, now);
    }
    if changed {
        if let Err(e) = apply(&lock(), filter, ips) {
            tracing::warn!("applying subscriptions failed: {e}");
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
    let agent = agent(&limits);
    loop {
        let (store, agent2, limits2, filter2, ips2) = (
            store.clone(),
            agent.clone(),
            limits.clone(),
            filter.clone(),
            ips.clone(),
        );
        let _ = tokio::task::spawn_blocking(move || {
            refresh_due(
                &store,
                &agent2,
                &limits2,
                &filter2,
                &ips2,
                crate::unix_now(),
            )
        })
        .await;
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

    fn setup(lists: Vec<SubscribedList>) -> (Mutex<Store>, Handle, SharedIps, Limits) {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let config = Subscriptions {
            lists,
            ..Subscriptions::default()
        };
        sync_config(&mut store, &config).unwrap();
        let limits = Limits {
            proxy_from_env: false,
            ..Limits::from_config(&config)
        };
        (
            Mutex::new(store),
            lodestar_pipeline::default_filter(),
            SharedIps::default(),
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

    #[test]
    fn ip_lists_refresh_conditionally_and_hold_suspicious_versions() {
        let v1 = b"# test\n10.0.0.0/8\n172.16.0.0/12\n192.168.0.0/16\n".to_vec();
        let shrunk = b"10.0.0.0/8\n".to_vec();
        let (url, server) = serve(vec![(200, v1), (304, Vec::new()), (200, shrunk)]);
        let (store, filter, ips, limits) = setup(vec![list(&url, ListKind::Ip, false)]);
        let agent = agent(&limits);
        assert_eq!(
            refresh_due(&store, &agent, &limits, &filter, &ips, 1_000),
            1
        );
        assert!(ips.contains(ip("10.1.2.3")) && ips.contains(ip("192.168.4.5")));
        assert!(!ips.contains(ip("11.0.0.1")));
        // Not due again until the interval passes.
        assert_eq!(
            refresh_due(&store, &agent, &limits, &filter, &ips, 2_000),
            0
        );
        let later = 1_000 + 86_400;
        assert_eq!(
            refresh_due(&store, &agent, &limits, &filter, &ips, later),
            1
        );
        let sub = store.lock().unwrap().subscriptions().unwrap().remove(0);
        assert_eq!(sub.status, "unchanged");
        // Two thirds of the list vanished: held, and the applied version
        // stays.
        refresh_due(&store, &agent, &limits, &filter, &ips, later + 86_400);
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

    #[test]
    fn content_lists_drop_or_join_the_floor() {
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
        let agent = agent(&limits);
        assert_eq!(refresh_due(&store, &agent, &limits, &filter, &ips, 5), 2);
        sa.join().unwrap();
        sb.join().unwrap();
        let e: Arc<Engine> = filter.current();
        let item = |h: &'static [[u8; 20]]| Item {
            hashes: h,
            name: "x",
            files: &[],
            content_type: lodestar_classify::ContentType::Other,
        };
        let da = e.judge(&item(&[[0xaa; 20]])).unwrap();
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
        assert!(e.judge(&item(&[[0xaa; 20]])).is_none());
        assert!(e.blocked_hash(&[[0xbb; 20]]).is_none());
    }

    #[test]
    fn failures_are_recorded_not_applied() {
        let (url, server) = serve(vec![(404, Vec::new()), (200, b"\0\0binary".to_vec())]);
        let (store, filter, ips, limits) = setup(vec![list(&url, ListKind::Ip, false)]);
        let agent = agent(&limits);
        refresh_due(&store, &agent, &limits, &filter, &ips, 10);
        let sub = store.lock().unwrap().subscriptions().unwrap().remove(0);
        assert_eq!(sub.status, "error");
        assert!(sub.error.unwrap().contains("404"));
        refresh_due(&store, &agent, &limits, &filter, &ips, 10 + 86_400);
        let sub = store.lock().unwrap().subscriptions().unwrap().remove(0);
        assert!(sub.error.unwrap().contains("isn't text"));
        server.join().unwrap();
        assert!(ips.current().is_empty());
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
