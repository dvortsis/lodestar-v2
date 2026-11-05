//! The torrent-cache plugin, wired into the host (SRC-13b;
//! `docs/decisions/2026-11-05-SRC-13b.md`, `2026-11-05-SRC-13.md`).
//!
//! - **Built from its saved settings** (`sources.config.torrent_cache`,
//!   [`saved_config`]). Settings that are missing or don't pass the
//!   plugin's rules give an empty list: the plugin is registered but
//!   fails "needs setup", and its egress reaches nothing.
//! - **Consent covers the configured hosts.** The plugin's manifest lists
//!   its config's hosts as its endpoints, which SRC-4's consent version
//!   digests: a list with a new or changed host switches the plugin off at
//!   the next start until it's acknowledged again.
//! - **Fed from two places** ([`run`]): the pipeline's [`Cursor`] over
//!   unverified items, a page at a time and never more than the lane has
//!   room for (`Why::Unverified`), and user interest, a search hit or a
//!   detail view (`Why::Interest`). An interest key and its
//!   `private_source` flag come from the store
//!   ([`torrent_cache::interest`]), never from the caller, and a key the
//!   store doesn't know isn't looked up.
//! - **Every verified file goes to the pipeline** (`Pipeline::torrent_cache_feed`),
//!   which judges it through the full filter and stores it through the
//!   writer's lease-free entry.
//!
//! Nothing here logs a hash.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_pipeline::torrent_cache::{self, Cursor, Feed, Fetched, PAGE};
use lodestar_sources_plugins::torrent_cache::{
    Config, Lookup, Queue, Stats, TorrentCache, Verified, Why,
};
use lodestar_store::{SourceSetting, Store};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub use lodestar_sources_plugins::torrent_cache::ID;

/// How often the cursor is asked for a page of unverified items. Each
/// cache is asked at most once a minute, so a page lasts at least as long.
pub const PAGE_EVERY: Duration = Duration::from_secs(60);

/// Interest hints waiting for their store read. A full queue drops the
/// hint: the next search or detail view sends it again.
pub const INTEREST_QUEUE: usize = 256;

/// Interest keys remembered as already queued, so the top of a search
/// that's run again doesn't queue them twice. Forgotten all at once when
/// full: the plugin's own lane is the real bound.
pub const INTEREST_SEEN: usize = 4096;

/// The plugin as the host builds it, and what the host keeps of it.
pub struct Built {
    pub plugin: Arc<TorrentCache>,
    pub queue: Queue,
    pub verified: mpsc::Receiver<Verified>,
    pub stats: Arc<Stats>,
}

/// The plugin's saved settings (blocking). Missing, unreadable or refused
/// settings are an empty list, never an error: the plugin then can't
/// reach anything. Nothing about them is logged.
pub fn saved_config(store: &Store) -> Config {
    match store.source_setting(ID, SourceSetting::Config) {
        Ok(Some(json)) => Config::from_json(&json).unwrap_or_default(),
        _ => Config::default(),
    }
}

/// The plugin, built from `config`.
pub fn build(config: Config) -> Built {
    let (plugin, queue, verified) = TorrentCache::new(config);
    let stats = plugin.stats_handle();
    Built {
        plugin: Arc::new(plugin),
        queue,
        verified,
        stats,
    }
}

/// Where the API's interest hints go: a torrent or item id the user saw
/// at the top of a search or opened. Cheap to clone; never waits.
#[derive(Debug, Clone)]
pub struct InterestHints(mpsc::Sender<i64>);

impl InterestHints {
    /// The hints and the receiver [`run`] reads them from.
    pub fn new() -> (InterestHints, mpsc::Receiver<i64>) {
        let (tx, rx) = mpsc::channel(INTEREST_QUEUE);
        (InterestHints(tx), rx)
    }

    pub fn hint(&self, id: i64) {
        let _ = self.0.try_send(id);
    }
}

/// What [`run`] works with.
pub struct Wiring {
    /// The pipeline's side: where verified files go.
    pub feed: Feed,
    pub verified: mpsc::Receiver<Verified>,
    pub queue: Queue,
    /// A store connection for the cursor and the interest reads.
    pub reader: Arc<Mutex<Store>>,
    pub interest: mpsc::Receiver<i64>,
    pub page_every: Duration,
}

/// Feeds the plugin's queue and hands its verified files to the pipeline,
/// until `stop` is set or the pipeline closes. Store reads run on the
/// blocking pool.
pub fn run(w: Wiring, mut stop: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Wiring {
            feed,
            mut verified,
            queue,
            reader,
            mut interest,
            page_every,
        } = w;
        let mut cursor = Cursor::default();
        let mut seen: HashSet<[u8; 20]> = HashSet::new();
        // The first page a whole interval after the start, then one per
        // interval.
        let mut tick =
            tokio::time::interval_at(tokio::time::Instant::now() + page_every, page_every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                v = verified.recv() => {
                    let Some(v) = v else { break };
                    if feed.send(fetched(&v)).await.is_err() {
                        break;
                    }
                }
                Some(id) = interest.recv() => {
                    let reader = reader.clone();
                    let found = tokio::task::spawn_blocking(move || {
                        let store = reader.lock().unwrap_or_else(|e| e.into_inner());
                        torrent_cache::interest(&store, id)
                    })
                    .await;
                    // A store error or an unknown id: nothing is looked up.
                    let Ok(Ok(Some(i))) = found else { continue };
                    if seen.contains(&i.key.0) {
                        continue;
                    }
                    // `None` for a private key: never queued (§18.7).
                    // Remembered only once queued, so a hint dropped on a
                    // full lane can come again.
                    if let Some(l) = Lookup::new(i.key, Why::Interest, i.private_source)
                        && queue.push(l)
                    {
                        if seen.len() >= INTEREST_SEEN {
                            seen.clear();
                        }
                        seen.insert(i.key.0);
                    }
                }
                _ = tick.tick() => {
                    let room = queue.room(Why::Unverified);
                    if room == 0 {
                        continue;
                    }
                    let reader = reader.clone();
                    let mut c = cursor;
                    let now = crate::unix_now();
                    let page = tokio::task::spawn_blocking(move || {
                        let store = reader.lock().unwrap_or_else(|e| e.into_inner());
                        let page = c.next(&store, room.min(PAGE), now);
                        (c, page)
                    })
                    .await;
                    let Ok((c, page)) = page else { continue };
                    // On a store error the cursor stays where it was.
                    let Ok(page) = page else { continue };
                    cursor = c;
                    for key in page.keys {
                        // The cursor never offers a private item.
                        if let Some(l) = Lookup::new(key, Why::Unverified, false) {
                            queue.push(l);
                        }
                    }
                }
            }
        }
    })
}

/// A verified file as the pipeline takes it: the raw info dict, and the
/// fields outside it as the plugin's claims.
fn fetched(v: &Verified) -> Fetched {
    Fetched {
        key: v.key(),
        info_raw: v.info_raw().to_vec(),
        claims: v.claims(),
        provenance: v.provenance(),
    }
}

/// The plugin's counters, for the status page and `/metrics`.
pub fn stats_json(s: &Stats, c: &torrent_cache::CacheStats) -> serde_json::Value {
    use std::sync::atomic::Ordering::Relaxed;
    serde_json::json!({
        "received": c.received.load(Relaxed),
        "lookups": s.lookups.load(Relaxed),
        "verified": s.verified.load(Relaxed),
        "mismatched": s.mismatched.load(Relaxed),
        "invalid": s.invalid.load(Relaxed),
        "failed": s.failed.load(Relaxed),
        "missed": s.missed.load(Relaxed),
        "skipped": s.skipped.load(Relaxed),
        "dropped": s.dropped.load(Relaxed),
        "stored": c.stored.load(Relaxed),
        "promoted": c.promoted.load(Relaxed),
        "existing": c.existing.load(Relaxed),
        "held": c.held.load(Relaxed),
        "refused": c.refused.load(Relaxed),
        "stale": c.stale.load(Relaxed),
        "errors": c.errors.load(Relaxed),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use lodestar_metainfo::{Info, InfoHash};
    use lodestar_sources::consent::{self, Ack};
    use lodestar_sources::{
        BoxFuture, Emitter, Fetch, HttpError, MemoryState, Request, Response, Shutdown, SourceCtx,
        SourcePlugin, UserLimits,
    };
    use lodestar_store::unverified::{Network, NewUnverified};
    use lodestar_store::{PreparedTorrent, SourceState, StoreConfig, WriteBatch, flags};

    use super::*;

    const T: i64 = 1_700_000_000;

    /// A cache that has nothing, and records every address asked.
    #[derive(Default)]
    struct Empty(Mutex<Vec<String>>);

    impl Fetch for Empty {
        fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
            self.0.lock().unwrap().push(req.url.clone());
            Box::pin(async {
                Ok(Response {
                    status: 404,
                    headers: Vec::new(),
                    body: Vec::new(),
                })
            })
        }
    }

    /// A one-file info dict named `name`.
    fn dict(name: &str) -> Vec<u8> {
        let mut v = format!("d6:lengthi5e4:name{}:{name}", name.len()).into_bytes();
        v.extend_from_slice(b"12:piece lengthi16384e6:pieces20:");
        v.extend_from_slice(&[7u8; 20]);
        v.push(b'e');
        v
    }

    /// Stores `name`'s torrent with `extra` flags: its id and key.
    fn stored(store: &mut Store, name: &str, extra: i64) -> (i64, InfoHash) {
        let raw = dict(name);
        let info = Info::parse(&raw).unwrap();
        let p = store.preparer().prepare(&info, Some(&raw)).unwrap();
        let w = store
            .write(&WriteBatch {
                prepared: &[PreparedTorrent {
                    torrent: &p,
                    discovered_at: T,
                    metadata_at: T,
                    extra_flags: extra,
                    source: lodestar_store::SourceId::new(0).unwrap(),
                    verified: true,
                }],
                ..WriteBatch::default()
            })
            .unwrap();
        (w.prepared[0].stored().unwrap(), info.primary_hash())
    }

    /// An unverified item for `key`: its id.
    fn item(store: &mut Store, key: [u8; 20], private_source: bool) -> i64 {
        let src = store
            .register_source_as("feed", "1", SourceState::Enabled, T)
            .unwrap();
        let c = lodestar_store::claims::Claims {
            name: Some("Holiday Film".into()),
            ..Default::default()
        };
        store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &key,
                source: src,
                private_source,
                claims: Some(&c),
                at: T,
            }])
            .unwrap();
        store.unverified(Network::Bt, &key).unwrap().unwrap().id
    }

    async fn until(what: &str, mut f: impl FnMut() -> bool) {
        // Polls with a 20 s deadline (standards §2).
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !f() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The runner's end-to-end check (SRC-13b): interest in a private
    /// item or torrent never reaches the plugin's fetch. The host hands
    /// over only an id; whether it may be looked up, and whether it's
    /// private, comes from the store.
    #[tokio::test]
    async fn interest_in_a_private_item_or_torrent_never_reaches_a_cache() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let private_item = item(&mut store, [0x11; 20], true);
        // A public item: the cursor's, not interest's (its page is an
        // hour away here).
        let public_item = item(&mut store, [0x22; 20], false);
        // An import with a partial file list, only private sources found.
        let (private_partial, private_key) = stored(
            &mut store,
            "Private.Partial",
            flags::FILES_PARTIAL | flags::PRIVATE_SOURCE,
        );
        // One marked private (BEP 27), and a public partial one.
        let (bep27, bep27_key) = stored(
            &mut store,
            "Bep27.Partial",
            flags::FILES_PARTIAL | flags::PRIVATE,
        );
        let (public_partial, public_key) =
            stored(&mut store, "Public.Partial", flags::FILES_PARTIAL);
        // A complete torrent: a cache has nothing to add.
        let (complete, complete_key) = stored(&mut store, "Complete", 0);
        let reader = Arc::new(Mutex::new(store));

        let built = build(Config::new(&["https://c.example/{hash}"]).unwrap());
        let fake = Arc::new(Empty::default());
        let (stop_plugin, shutdown) = Shutdown::new();
        let (emit, _found) = mpsc::channel(4);
        let source = lodestar_store::SourceId::new(9).unwrap();
        let ctx = SourceCtx {
            source,
            emitter: Emitter::new(source, emit),
            peer_requests: None,
            peer_replies: None,
            shutdown,
            kit: lodestar_sources::Kit::for_plugin(
                &*built.plugin,
                Arc::new(MemoryState::new()),
                fake.clone(),
                &UserLimits::default(),
            ),
        };
        let plugin = built.plugin.clone();
        let running = tokio::spawn(async move { plugin.run(ctx).await });
        let (feed, _fetched) = torrent_cache::feed();
        let (hints, interest) = InterestHints::new();
        let (stop, stopped) = watch::channel(false);
        let task = run(
            Wiring {
                feed,
                verified: built.verified,
                queue: built.queue,
                reader,
                interest,
                page_every: Duration::from_secs(3600),
            },
            stopped,
        );
        for id in [
            private_item,
            private_partial,
            bep27,
            complete,
            999_999,
            public_partial,
        ] {
            hints.hint(id);
        }
        let asked = |k: &[u8; 20]| {
            let hex = InfoHash(*k).to_string();
            fake.0.lock().unwrap().iter().any(|u| u.contains(&hex))
        };
        // The public partial torrent, hinted last, is the first and only
        // key asked: the private ones never reached the queue.
        until("the public key", || asked(&public_key.0)).await;
        until("its lookup to end", || {
            built.stats.missed.load(Ordering::Relaxed) == 1
        })
        .await;
        for k in [[0x11; 20], private_key.0, bep27_key.0, complete_key.0] {
            assert!(!asked(&k), "a key that may not be looked up was sent");
        }
        assert_eq!(fake.0.lock().unwrap().len(), 1);
        assert_eq!(built.stats.lookups.load(Ordering::Relaxed), 1);
        let _ = public_item;
        let _ = stop.send(true);
        task.await.unwrap();
        let _ = stop_plugin.send(true);
        running.await.unwrap().unwrap();
    }

    /// Settings that are missing or refused give an empty list, which
    /// reaches nothing; saved settings build the plugin with their hosts.
    #[test]
    fn the_plugin_is_built_from_its_saved_settings() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        assert!(saved_config(&store).caches().is_empty());
        let id = store
            .register_source_as(ID, "1", SourceState::Disabled, T)
            .unwrap();
        let save = |store: &mut Store, json: &str| {
            store
                .set_source_settings(id, &[(SourceSetting::Config, Some(json))])
                .unwrap();
        };
        save(&mut store, r#"{"caches":["http://plain.example/{hash}"]}"#);
        assert!(
            saved_config(&store).caches().is_empty(),
            "refused: not https"
        );
        save(&mut store, "not json");
        assert!(saved_config(&store).caches().is_empty());
        save(&mut store, r#"{"caches":["https://a.example/t/{hash}"]}"#);
        let built = build(saved_config(&store));
        assert_eq!(built.plugin.config().caches().len(), 1);
        assert!(SourcePlugin::endpoints(&*built.plugin).is_some());
        let hosts: Vec<&str> = built
            .plugin
            .manifest()
            .endpoints
            .iter()
            .map(|e| e.host)
            .collect();
        assert_eq!(hosts, ["a.example"]);
    }

    /// The start's consent check (`need_in`, as the pipeline's start runs
    /// it) on a plugin rebuilt from changed settings: a new host switches
    /// it off until it's acknowledged again; the same list doesn't.
    #[test]
    fn a_saved_acknowledgement_covers_only_the_hosts_it_was_shown() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let id = store
            .register_source_as(ID, "1", SourceState::Disabled, T)
            .unwrap();
        let shown = build(Config::new(&["https://a.example/{hash}"]).unwrap());
        store
            .enable_source(id, Some(&Ack::of(shown.plugin.manifest()).encode()))
            .unwrap();
        let again = build(Config::new(&["https://a.example/{hash}"]).unwrap());
        assert_eq!(
            consent::need_in(&store, again.plugin.manifest()).unwrap(),
            None
        );
        let added =
            build(Config::new(&["https://a.example/{hash}", "https://b.example/{hash}"]).unwrap());
        assert_eq!(
            consent::need_in(&store, added.plugin.manifest()).unwrap(),
            Some(consent::Need::Endpoints)
        );
    }
}
