//! The details section through the unverified pipeline (ENR-9b,
//! `docs/decisions/2026-11-06-ENR-9b.md`): `store_claims` stores a source's
//! clamped section as claim version 3, never beside an adult section, a
//! description or a publisher, and every text in it passes the full filter
//! before storage and again when the rules change (runner, #383: every
//! claim text that is stored passes the filter).

use std::sync::atomic::Ordering;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_filter::{Action, Core, CsList, Engine, Handle, Lists, Sealer, Switches, TermSet};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::unverified::{
    self, Ctx, Phase, UnverifiedStats, claims_kib, details_texts, store_claims,
};
use lodestar_sources::{Discovery, Found, Provenance, claims};
use lodestar_store::claims as stored;
use lodestar_store::unverified::Network;
use lodestar_store::{PreparedTorrent, SourceId, Store, StoreConfig, WriteBatch};
use proptest::prelude::*;

const T: i64 = 1_700_000_000;

const V1: &[u8] = include_bytes!("../../store/tests/fixtures/claims/v1.bin");
const V2: &[u8] = include_bytes!("../../store/tests/fixtures/claims/v2.bin");
const V3: &[u8] = include_bytes!("../../store/tests/fixtures/claims/v3.bin");

/// The core with stand-in term lists: `lexa` is sexual, `rolea` a child
/// role, so "rolea lexa" is a CS-4a pattern hit.
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

fn mem() -> (Store, SourceId, SourceId) {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", T).unwrap();
    let feed = store.register_source("feed", "1", T).unwrap();
    (store, dht, feed)
}

fn key(n: u8) -> [u8; 20] {
    [n; 20]
}

/// A discovery as the emitter hands it on: the claims clamped.
fn found(k: [u8; 20], source: SourceId, mut c: claims::Claims) -> Discovery {
    c.clamp();
    raw(k, source, c)
}

/// A discovery with the claims as given, not clamped.
fn raw(k: [u8; 20], source: SourceId, c: claims::Claims) -> Discovery {
    let mut found = Found::key(InfoHash(k), 0);
    found.claims = Some(Box::new(c));
    Discovery {
        source,
        found,
        at: T,
        reply: false,
    }
}

fn details() -> claims::Details {
    claims::Details {
        source_category: Some("TV > HD".into()),
        posted_at: Some(T),
        newsgroups: vec!["alt.binaries.hdtv".into()],
        alt_titles: vec![claims::AltTitle {
            language: "ja".into(),
            title: "某番組".into(),
        }],
        canonical_title: Some("Some Show".into()),
        genres: vec!["drama".into()],
        year: Some(2011),
        match_confidence: Some(90),
    }
}

fn with_details(name: &str, d: claims::Details) -> claims::Claims {
    claims::Claims {
        name: Some(name.into()),
        size: Some(5),
        ids: vec![claims::ExternalId {
            scheme: "imdb".into(),
            value: "tt0111161".into(),
        }],
        details: Some(Box::new(d)),
        ..claims::Claims::default()
    }
}

fn ingest(
    store: &mut Store,
    filter: &Handle,
    sealer: Option<&Sealer>,
    batch: &[Discovery],
) -> UnverifiedStats {
    ingest_as(store, filter, sealer, AdultSources::default(), batch)
}

/// [`ingest`] with `adult` the always-adult sources.
fn ingest_as(
    store: &mut Store,
    filter: &Handle,
    sealer: Option<&Sealer>,
    adult: AdultSources,
    batch: &[Discovery],
) -> UnverifiedStats {
    let stats = UnverifiedStats::default();
    let ctx = Ctx {
        sealer,
        quarantine_secs: 86_400,
        adult,
        ..Ctx::default()
    };
    unverified::ingest(store, &filter.current(), ctx, batch, &stats, T, &|_, _| {}).unwrap();
    stats
}

fn held(store: &Store, k: [u8; 20]) -> bool {
    store.held_among(&[k]).unwrap()[0]
}

fn n(c: &std::sync::atomic::AtomicU64) -> u64 {
    c.load(Ordering::Relaxed)
}

fn stored_item(store: &Store, k: [u8; 20]) -> Option<stored::Claims> {
    store
        .unverified(Network::Bt, &k)
        .unwrap()
        .map(|i| i.claims.expect("claims decode"))
}

fn info_bytes(name: &str) -> Vec<u8> {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(5)),
        (b"name".to_vec(), b(name.as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), b(&[0; 20])),
    ];
    to_vec(&OwnedValue::Dict(e.into_iter().collect()))
}

/// Stores a crawled torrent; its id and swarm hash.
fn crawled(store: &mut Store, name: &str, source: SourceId) -> (i64, [u8; 20]) {
    let p = store
        .preparer()
        .prepare(&Info::parse(&info_bytes(name)).unwrap(), None)
        .unwrap();
    let id = store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                torrent: &p,
                discovered_at: T,
                metadata_at: T,
                extra_flags: 0,
                source,
                verified: true,
            }],
            ..WriteBatch::default()
        })
        .unwrap()
        .prepared[0]
        .id();
    (id, p.info_hash.0)
}

/// Runs the refile sweep to its end under the current rules; what it
/// removed.
fn sweep(store: &mut Store, filter: &Handle) -> usize {
    let version = filter.current().version;
    let ctx = Ctx {
        quarantine_secs: 86_400,
        ..Ctx::default()
    };
    let (mut phase, mut after_item, mut after_claims) = (Phase::Items, 0, (0, 0));
    let mut removed = 0;
    loop {
        let s = unverified::refile_step(
            store,
            filter,
            version,
            phase,
            after_item,
            after_claims,
            ctx,
            T,
            &|_, _| {},
        )
        .unwrap();
        assert!(!s.rejected);
        removed += s.removed;
        after_item = s.after_item;
        after_claims = s.after_claims;
        if s.end {
            match phase {
                Phase::Items => phase = Phase::Claims,
                Phase::Claims => return removed,
            }
        }
    }
}

/// The source's claims and provenance a stored value came from, so a
/// golden blob can be sent through `store_claims`.
fn from_stored(c: &stored::Claims) -> (claims::Claims, Provenance) {
    let claims = claims::Claims {
        name: c.name.clone(),
        size: c.size,
        files: c
            .files
            .iter()
            .map(|f| claims::ClaimedFile {
                path: f.path.clone(),
                size: f.size,
            })
            .collect(),
        trackers: c.trackers.clone(),
        ids: c
            .ids
            .iter()
            .map(|i| claims::ExternalId {
                scheme: i.scheme.clone(),
                value: i.value.clone(),
            })
            .collect(),
        description: c.description.clone(),
        publisher: c.publisher.clone(),
        adult: c.adult.clone(),
        details: c.details.clone(),
    };
    let provenance = Provenance {
        external_ref: c.provenance.external_ref.clone(),
        published_at: c.provenance.published_at,
        confidence: match c.provenance.confidence {
            stored::Confidence::High => claims::Confidence::High,
            stored::Confidence::Medium => claims::Confidence::Medium,
            stored::Confidence::Low => claims::Confidence::Low,
        },
    };
    (claims, provenance)
}

/// A golden blob's plain body: its bytes when stored plain; for a
/// compressed one, the store's own decoder and plain encoder, which the
/// store's golden test checks against the bytes.
fn plain(blob: &[u8]) -> Vec<u8> {
    match blob[1] {
        0 => blob[2..].to_vec(),
        _ => stored::encode_plain(&stored::decode(blob).unwrap()).unwrap(),
    }
}

/// Versions 1 and 2 store exactly the bytes they did before ENR-9b, and
/// version 3 now stores as its golden blob: each golden value, sent the
/// way a source sends it, comes out of `store_claims` byte for byte.
#[test]
fn golden_claims_store_byte_identical_blobs() {
    for (blob, version) in [
        (V1, stored::VERSION),
        (V2, stored::VERSION_ADULT),
        (V3, stored::VERSION_DETAILS),
    ] {
        let want = stored::decode(blob).unwrap();
        let (c, p) = from_stored(&want);
        let mut d = raw(key(9), SourceId::new(1).unwrap(), c.clone());
        d.found.provenance = Some(Box::new(p.clone()));
        let got = store_claims(&d).expect("claims");
        assert_eq!(got, want, "version {version}");
        assert_eq!(stored::encode_plain(&got).unwrap(), plain(blob));
        assert_eq!(stored::encode(&got, 3).unwrap()[0], version);

        // Clamped as the emitter does, the same.
        let mut clamped = c;
        clamped.clamp();
        let mut d = raw(key(9), SourceId::new(1).unwrap(), clamped);
        d.found.provenance = Some(Box::new(p));
        assert_eq!(store_claims(&d).unwrap(), want, "version {version}");
    }
}

/// A section stores as version 3 through ingest, as an unverified item and
/// as claims on a stored torrent, and reads back unchanged.
#[test]
fn a_details_section_stores_as_v3_and_round_trips() {
    let (mut store, dht, feed) = mem();
    let (id, h) = crawled(&mut store, "Some Show S01", dht);
    let filter = stand_in();
    let stats = ingest(
        &mut store,
        &filter,
        None,
        &[
            found(key(1), feed, with_details("Some.Show.S01E01", details())),
            found(h, feed, with_details("Some.Show.S01", details())),
        ],
    );
    assert_eq!((n(&stats.stored), n(&stats.on_torrent)), (1, 1));
    let item = stored_item(&store, key(1)).unwrap();
    assert_eq!(item.details.as_deref(), Some(&details()));
    assert_eq!(item.version(), stored::VERSION_DETAILS);
    let blob = stored::encode(&item, 3).unwrap();
    assert_eq!(blob[0], stored::VERSION_DETAILS);
    assert_eq!(stored::decode(&blob).unwrap(), item);
    let on = store.torrent_claims(id).unwrap();
    assert_eq!(on.len(), 1);
    let on = on[0].claims.as_ref().unwrap();
    assert_eq!(on.details.as_deref(), Some(&details()));
    assert_eq!(on.ids, item.ids);
}

/// One section a row: a details section never goes in beside an adult
/// section, even when the claims weren't clamped (the clamp drops it, and
/// `store_claims` doesn't rely on that).
#[test]
fn never_beside_an_adult_section() {
    let src = SourceId::new(1).unwrap();
    let both = claims::Claims {
        name: Some("Scene".into()),
        adult: Some(claims::AdultEnrichment::new("123")),
        details: Some(Box::new(details())),
        ..claims::Claims::default()
    };
    let s = store_claims(&raw(key(1), src, both.clone())).unwrap();
    assert!(s.adult.is_some() && s.details.is_none());
    assert!(s.check().is_ok());
    assert_eq!(stored::encode(&s, 3).unwrap()[0], stored::VERSION_ADULT);
    let s = store_claims(&found(key(1), src, both)).unwrap();
    assert!(s.adult.is_some() && s.details.is_none());
}

/// No personal fields beside the section (ENR-9 "No personal fields"):
/// clamped claims lose the description and publisher and keep the
/// section; claims that weren't clamped keep what version 1 stored and
/// lose the section. Either way the store accepts them.
#[test]
fn never_beside_a_description_or_publisher() {
    let (mut store, _, feed) = mem();
    let personal = |description: Option<&str>, publisher: Option<&str>| claims::Claims {
        description: description.map(String::from),
        publisher: publisher.map(String::from),
        ..with_details("Some.Show.S01E01", details())
    };
    for (d, p) in [
        (Some("Starring A. Person"), None),
        (None, Some("poster@example.com")),
        (Some("Overview"), Some("uploader")),
    ] {
        let unclamped = store_claims(&raw(key(1), feed, personal(d, p))).unwrap();
        assert!(unclamped.details.is_none());
        assert_eq!(
            (
                unclamped.description.as_deref(),
                unclamped.publisher.as_deref()
            ),
            (d, p)
        );
        assert!(unclamped.check().is_ok());
        assert_eq!(stored::encode(&unclamped, 3).unwrap()[0], stored::VERSION);

        let clamped = store_claims(&found(key(1), feed, personal(d, p))).unwrap();
        assert_eq!(clamped.details.as_deref(), Some(&details()));
        assert!(clamped.description.is_none() && clamped.publisher.is_none());
        assert!(clamped.check().is_ok());
    }
    let stats = ingest(
        &mut store,
        &stand_in(),
        None,
        &[found(key(2), feed, personal(Some("x"), Some("y")))],
    );
    assert_eq!(n(&stats.stored), 1);
    let item = stored_item(&store, key(2)).unwrap();
    assert!(item.details.is_some());
    assert!(item.description.is_none() && item.publisher.is_none());
}

/// A section the store wouldn't take beside these claims (here an id it
/// refuses beside a section, sent unclamped) is dropped, and the rest is
/// stored as version 1, never refused as a whole.
#[test]
fn a_section_the_store_would_refuse_is_dropped_not_the_claims() {
    let (mut store, _, feed) = mem();
    let mut c = with_details("Some.Show.S01E01", details());
    c.ids.push(claims::ExternalId {
        scheme: "tvrage".into(),
        value: "1".into(),
    });
    let s = store_claims(&raw(key(1), feed, c.clone())).unwrap();
    assert!(s.details.is_none() && s.check().is_ok());
    let stats = ingest(&mut store, &stand_in(), None, &[raw(key(1), feed, c)]);
    assert_eq!((n(&stats.stored), n(&stats.invalid)), (1, 0));
    assert!(stored_item(&store, key(1)).unwrap().details.is_none());
}

/// A private find keeps its section: none of its fields is a credential
/// or a link, and the row carries the private flag (ENR-9b decisions).
#[test]
fn a_private_find_keeps_its_section() {
    let mut d = found(
        key(1),
        SourceId::new(1).unwrap(),
        with_details("X", details()),
    );
    d.found.private_source = true;
    let s = store_claims(&d).unwrap();
    assert_eq!(s.details.as_deref(), Some(&details()));
}

/// Each free-text field of the section, and the joined texts.
#[test]
fn the_judged_texts_are_every_free_text_field() {
    let d = details();
    let fields = [
        "Some Show",
        "某番組",
        "TV > HD",
        "drama",
        "alt.binaries.hdtv",
    ];
    let texts = details_texts(&d, &[Some("Name"), Some("Verified"), Some("Name"), None]);
    let together = fields.join(" ");
    for t in fields {
        assert!(texts.iter().any(|x| x == t), "{t} alone");
        for name in ["Name", "Verified"] {
            let pair = format!("{name} {t}");
            assert_eq!(texts.iter().filter(|x| **x == pair).count(), 1, "{pair}");
        }
    }
    assert!(texts.contains(&together));
    assert!(texts.iter().any(|x| *x == format!("Verified {together}")));
    // 5 alone, together, and 6 for each distinct name.
    assert_eq!(texts.len(), 5 + 1 + 2 * 6);
    // Numbers and language tags aren't text: never judged.
    assert!(!texts.iter().any(|x| x.contains("2011") || x.contains("ja")));
    assert!(details_texts(&claims::Details::default(), &[Some("Name")]).is_empty());
    assert_eq!(details_texts(&d, &[]).len(), 6);
}

/// Your rule on any one free-text field drops the source's claims and
/// holds nothing, exactly as on the claimed name (runner, #383); the
/// floor or core on any one holds the key, quarantined with the text
/// sealed.
#[test]
fn a_blocked_field_blocks_the_key() {
    type Set = fn(&mut claims::Details, &str);
    let fields: [(&str, Set); 5] = [
        ("alternative title", |d, t| {
            d.alt_titles.push(claims::AltTitle {
                language: "en".into(),
                title: t.into(),
            })
        }),
        ("category", |d, t| d.source_category = Some(t.into())),
        ("canonical title", |d, t| d.canonical_title = Some(t.into())),
        ("genre", |d, t| d.genres.push(t.to_lowercase())),
        ("newsgroup", |d, t| {
            d.newsgroups
                .push(format!("alt.{}", t.to_lowercase().replace(' ', ".")))
        }),
    ];
    let filter = stand_in();
    rule(&filter, "badword", Action::Drop);
    let sealer = Sealer::new(&[9; 32]);
    for (i, (what, set)) in fields.into_iter().enumerate() {
        let (mut store, _, feed) = mem();
        let mut blocked = details();
        set(&mut blocked, "Badword");
        let mut core = details();
        set(&mut core, "Rolea Lexa");
        let batch = [
            found(key(1), feed, with_details("Plain Show", blocked)),
            found(key(2), feed, with_details("Plain Show", core)),
        ];
        let stats = ingest(&mut store, &filter, Some(&sealer), &batch);
        assert_eq!((n(&stats.dropped), n(&stats.held)), (1, 1), "{what} ({i})");
        assert_eq!(n(&stats.stored), 0, "{what}");
        // Your rule: dropped, nothing held.
        assert!(stored_item(&store, key(1)).is_none(), "{what}");
        assert!(!held(&store, key(1)), "{what}");
        // The core: held, never stored.
        assert!(stored_item(&store, key(2)).is_none(), "{what}");
        assert!(held(&store, key(2)), "{what}");
        let q = store.quarantined(10).unwrap();
        let q = q.iter().find(|q| q.hash == key(2)).expect(what);
        assert!(q.rule.starts_with("CS-"), "{what}: {}", q.rule);
        assert!(sealer.open(q.sealed.as_deref().unwrap()).is_ok());
    }
}

/// The joined texts only add holds: a core pattern split between the
/// claimed name and a genre holds the key, which neither alone would.
#[test]
fn a_pattern_split_across_the_name_and_a_field_holds() {
    let (mut store, _, feed) = mem();
    let filter = stand_in();
    let mut d = details();
    // Padding in another field doesn't hide it: each text is joined with
    // the name on its own.
    d.canonical_title = Some("one two three four five six seven eight nine ten".into());
    d.alt_titles = (0..claims::MAX_ALT_TITLES)
        .map(|i| claims::AltTitle {
            language: "en".into(),
            title: format!("filler title number {i}"),
        })
        .collect();
    d.genres = vec!["rolea".into()];
    let stats = ingest(
        &mut store,
        &filter,
        None,
        &[
            found(key(1), feed, with_details("Lexa Show", d.clone())),
            // Each half alone is clean.
            found(key(2), feed, with_details("Plain Show", d)),
            found(key(3), feed, with_details("Lexa Show", details())),
        ],
    );
    assert_eq!((n(&stats.held), n(&stats.stored)), (1, 2));
    assert!(held(&store, key(1)));
    assert!(stored_item(&store, key(2)).is_some());
    assert!(stored_item(&store, key(3)).is_some());
}

/// A section on a stored torrent is judged with the torrent's verified
/// name as well, at ingest and in the refile: a child-role genre on a
/// torrent whose verified name is sexual holds it, though neither the
/// claims (which name nothing) nor the torrent alone would.
#[test]
fn the_section_is_judged_with_the_verified_name() {
    let (mut store, dht, feed) = mem();
    let (id, h) = crawled(&mut store, "Lexa Film", dht);
    let genre = |g: &str| {
        let mut c = with_details(
            "ignored",
            claims::Details {
                genres: vec![g.into()],
                match_confidence: Some(80),
                ..claims::Details::default()
            },
        );
        c.name = None;
        c
    };
    let filter = stand_in();
    let stats = ingest(&mut store, &filter, None, &[found(h, feed, genre("rolea"))]);
    assert_eq!(n(&stats.held), 1);
    assert!(held(&store, h));
    assert!(store.torrent(id).unwrap().is_none());

    // The refile: stored while the core didn't know the role term.
    let (mut store, dht, feed) = mem();
    let (id, h) = crawled(&mut store, "Lexa Film", dht);
    let stats = ingest(&mut store, &filter, None, &[found(h, feed, genre("roleb"))]);
    assert_eq!(n(&stats.on_torrent), 1);
    assert_eq!(sweep(&mut store, &filter), 0);
    filter.update(|c| {
        let mut lists = c.lists().clone();
        lists.child_roles = TermSet::of(&["rolea", "roleb"]);
        c.set_lists(lists);
    });
    assert_eq!(sweep(&mut store, &filter), 1);
    assert!(held(&store, h));
    assert!(store.torrent(id).unwrap().is_none());
}

/// An always-adult source's section is judged as adult too, and the
/// stricter verdict stands: a pair the Adult-only core's wider window
/// catches holds there, and is stored from any other source.
#[test]
fn an_always_adult_sources_section_is_judged_as_adult() {
    // Ten words apart: only the Adult-only CS-5a (window 12) catches
    // them, as in `source_adult.rs`.
    let spread = "Studiox Rolea one two three four five six seven eight nine Lexa";
    let mut d = details();
    d.source_category = Some(spread.into());
    let c = with_details("Plain Show", d);
    let filter = stand_in();

    let (mut store, _, feed) = mem();
    let stats = ingest(&mut store, &filter, None, &[found(key(1), feed, c.clone())]);
    assert_eq!((n(&stats.stored), n(&stats.held)), (1, 0), "not adult");

    let (mut store, _, feed) = mem();
    let adult = AdultSources(1 << feed.get());
    let stats = ingest_as(&mut store, &filter, None, adult, &[found(key(1), feed, c)]);
    assert_eq!((n(&stats.stored), n(&stats.held)), (0, 1), "adult");
    assert!(held(&store, key(1)));
}

/// The refile: a rule added after storage blocks a key through an
/// alternative title alone, on an unverified item and on a stored
/// torrent's claims; a core term added later holds the torrent's hash.
#[test]
fn a_rule_added_after_storage_blocks_through_an_alternative_title_alone() {
    let (mut store, dht, feed) = mem();
    let (kept_id, kept_h) = crawled(&mut store, "Kept Show", dht);
    let (gone_id, gone_h) = crawled(&mut store, "Other Show", dht);
    let filter = stand_in();
    let titled = |t: &str| {
        let mut d = details();
        d.alt_titles.push(claims::AltTitle {
            language: "en".into(),
            title: t.into(),
        });
        with_details("Plain Show", d)
    };
    let stats = ingest(
        &mut store,
        &filter,
        None,
        &[
            found(key(1), feed, titled("Badword Saga")),
            found(key(2), feed, titled("Plain Saga")),
            found(kept_h, feed, titled("Badword Saga")),
            found(gone_h, feed, titled("Codez Saga")),
        ],
    );
    assert_eq!((n(&stats.stored), n(&stats.on_torrent)), (2, 2));
    assert_eq!(sweep(&mut store, &filter), 0, "nothing matches yet");

    rule(&filter, "badword", Action::Drop);
    filter.update(|c| {
        let mut lists = c.lists().clone();
        lists.codes = TermSet::of(&["codez"]);
        c.set_lists(lists);
    });
    assert_eq!(sweep(&mut store, &filter), 3, "an item and two claims");
    assert!(stored_item(&store, key(1)).is_none());
    assert!(!held(&store, key(1)), "your rule holds nothing");
    assert!(stored_item(&store, key(2)).is_some());
    assert!(store.torrent(kept_id).unwrap().is_some());
    assert!(store.torrent_claims(kept_id).unwrap().is_empty());
    assert!(!held(&store, kept_h));
    assert!(store.torrent(gone_id).unwrap().is_none());
    assert!(held(&store, gone_h));
}

/// The bounded sizes: `claims_kib` counts the section at its charge, and
/// a full section beside a file list at the total still stores (the clamp
/// gives the section's room to it, cutting files).
#[test]
fn the_section_counts_toward_the_bounds() {
    let src = SourceId::new(1).unwrap();
    let full = claims::Details {
        alt_titles: (0..claims::MAX_ALT_TITLES)
            .map(|i| claims::AltTitle {
                language: "en".into(),
                title: format!("{i}{}", "t".repeat(claims::MAX_ALT_TITLE - 3)),
            })
            .collect(),
        canonical_title: Some("c".repeat(claims::MAX_CANONICAL_TITLE)),
        genres: (0..claims::MAX_GENRES)
            .map(|i| format!("genre {i}"))
            .collect(),
        ..details()
    };
    let bare = found(key(1), src, with_details("Name", details()));
    let mut no_details = with_details("Name", details());
    no_details.details = None;
    let without = claims_kib(&found(key(1), src, no_details));
    let with = found(key(1), src, with_details("Name", full.clone()));
    let charged = full.charged_len();
    assert!(charged > 16 * 1024);
    assert!(claims_kib(&with) >= without + u32::try_from(charged / 1024).unwrap());
    assert!(claims_kib(&with) > claims_kib(&bare));

    // Files up to the total: the clamp cuts them to fit the section.
    let mut big = with_details("Name", full);
    big.files = (0..2_000)
        .map(|i| claims::ClaimedFile {
            path: format!("{i:04}{}", "p".repeat(1020)),
            size: Some(1),
        })
        .collect();
    let d = found(key(1), src, big);
    let s = store_claims(&d).unwrap();
    assert!(s.details.is_some());
    assert!(s.text_len() <= stored::MAX_TOTAL);
    assert!(s.check().is_ok());
    assert!(claims_kib(&d) <= unverified::CLAIMS_STEP_KIB);
    let blob = stored::encode(&s, 3).unwrap();
    assert!(blob.len() <= stored::MAX_STORED);
    assert_eq!(stored::decode(&blob).unwrap(), s);
}

fn text(max: usize) -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[a-zA-Z0-9 .>_-]{1,12}",
        "\\PC{0,8}",
        (1..=max + 4).prop_map(|n| "x".repeat(n)),
    ]
}

fn any_details() -> impl Strategy<Value = claims::Details> {
    (
        (
            proptest::option::of(text(claims::MAX_CATEGORY)),
            proptest::option::of(any::<i64>()),
            proptest::collection::vec(
                prop_oneof![
                    Just("alt.binaries.hdtv".to_string()),
                    "[a-zA-Z0-9.+-]{0,10}",
                    text(claims::MAX_NEWSGROUP),
                ],
                0..12,
            ),
        ),
        (
            proptest::collection::vec(
                (
                    prop_oneof![
                        Just("en".to_string()),
                        Just("ZH-Hant".to_string()),
                        "[a-zA-Z0-9-]{0,12}",
                    ],
                    text(claims::MAX_ALT_TITLE),
                )
                    .prop_map(|(language, title)| claims::AltTitle { language, title }),
                0..40,
            ),
            proptest::option::of(text(claims::MAX_CANONICAL_TITLE)),
            proptest::collection::vec(
                prop_oneof![Just("Drama".to_string()), text(claims::MAX_GENRE)],
                0..20,
            ),
            proptest::option::of(any::<u16>()),
            proptest::option::of(any::<u8>()),
        ),
    )
        .prop_map(
            |(
                (source_category, posted_at, newsgroups),
                (alt_titles, canonical_title, genres, year, match_confidence),
            )| claims::Details {
                source_category,
                posted_at,
                newsgroups,
                alt_titles,
                canonical_title,
                genres,
                year,
                match_confidence,
            },
        )
}

fn any_claims() -> impl Strategy<Value = claims::Claims> {
    (
        proptest::option::of(text(claims::MAX_NAME)),
        proptest::collection::vec(
            prop_oneof![
                Just(("imdb".to_string(), "tt0111161".to_string())),
                Just((
                    "mbid".to_string(),
                    "0B9E4C1A-6C1F-4F7E-9D0A-2F3E4D5C6B7A".to_string()
                )),
                Just(("tvrage".to_string(), "1".to_string())),
                ("[a-zA-Z]{0,6}", "[a-zA-Z0-9]{0,10}"),
            ],
            0..6,
        ),
        proptest::option::of(text(64)),
        proptest::option::of(text(64)),
        proptest::collection::vec(
            prop_oneof![
                Just("udp://tracker.example:1337/announce".to_string()),
                "[a-z:/.]{0,20}",
            ],
            0..3,
        ),
        proptest::bool::ANY,
        proptest::option::of(any_details()),
    )
        .prop_map(
            |(name, ids, description, publisher, trackers, adult, details)| claims::Claims {
                name,
                size: Some(1),
                files: vec![claims::ClaimedFile {
                    path: "a/b.mkv".into(),
                    size: Some(1),
                }],
                trackers,
                ids: ids
                    .into_iter()
                    .map(|(scheme, value)| claims::ExternalId { scheme, value })
                    .collect(),
                description,
                publisher,
                adult: adult.then(|| claims::AdultEnrichment::new("123")),
                details: details.map(Box::new),
            },
        )
}

/// Sections the store takes as they are: valid entries only, no
/// duplicates, at least `match_confidence`.
fn good_details() -> impl Strategy<Value = claims::Details> {
    (
        proptest::option::of("[a-zA-Z0-9 >]{1,40}"),
        proptest::option::of(0..=claims::MAX_POSTED_AT),
        proptest::collection::btree_set("alt\\.binaries\\.[a-z]{1,10}", 0..=claims::MAX_NEWSGROUPS),
        proptest::collection::btree_set(
            ("(en|ja|und|zh-hant)", "\\PC{1,40}"),
            0..=claims::MAX_ALT_TITLES,
        ),
        proptest::option::of("\\PC{1,80}"),
        proptest::collection::btree_set("[a-z]{1,12}( [a-z]{1,12})?", 0..=claims::MAX_GENRES),
        proptest::option::of(1u16..=9999),
        0..=claims::MAX_MATCH_CONFIDENCE,
    )
        .prop_map(
            |(source_category, posted_at, groups, alt, canonical_title, genres, year, conf)| {
                claims::Details {
                    source_category,
                    posted_at,
                    newsgroups: groups.into_iter().collect(),
                    alt_titles: alt
                        .into_iter()
                        .map(|(language, title)| claims::AltTitle { language, title })
                        .collect(),
                    canonical_title,
                    genres: genres.into_iter().collect(),
                    year,
                    match_confidence: Some(conf),
                }
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A well-formed section beside clean claims is stored exactly as
    /// sent, clamped or not, and round-trips as version 3.
    #[test]
    fn a_well_formed_section_is_stored_as_sent(
        d in good_details(),
        name in proptest::option::of("[a-zA-Z0-9 .]{1,40}"),
        private in proptest::bool::ANY,
    ) {
        let c = claims::Claims {
            name,
            details: Some(Box::new(d.clone())),
            ..with_details("x", claims::Details::default())
        };
        for clamp in [false, true] {
            let mut sent = raw(key(1), SourceId::new(1).unwrap(), c.clone());
            if clamp {
                sent = found(key(1), SourceId::new(1).unwrap(), c.clone());
            }
            sent.found.private_source = private;
            let s = store_claims(&sent).unwrap();
            prop_assert_eq!(s.details.as_deref(), Some(&d));
            let blob = stored::encode(&s, 3).unwrap();
            prop_assert_eq!(blob[0], stored::VERSION_DETAILS);
            prop_assert_eq!(stored::decode(&blob).unwrap(), s);
        }
    }

    /// Whatever a source sends, `store_claims` yields claims the store
    /// accepts, or drops the section: never an error or a panic.
    /// - Clamped by the emitter: the section survives exactly when the
    ///   clamp kept it, and the claims encode and round-trip.
    /// - The section sent as is, beside clamped claims: the claims are
    ///   still accepted, the section stored only as sent.
    #[test]
    fn store_claims_stores_what_the_store_accepts_or_drops_the_section(
        c in any_claims(),
        private in proptest::bool::ANY,
    ) {
        let src = SourceId::new(1).unwrap();
        let mut d = found(key(1), src, c.clone());
        d.found.private_source = private;
        let clamped = d.found.claims.clone().unwrap();
        let s = store_claims(&d).unwrap();
        prop_assert!(s.check().is_ok(), "{s:?}");
        prop_assert_eq!(s.details.is_some(), clamped.details.is_some());
        prop_assert!(!(s.details.is_some() && s.adult.is_some()));
        let blob = stored::encode(&s, 3).unwrap();
        prop_assert_eq!(blob[0], s.version());
        prop_assert_eq!(stored::decode(&blob).unwrap(), s);

        // The rest clamped, the section as the source sent it.
        let mut rest = c.clone();
        rest.details = None;
        rest.clamp();
        rest.details = c.details.clone();
        let mut d = raw(key(1), src, rest);
        d.found.private_source = private;
        let s = store_claims(&d).unwrap();
        prop_assert!(s.check().is_ok(), "{s:?}");
        if let Some(got) = &s.details {
            prop_assert_eq!(Some(got), c.details.as_ref());
        }
        prop_assert!(stored::encode(&s, 3).is_ok());
    }
}

/// Promotion through the running pipeline: a feed's details claims arrive
/// first as an unverified item, then the crawl fetches the torrent. The
/// item's section is judged with the verified name before it's promoted
/// (ENR-9b, runner 2026-11-06T21:48Z), so arrival order can't decide
/// whether the pair is judged.
mod promotion {
    use std::sync::Mutex;
    use std::time::Duration;

    use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
    use lodestar_pipeline::{Pipeline, PipelineConfig};
    use lodestar_sources::{
        BoxFuture, Consent, Kind, Manifest, Network as SourceNetwork, PluginError, PoolConfig,
        Priority, Risk, SourceCtx, SourcePlugin,
    };
    use tokio::sync::mpsc;

    use super::*;

    /// A source the test drives through a channel.
    struct Scripted {
        manifest: Manifest,
        rx: Mutex<Option<mpsc::Receiver<Found>>>,
    }

    fn scripted(id: &'static str) -> (std::sync::Arc<Scripted>, mpsc::Sender<Found>) {
        let (tx, rx) = mpsc::channel(8);
        let s = Scripted {
            manifest: Manifest {
                id,
                name: id,
                version: "1",
                kinds: &[Kind::Discovery],
                networks: &[SourceNetwork::Bt],
                endpoints: &[],
                risk: Risk::Low,
                consent: Consent::None,
                default_priority: Priority::Normal,
                enabled_by_default: true,
                always_adult: false,
                finds_private: false,
            },
            rx: Mutex::new(Some(rx)),
        };
        (std::sync::Arc::new(s), tx)
    }

    impl SourcePlugin for Scripted {
        fn manifest(&self) -> &Manifest {
            &self.manifest
        }

        fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
            let mut rx = self.rx.lock().unwrap().take().expect("run once");
            Box::pin(async move {
                loop {
                    tokio::select! {
                        _ = ctx.shutdown.requested() => return Ok(()),
                        Some(f) = rx.recv() => {
                            ctx.emitter.emit(f).await.map_err(|_| PluginError("closed".into()))?;
                        }
                    }
                }
            })
        }
    }

    fn config() -> PipelineConfig {
        PipelineConfig {
            fetch: lodestar_peer_wire::FetchOptions {
                allow_private: true,
                ..Default::default()
            },
            pool: PoolConfig {
                lookup_wait: Duration::from_millis(300),
                retry_backoff: Duration::from_millis(100),
                max_attempts: 2,
                ..PoolConfig::default()
            },
            flush_every: Duration::from_millis(100),
            batch_wait: Duration::from_millis(20),
            filter: std::sync::Arc::new(stand_in()),
            sealer: Some(std::sync::Arc::new(Sealer::new(&[9; 32]))),
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

    /// A child-role genre claimed first; the verified name, fetched later,
    /// is sexual. Neither alone is held: the promotion holds the pair, and
    /// a clean item beside it is promoted as usual.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_details_pair_completed_by_the_verified_name_holds_at_promotion() {
        let seeds = vec![
            Seed::v1(info_bytes("Lexa Show")),
            Seed::v1(info_bytes("Garden Show")),
        ];
        let keys: Vec<InfoHash> = seeds.iter().map(|s| InfoHash(s.info_hash)).collect();
        let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
        let (feed, feed_tx) = scripted("feed");
        let (crawl, crawl_tx) = scripted("crawl");
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let pipeline = Pipeline::start(config(), store, vec![feed, crawl])
            .await
            .unwrap();
        let mut d = details();
        d.genres = vec!["rolea".into()];
        for k in &keys {
            feed_tx
                .send(Found::key(*k, 0).with_claims(with_details("Plain Show", d.clone())))
                .await
                .unwrap();
        }
        until("unverified", || {
            n(&pipeline.writer_stats().unverified.stored) == 2
        })
        .await;
        for k in &keys {
            crawl_tx
                .send(Found::key(*k, 0).with_peers(vec![peer.addr()]))
                .await
                .unwrap();
        }
        until("held and promoted", || {
            let u = &pipeline.writer_stats().unverified;
            n(&u.held) == 1 && n(&u.promoted) == 1
        })
        .await;
        let store = pipeline.shutdown().await.unwrap();
        assert!(held(&store, keys[0].0));
        assert_eq!(store.find(&keys[0]).unwrap(), None);
        assert!(stored_item(&store, keys[0].0).is_none());
        let q = store.quarantined(10).unwrap();
        assert_eq!(q.len(), 1);
        assert!(q[0].hash == keys[0].0 && q[0].rule.starts_with("CS-"));
        // "Garden Show rolea": clean, promoted with its section.
        let id = store.find(&keys[1]).unwrap().expect("promoted");
        let on = store.torrent_claims(id).unwrap();
        assert_eq!(on[0].claims.as_ref().unwrap().details.as_deref(), Some(&d));
    }
}
