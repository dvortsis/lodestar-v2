//! The pipeline's side of torrent-cache enrichment (SRC-13,
//! `docs/decisions/2026-11-05-SRC-13.md`): the cursor never offers a
//! `private_source` key, and a cached file passes the full filter before
//! it may be stored, as a BEP 9 fetch does.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_filter::{Action, Core, CsList, Engine, Handle, Lists, Switches, TermSet};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::torrent_cache::{self, Cursor, Judged, PAGE, Refusal};
use lodestar_pipeline::unverified::VERIFY_ROUND_SECS;
use lodestar_store::claims::Claims;
use lodestar_store::unverified::{Network, NewUnverified};
use lodestar_store::{Store, StoreConfig};

const T: i64 = 1_700_000_000;

/// The core with stand-in term lists: `lexa` is sexual, `rolea` a child
/// role, so "rolea lexa" is a CS pattern hit.
fn stand_in() -> Handle {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Handle::new(Engine::new(Core::new(
        lists,
        CsList::default(),
        Switches::default(),
    )))
}

fn rule(filter: &Handle, query: &str, action: Action) {
    filter.update_engine(|e| {
        let notes = e.load_rules(&lodestar_store::RuleSet {
            rules: vec![lodestar_store::RuleRow {
                id: 1,
                query: query.into(),
                action: action.code(),
                enabled: true,
                shadow_until: None,
                unless_query: None,
            }],
            ..Default::default()
        });
        assert!(notes.is_empty(), "{notes:?}");
        e.version += 1;
    });
}

fn info_bytes(name: &str) -> Vec<u8> {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(900 << 20)),
        (b"name".to_vec(), b(name.as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
        (b"pieces".to_vec(), b(&[0; 20 * 900])),
    ];
    to_vec(&OwnedValue::Dict(e.into_iter().collect()))
}

fn judge(filter: &Handle, name: &str) -> (InfoHash, Judged) {
    let raw = info_bytes(name);
    let key = Info::parse(&raw).unwrap().primary_hash();
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let mut preparer = store.preparer();
    let j = torrent_cache::judge(&filter.current(), &mut preparer, key, &raw, T);
    (key, j)
}

#[test]
fn a_clean_file_is_admitted_as_verified_metadata() {
    let (key, j) = judge(&stand_in(), "Sintel 2010 1080p.mkv");
    let Judged::Store(a) = j else {
        panic!("not admitted: {j:?}");
    };
    assert_eq!(a.key, key);
    assert_eq!(a.torrent.info_hash, key);
    assert!(a.torrent.enrichment.is_some(), "enrichment ran");
    assert!(a.verdict.decision.is_none());
    assert_eq!(torrent_cache::PROVENANCE, "torrent cache");
}

#[test]
fn a_cs_pattern_name_is_held_never_stored() {
    let (_, j) = judge(&stand_in(), "rolea lexa.mp4");
    let Judged::Held { verdict, .. } = j else {
        panic!("a CS hit was admitted: {j:?}");
    };
    assert!(matches!(
        verdict.decision.map(|d| d.action),
        Some(Action::Drop | Action::Quarantine)
    ));
}

#[test]
fn a_name_your_rules_drop_is_held() {
    let filter = stand_in();
    rule(&filter, "bunny", Action::Drop);
    let (key, j) = judge(&filter, "Big Buck Bunny.mkv");
    let Judged::Held {
        hashes, version, ..
    } = j
    else {
        panic!("a dropped name was admitted: {j:?}");
    };
    assert_eq!(hashes, [key.0]);
    assert_eq!(version, filter.current().version);
    // A weaker rule leaves it stored, marked.
    let filter = stand_in();
    rule(&filter, "bunny", Action::Flag);
    assert!(matches!(
        judge(&filter, "Big Buck Bunny.mkv").1,
        Judged::Store(_)
    ));
}

#[test]
fn a_file_for_another_key_is_refused() {
    let raw = info_bytes("Sintel.mkv");
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let j = torrent_cache::judge(
        &stand_in().current(),
        &mut store.preparer(),
        InfoHash([9; 20]),
        &raw,
        T,
    );
    assert!(matches!(j, Judged::Refused(Refusal::Mismatch)));
}

fn claims(name: &str) -> Claims {
    Claims {
        name: Some(name.into()),
        ..Claims::default()
    }
}

#[test]
fn the_cursor_never_offers_a_private_source_key() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let src = store.register_source("nyaa", "1", T).unwrap();
    let c = claims("Plain Film 2020");
    let keys: Vec<([u8; 20], bool)> = (1..=5u8).map(|i| ([i; 20], i % 2 == 0)).collect();
    let items: Vec<NewUnverified<'_>> = keys
        .iter()
        .map(|(k, private)| NewUnverified {
            network: Network::Bt,
            key: k,
            source: src,
            private_source: *private,
            claims: Some(&c),
            at: T,
        })
        .collect();
    store.put_unverified(&items).unwrap();

    let mut cursor = Cursor::default();
    let page = cursor.next(&store, 2, T).unwrap();
    assert_eq!(page.keys, [InfoHash([1; 20])]);
    assert_eq!(page.private, 1);
    let page = cursor.next(&store, PAGE, T).unwrap();
    assert_eq!(page.keys, [InfoHash([3; 20]), InfoHash([5; 20])]);
    assert_eq!(page.private, 1);

    // The end: the round rests, then starts over.
    assert!(cursor.next(&store, PAGE, T).unwrap().keys.is_empty());
    assert!(cursor.next(&store, PAGE, T + 60).unwrap().keys.is_empty());
    let again = cursor.next(&store, PAGE, T + VERIFY_ROUND_SECS).unwrap();
    assert_eq!(again.keys.len(), 3);
    assert!(
        !again
            .keys
            .iter()
            .any(|k| keys.iter().any(|(p, private)| *private && *p == k.0))
    );
}

#[test]
fn a_key_on_the_floors_hash_list_is_refused_before_preparing() {
    let raw = info_bytes("Sintel.mkv");
    let key = Info::parse(&raw).unwrap().primary_hash();
    let mut cs = CsList::default();
    cs.add_hash(key.0, lodestar_filter::Origin::Imported);
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let engine = Engine::new(Core::new(lists, cs, Switches::default()));
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let j = torrent_cache::judge(&engine, &mut store.preparer(), key, &raw, T);
    assert!(matches!(j, Judged::Refused(Refusal::Blocked)));
}

/// The plugin's recorded cache reply, judged whole: the file the
/// plugin's contract test verifies is the file the filter judges.
fn fixture_info() -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sources-plugins/fixtures/torrent_cache/hit/002.body");
    let file = std::fs::read(path).unwrap();
    lodestar_metainfo::TorrentFile::raw_info(&file, &lodestar_metainfo::Limits::DEFAULT)
        .unwrap()
        .to_vec()
}

#[test]
fn the_recorded_file_is_admitted_and_a_rule_on_its_name_holds_it() {
    let raw = fixture_info();
    let key: InfoHash = "695b0b3debb14473d741f7a3a86561e42009d6dc".parse().unwrap();
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let filter = stand_in();
    let j = torrent_cache::judge(&filter.current(), &mut store.preparer(), key, &raw, T);
    assert!(matches!(j, Judged::Store(_)), "{j:?}");
    rule(&filter, "sintel", Action::Drop);
    let j = torrent_cache::judge(&filter.current(), &mut store.preparer(), key, &raw, T);
    assert!(matches!(j, Judged::Held { .. }), "{j:?}");
}

/// The same GBK name in a file whose unhashed `encoding` key says
/// windows-1252: judged from the raw span with no hint, the name decodes
/// as the hint-free bytes do, and a rule on that name still holds it.
#[test]
fn the_files_encoding_key_cant_change_how_the_name_is_judged() {
    let gbk = [0xd6, 0xd0, 0xce, 0xc4]; // GBK for a two-character word
    let raw = [
        &b"d6:lengthi944766976e4:name4:"[..],
        &gbk,
        b"12:piece lengthi1048576e6:pieces",
        format!("{}:", 20 * 901).as_bytes(),
        &vec![0u8; 20 * 901],
        b"e",
    ]
    .concat();
    let key = Info::parse(&raw).unwrap().primary_hash();
    let hint_free = Info::parse(&raw).unwrap().name.decode().into_owned();
    let lied = [&b"d8:encoding12:windows-12524:info"[..], &raw, b"e"].concat();
    let span = lodestar_metainfo::TorrentFile::raw_info(&lied, &lodestar_metainfo::Limits::DEFAULT)
        .unwrap();
    assert_eq!(span, &raw[..]);
    // The whole-file parse would have taken the lie.
    let hinted = lodestar_metainfo::TorrentFile::parse(&lied)
        .unwrap()
        .info
        .name
        .decode()
        .into_owned();
    assert_ne!(hinted, hint_free);

    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let Judged::Store(a) =
        torrent_cache::judge(&stand_in().current(), &mut store.preparer(), key, span, T)
    else {
        panic!("a clean name wasn't admitted");
    };
    assert_eq!(a.torrent.name, hint_free);
    let filter = stand_in();
    rule(&filter, &hint_free, Action::Drop);
    let j = torrent_cache::judge(&filter.current(), &mut store.preparer(), key, span, T);
    assert!(matches!(j, Judged::Held { .. }), "{j:?}");
}

// ---- through the pipeline: the writer's lease-free entry (SRC-13b) ----

mod wired {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use lodestar_classify::ContentType;
    use lodestar_metainfo::{Info, InfoHash};
    use lodestar_pipeline::torrent_cache::{Feed, Fetched};
    use lodestar_pipeline::{Pipeline, PipelineConfig, WriterStats};
    use lodestar_sources::claims::{Claims as Found, Confidence, Provenance};
    use lodestar_sources::{
        BoxFuture, Consent, Kind, Manifest, Network as Net, PluginError, Priority, Risk, SourceCtx,
        SourcePlugin,
    };
    use lodestar_store::claims::Claims;
    use lodestar_store::unverified::{Network, NewUnverified};
    use lodestar_store::{Store, StoreConfig};

    use super::{T, info_bytes, stand_in};

    const CACHE: &str = "torrent_cache";

    /// A source that does nothing until it's stopped: the stand-in for
    /// the torrent-cache plugin (whose files the test hands over itself)
    /// and for the sources whose unverified items it looks up.
    struct Idle(Manifest);

    fn idle(id: &'static str, on: bool, always_adult: bool) -> Arc<dyn SourcePlugin> {
        Arc::new(Idle(Manifest {
            id,
            name: id,
            version: "1",
            kinds: &[Kind::Enrichment],
            networks: &[Net::Bt],
            endpoints: &[],
            risk: Risk::Low,
            consent: Consent::None,
            default_priority: Priority::Normal,
            enabled_by_default: on,
            always_adult,
            finds_private: false,
        }))
    }

    impl SourcePlugin for Idle {
        fn manifest(&self) -> &Manifest {
            &self.0
        }

        fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
            Box::pin(async move {
                ctx.shutdown.requested().await;
                Ok(())
            })
        }
    }

    fn config() -> PipelineConfig {
        PipelineConfig {
            flush_every: Duration::from_millis(100),
            batch_wait: Duration::from_millis(20),
            filter: Arc::new(stand_in()),
            sealer: Some(Arc::new(lodestar_filter::Sealer::new(
                &lodestar_filter::Sealer::generate_key().unwrap(),
            ))),
            torrent_cache: Some(CACHE),
            ..PipelineConfig::default()
        }
    }

    /// Polls for 10 s (standards §2), so the slowest runner passes.
    async fn until(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..400 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}");
    }

    fn n(a: &std::sync::atomic::AtomicU64) -> u64 {
        a.load(Ordering::Relaxed)
    }

    /// A store holding one unverified item for `name`'s info dict, found by
    /// source `by`: its key, its id and the dict.
    fn with_item(
        store: &mut Store,
        by: &str,
        claimed: &str,
        name: &str,
    ) -> (InfoHash, i64, Vec<u8>) {
        let src = store.register_source(by, "1", T).unwrap();
        let raw = info_bytes(name);
        let key = Info::parse(&raw).unwrap().primary_hash();
        let c = Claims {
            name: Some(claimed.into()),
            ..Claims::default()
        };
        store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &key.0,
                source: src,
                private_source: false,
                claims: Some(&c),
                at: T,
            }])
            .unwrap();
        let id = store.unverified(Network::Bt, &key.0).unwrap().unwrap().id;
        (key, id, raw)
    }

    fn fetched(key: InfoHash, raw: Vec<u8>) -> Fetched {
        Fetched {
            key,
            info_raw: raw,
            claims: Found {
                trackers: vec!["udp://tracker.example:1337/announce".into()],
                description: Some("Encoded by someone".into()),
                ..Found::default()
            },
            provenance: Provenance {
                external_ref: None,
                published_at: Some(T - 86_400),
                confidence: Confidence::Medium,
            },
        }
    }

    async fn start(store: Store, plugins: Vec<Arc<dyn SourcePlugin>>) -> (Pipeline, Feed) {
        let pipeline = Pipeline::start(config(), store, plugins).await.unwrap();
        let feed = pipeline.torrent_cache_feed().expect("the cache is enabled");
        (pipeline, feed)
    }

    fn stats(p: &Pipeline) -> &WriterStats {
        p.writer_stats()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cached_file_promotes_the_item_under_its_id_with_the_caches_claims() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let (key, item, raw) = with_item(&mut store, "feed", "Sintel", "Sintel 2010 1080p.mkv");
        let (pipeline, feed) = start(
            store,
            vec![idle("feed", true, false), idle(CACHE, true, false)],
        )
        .await;
        feed.send(fetched(key, raw)).await.unwrap();
        until("stored, with its claims", || {
            let s = stats(&pipeline);
            n(&s.torrent_cache.stored) == 1 && n(&s.unverified.on_torrent) == 1
        })
        .await;
        let s = stats(&pipeline);
        assert_eq!(n(&s.torrent_cache.promoted), 1);
        // Not the crawl's: nothing in the writer's own stored count, and
        // the cache's claims on it aren't a rediscovery.
        assert_eq!(n(&s.stored), 0);
        assert_eq!(n(&s.rediscovered), 0);
        let gate = pipeline.monitor().gate().clone();
        assert_eq!(
            gate.excluded(lodestar_pipeline::gate::Excluded::Rediscovered),
            0
        );
        drop(feed);
        let store = pipeline.shutdown().await.unwrap();
        // Promoted under the item's own id (§18.5), and the item is gone.
        assert_eq!(store.find(&key).unwrap(), Some(item));
        assert!(store.unverified(Network::Bt, &key.0).unwrap().is_none());
        let cache = store
            .sources()
            .unwrap()
            .into_iter()
            .find(|r| r.plugin == CACHE)
            .unwrap()
            .id;
        let row = store.torrent(item).unwrap().unwrap();
        assert_ne!(row.sources & (1 << cache.get()), 0, "the plugin's bit");
        assert_eq!(row.name, "Sintel 2010 1080p.mkv", "the verified name");
        // The fields outside the info dict, as the plugin's claims.
        let claims = store.torrent_claims(item).unwrap();
        let mine = claims
            .iter()
            .find(|c| c.source == cache)
            .expect("cache claims");
        let c = mine.claims.as_ref().unwrap();
        assert_eq!(c.trackers, ["udp://tracker.example:1337/announce"]);
        assert_eq!(c.description.as_deref(), Some("Encoded by someone"));
        assert_eq!(c.provenance.published_at, Some(T - 86_400));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_cached_file_is_never_stored_and_its_item_goes() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        // The claims look clean; the verified name is a CS pattern.
        let (key, _, raw) = with_item(&mut store, "feed", "Holiday", "rolea lexa.mp4");
        let (pipeline, feed) = start(
            store,
            vec![idle("feed", true, false), idle(CACHE, true, false)],
        )
        .await;
        feed.send(fetched(key, raw)).await.unwrap();
        until("held", || n(&stats(&pipeline).torrent_cache.held) == 1).await;
        assert_eq!(n(&stats(&pipeline).torrent_cache.stored), 0);
        drop(feed);
        let store = pipeline.shutdown().await.unwrap();
        assert!(store.find(&key).unwrap().is_none(), "never stored");
        assert!(store.unverified(Network::Bt, &key.0).unwrap().is_none());
        assert_eq!(store.held_among(&[key.0]).unwrap(), [true]);
        // The hold records the rule.
        let hits = store.filter_hits(0).unwrap();
        assert!(hits.iter().any(|h| h.hash == key.0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_always_adult_sources_item_stays_adult_through_a_cache() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let (key, item, raw) = with_item(&mut store, "adult", "Clips", "Garden Tour 2022.mkv");
        let (pipeline, feed) = start(
            store,
            vec![idle("adult", true, true), idle(CACHE, true, false)],
        )
        .await;
        feed.send(fetched(key, raw)).await.unwrap();
        until("stored", || n(&stats(&pipeline).torrent_cache.stored) == 1).await;
        drop(feed);
        let store = pipeline.shutdown().await.unwrap();
        assert_eq!(store.find(&key).unwrap(), Some(item));
        // The writer judged it through the always-adult mask: a clean name
        // from an always-adult source's item is stored Adult.
        assert_eq!(
            store.content_type(item).unwrap(),
            Some(ContentType::Adult.code())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_feed_while_the_plugin_is_off() {
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let pipeline = Pipeline::start(config(), store, vec![idle(CACHE, false, false)])
            .await
            .unwrap();
        assert!(pipeline.torrent_cache_feed().is_none());
        pipeline.shutdown().await.unwrap();
        // And none without the setting naming it.
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let cfg = PipelineConfig {
            torrent_cache: None,
            ..config()
        };
        let pipeline = Pipeline::start(cfg, store, vec![idle(CACHE, true, false)])
            .await
            .unwrap();
        assert!(pipeline.torrent_cache_feed().is_none());
        pipeline.shutdown().await.unwrap();
    }
}
