//! ENR-11: identification answers in the index. Alternative titles go
//! into `title` and external ids into `tag` terms, only from answers that
//! may be served (`Store::served_identifications`); `imdb:` and the other
//! id fields find them; and the `reindex_log` sweep brings torrents that
//! held answers before ENR-11 to the index, resumably.

use std::path::Path;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::query::{TagKey, id_value};
use lodestar_search::{DocEnrichment, Node, SearchIndex, index_dir, parse};
use lodestar_store::claims::{AltTitle, Claims, Details, ExternalId};
use lodestar_store::identify_sweep::{Identified, SweepState, TERMS_KEY};
use lodestar_store::{Enriched, NewTorrent, SourceId, Store, StoreConfig};
use proptest::prelude::*;

const HEAP: usize = 15 << 20;
const NOW: i64 = 1_700_000_100;

/// The torrents, by index: what each is for.
const NAMES: [&str; 9] = [
    // 0: served: ids and titles indexed.
    "The.Matrix.1999.1080p.BluRay-GRP",
    // 1: the answer came through a private source.
    "Heat.1995.1080p.BluRay-GRP",
    // 2: the answering source found the torrent itself.
    "Alien.1979.1080p.BluRay-GRP",
    // 3: the answering source's terms don't allow passing answers on.
    "Ronin.1998.1080p.BluRay-GRP",
    // 4: a private (BEP 27) torrent.
    "Brazil.1985.1080p.BluRay-GRP",
    // 5: a feed's details with no match confidence: not an answer.
    "Fargo.1996.1080p.BluRay-GRP",
    // 6: more alternative titles than are indexed.
    "Akira.1988.1080p.BluRay-GRP",
    // 7: no answer at all.
    "Gattaca.1997.1080p.BluRay-GRP",
    // 8: two sources agree on an id, and one adds another.
    "Seven.1995.1080p.BluRay-GRP",
];

fn torrent(i: usize, private: bool) -> Vec<u8> {
    let d = |entries: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let mut info = vec![
        ("length", OwnedValue::Int(1_000_000 + i as i64)),
        ("name", b(&format!("{}.mkv", NAMES[i]))),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20 * 16])),
    ];
    if private {
        info.push(("private", OwnedValue::Int(1)));
    }
    to_vec(&d(info))
}

struct Fixture {
    store: Store,
    ids: Vec<i64>,
    /// Finds every torrent.
    dht: SourceId,
    /// Redistributable (as Wikidata, CC0).
    open: SourceId,
    /// Not redistributable (as TMDB).
    closed: SourceId,
    /// A second redistributable source (as MusicBrainz).
    open2: SourceId,
}

/// A terms record (`lodestar_sources::terms::Terms::record`): version 1,
/// flags, the retention in seconds.
fn terms_record(redistribute: bool) -> Vec<u8> {
    let mut v = vec![1, u8::from(redistribute) << 1];
    v.extend((180u64 * 86_400).to_be_bytes());
    v
}

fn answer(confidence: Option<u8>, ids: &[(&str, &str)], canonical: &str) -> Claims {
    Claims {
        ids: ids
            .iter()
            .map(|(s, v)| ExternalId {
                scheme: (*s).into(),
                value: (*v).into(),
            })
            .collect(),
        details: Some(Box::new(Details {
            canonical_title: Some(canonical.into()),
            alt_titles: vec![AltTitle {
                language: "ja".into(),
                title: format!("{canonical} ja"),
            }],
            match_confidence: confidence,
            ..Details::default()
        })),
        ..Claims::default()
    }
}

fn fixture(root: &Path) -> Fixture {
    let mut store = Store::open(root.join("s.db"), StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", 1).unwrap();
    let open = store.register_source("wikidata", "1", 1).unwrap();
    let closed = store.register_source("tmdb", "1", 1).unwrap();
    let open2 = store.register_source("musicbrainz", "1", 1).unwrap();
    for (s, r) in [(open, true), (closed, false), (open2, true)] {
        store
            .set_plugin_state(s, TERMS_KEY, Some(&terms_record(r)))
            .unwrap();
    }
    let raws: Vec<Vec<u8>> = (0..NAMES.len()).map(|i| torrent(i, i == 4)).collect();
    let infos: Vec<Info> = raws.iter().map(|t| Info::parse(t).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .enumerate()
        .map(|(i, (info, raw))| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            extra_flags: 0,
            // Torrent 2 was found by the source that answers for it.
            source: if i == 2 { open } else { dht },
        })
        .collect();
    let ids: Vec<i64> = store
        .insert(&batch)
        .unwrap()
        .into_iter()
        .map(|i| i.id())
        .collect();
    let enriched: Vec<(i64, Enriched)> = ids
        .iter()
        .map(|&id| {
            let row = store.torrent(id).unwrap().unwrap();
            let tree = store.files(id).unwrap().unwrap();
            let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
            let c = lodestar_classify::classify(&row.name, &files);
            let e = Enriched {
                version: lodestar_classify::ENRICHMENT_VERSION,
                content_type: c.content_type.code(),
                junk_score: c.junk.score,
                comp: c.composition.encode(),
                data: lodestar_classify::Record::new(&c).encode(),
            };
            (id, e)
        })
        .collect();
    store.set_enrichments(&enriched).unwrap();
    let mut f = Fixture {
        store,
        ids,
        dht,
        open,
        closed,
        open2,
    };
    f.claims();
    f
}

impl Fixture {
    fn put(&mut self, i: usize, source: SourceId, claims: &Claims, private: bool) {
        assert!(
            self.store
                .put_claims(self.ids[i], source, claims, private)
                .unwrap()
        );
    }

    fn claims(&mut self) {
        let (open, closed, open2) = (self.open, self.closed, self.open2);
        let matrix = answer(
            Some(90),
            &[
                ("imdb", "tt0133093"),
                ("tmdb", "603"),
                ("wikidata", "Q83495"),
            ],
            "The Matrix",
        );
        self.put(0, open, &matrix, false);
        let heat = answer(Some(90), &[("imdb", "tt0113277")], "Heat Private");
        self.put(1, open, &heat, true);
        let alien = answer(Some(90), &[("imdb", "tt0078748")], "Alien Finder");
        self.put(2, open, &alien, false);
        let ronin = answer(Some(90), &[("imdb", "tt0122690")], "Ronin Closed");
        self.put(3, closed, &ronin, false);
        let brazil = answer(Some(90), &[("imdb", "tt0088846")], "Brazil Private");
        self.put(4, open, &brazil, false);
        let fargo = answer(None, &[("imdb", "tt0116282")], "Fargo Feed");
        self.put(5, open, &fargo, false);
        let mut akira = answer(Some(80), &[("anidb", "1")], "Akira");
        let d = akira.details.as_mut().unwrap();
        d.alt_titles = (0..12)
            .map(|n| AltTitle {
                language: if n % 2 == 0 { "fr" } else { "en" }.into(),
                title: format!("Akira Alt {n}"),
            })
            .collect();
        self.put(6, open, &akira, false);
        let seven = answer(Some(70), &[("imdb", "tt0114369")], "Se7en");
        self.put(8, open, &seven, false);
        let seven2 = answer(
            Some(60),
            &[("imdb", "tt0114369"), ("mbid", MBID)],
            "Seven Two",
        );
        self.put(8, open2, &seven2, false);
    }

    fn index(&mut self, root: &Path) -> SearchIndex {
        let index = SearchIndex::open(&index_dir(root)).unwrap();
        let mut w = index.writer(HEAP).unwrap();
        while w.catch_up(&mut self.store, 1000).unwrap() > 0 {}
        w.commit().unwrap();
        w.finish().unwrap();
        index
    }
}

const MBID: &str = "f1e2d3c4-b5a6-4789-8abc-def012345678";

const NONE: [u64; 0] = [];

/// The torrent ids `q` finds, sorted.
fn found(index: &SearchIndex, q: &str) -> Vec<u64> {
    let mut ids: Vec<u64> = index
        .search(&index.query(&parse(q).unwrap(), NOW), 1000)
        .unwrap()
        .hits
        .iter()
        .map(|h| h.id)
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn only_served_answers_are_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut f = fixture(tmp.path());
    let index = f.index(tmp.path());
    let id = |i: usize| f.ids[i] as u64;

    // Served: every id, in any of its spellings, and the titles.
    for q in [
        "imdb:tt0133093",
        "imdb:0133093",
        "imdb:133093",
        "imdb:TT0133093",
        "tmdb:603",
        "tmdb:0603",
        "wikidata:Q83495",
        "wikidata:q83495",
        "title:\"the matrix ja\"",
        "imdb:tt0133093,tt0113277",
    ] {
        assert_eq!(found(&index, q), [id(0)], "{q}");
    }
    // Never served: a private source, a finder's own answer, terms that
    // don't allow it, a private torrent, details that aren't an answer.
    for (i, imdb, title) in [
        (1, "tt0113277", "heat private"),
        (2, "tt0078748", "alien finder"),
        (3, "tt0122690", "ronin closed"),
        (4, "tt0088846", "brazil private"),
        (5, "tt0116282", "fargo feed"),
    ] {
        assert_eq!(found(&index, &format!("imdb:{imdb}")), NONE, "torrent {i}");
        assert_eq!(found(&index, &format!("title:\"{title}\"")), NONE, "{i}");
    }
    // Two sources, one id; one adds a MusicBrainz id.
    assert_eq!(found(&index, "imdb:tt0114369"), [id(8)]);
    assert_eq!(
        found(&index, &format!("mbid:{}", MBID.to_uppercase())),
        [id(8)]
    );
    assert_eq!(found(&index, "title:se7en"), [id(8)]);
    // Negated, the clause keeps the rest.
    assert_eq!(found(&index, "-imdb:tt0133093").len(), NAMES.len() - 1);
    // An id nothing has.
    assert_eq!(found(&index, "imdb:tt9999999"), NONE);
}

/// X-purge-index-shapes: purging a source takes what it answered out of
/// the index at the next pass, through the purge's own `reindex_log`
/// entries. A torrent only it identified serves no ids or titles; one two
/// sources identified keeps the survivor's answer and loses the purged
/// one's. The answer on torrent 0 has no canonical title, the shape the
/// purge used to skip, so its ids stayed served after the purge.
#[test]
fn a_purge_takes_its_answers_out_of_the_index() {
    let tmp = tempfile::tempdir().unwrap();
    let mut f = fixture(tmp.path());
    let (matrix, seven) = (f.ids[0], f.ids[8]);
    let mut c = f.store.torrent_claims(matrix).unwrap()[0]
        .claims
        .clone()
        .unwrap();
    c.details.as_mut().unwrap().canonical_title = None;
    let open = f.open;
    f.put(0, open, &c, false);
    let index = f.index(tmp.path());
    for q in ["imdb:tt0133093", "title:\"the matrix ja\""] {
        assert_eq!(found(&index, q), [matrix as u64], "{q}");
    }
    for q in ["imdb:tt0114369", "title:se7en", "title:\"seven two\""] {
        assert_eq!(found(&index, q), [seven as u64], "{q}");
    }
    drop(index);

    f.store
        .set_source_state(open, lodestar_store::SourceState::Disabled)
        .unwrap();
    let mut cursor = lodestar_store::unverified::PurgeCursor::START;
    while let Some(next) = f.store.purge_source(open, cursor, 2).unwrap().next {
        cursor = next;
    }
    assert!(f.store.served_identifications(matrix).unwrap().is_empty());
    let index = f.index(tmp.path());
    for q in [
        "imdb:tt0133093",
        "tmdb:603",
        "wikidata:Q83495",
        "title:\"the matrix ja\"",
        "title:se7en",
        "title:\"se7en ja\"",
    ] {
        assert_eq!(found(&index, q), NONE, "{q}");
    }
    // The survivor's answer: its shared id, its own id and its titles.
    assert_eq!(found(&index, "imdb:tt0114369"), [seven as u64]);
    assert_eq!(found(&index, &format!("mbid:{MBID}")), [seven as u64]);
    assert_eq!(found(&index, "title:\"seven two\""), [seven as u64]);
    // The record's own name still finds both.
    assert_eq!(found(&index, "matrix"), [matrix as u64]);
}

#[test]
fn at_most_eight_titles_are_indexed_ranked() {
    let tmp = tempfile::tempdir().unwrap();
    let mut f = fixture(tmp.path());
    let index = f.index(tmp.path());
    let akira = f.ids[6] as u64;
    // "Akira" itself is the record's title; the canonical title adds
    // nothing new. English first (odd numbers), in stored order, then
    // French: alts 1, 3, 5, 7, 9, 11, then 0 and 2.
    let indexed: Vec<usize> = (0..12)
        .filter(|n| found(&index, &format!("title:\"akira alt {n}\"")) == [akira])
        .collect();
    assert_eq!(indexed, [0, 1, 2, 3, 5, 7, 9, 11]);
}

#[test]
fn identify_skips_adult_and_unrecorded_documents() {
    let a = Identified {
        source: SourceId::new(3).unwrap(),
        confidence: 90,
        ids: vec![ExternalId {
            scheme: "imdb".into(),
            value: "tt0133093".into(),
        }],
        canonical_title: Some("The Matrix".into()),
        alt_titles: Vec::new(),
    };
    // No record yet: nothing is known, so nothing is added.
    let mut e = DocEnrichment::default();
    e.identify(std::slice::from_ref(&a));
    assert_eq!(e, DocEnrichment::default());
    // Adult, by type or by kind.
    for tag in ["type:adult", "adult:scene"] {
        let mut e = DocEnrichment {
            tags: vec!["type:movie".into(), tag.into()],
            ..DocEnrichment::default()
        };
        let before = e.clone();
        e.identify(std::slice::from_ref(&a));
        assert_eq!(e, before, "{tag}");
    }
    let mut e = DocEnrichment {
        titles: vec!["THE MATRIX".into()],
        tags: vec!["type:movie".into()],
        ..DocEnrichment::default()
    };
    e.identify(std::slice::from_ref(&a));
    // The id is added; the title is already there, whatever its case.
    assert_eq!(e.tags, ["type:movie", "imdb:tt0133093"]);
    assert_eq!(e.titles, ["THE MATRIX"]);
}

#[test]
fn the_sweep_logs_torrents_holding_answers_resumably() {
    let tmp = tempfile::tempdir().unwrap();
    let mut f = fixture(tmp.path());
    let index = f.index(tmp.path());
    let gattaca = f.ids[7];
    // An answer stored with nothing logged, as before ENR-11.
    let claims = answer(Some(90), &[("imdb", "tt0119177")], "Gattaca");
    f.put(7, f.open, &claims, false);
    // And another source's version 1 row on the same torrent.
    let v1 = Claims {
        name: Some("Gattaca".into()),
        ..Claims::default()
    };
    f.put(7, f.dht, &v1, false);
    let mut w = index.writer(HEAP).unwrap();
    while w.catch_up(&mut f.store, 1000).unwrap() > 0 {}
    w.commit().unwrap();
    assert_eq!(found(&index, "imdb:tt0119177"), NONE);

    assert_eq!(
        f.store.identify_sweep_state().unwrap(),
        SweepState::NotStarted
    );
    // Two rows a step, with a restart (a fresh connection) between steps.
    let mut steps = 0;
    let mut logged = 0;
    loop {
        let step = f.store.identify_sweep_step(2).unwrap();
        steps += 1;
        logged += step.logged;
        assert!(step.read <= 2);
        if step.done {
            break;
        }
        assert!(matches!(
            f.store.identify_sweep_state().unwrap(),
            SweepState::Running { .. }
        ));
        drop(std::mem::replace(
            &mut f.store,
            Store::open(tmp.path().join("s.db"), StoreConfig::default()).unwrap(),
        ));
        assert!(steps < 100);
    }
    assert!(f.store.identify_sweep_state().unwrap().is_done());
    // Every torrent holding a version 3 row not through a private source:
    // 0, 2, 3, 4, 5, 6, 7 and 8; torrent 1's is private.
    assert_eq!(logged, 8);
    while w.catch_up(&mut f.store, 1000).unwrap() > 0 {}
    w.commit().unwrap();
    w.finish().unwrap();
    assert_eq!(found(&index, "imdb:tt0119177"), [gattaca as u64]);
    // Done stays done, and logs nothing more.
    let again = f.store.identify_sweep_step(100).unwrap();
    assert!(again.done && again.read == 0 && again.logged == 0);
}

/// A batch that ends partway through the last torrent's rows doesn't end
/// the sweep: the later row is still read and logs the torrent.
#[test]
fn the_sweep_reads_every_row_of_the_last_torrent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::open(tmp.path().join("s.db"), StoreConfig::default()).unwrap();
    let dht = store.register_source("dht", "1", 1).unwrap();
    let open = store.register_source("wikidata", "1", 1).unwrap();
    let raws: Vec<Vec<u8>> = (0..2).map(|i| torrent(i, false)).collect();
    let infos: Vec<Info> = raws.iter().map(|t| Info::parse(t).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .map(|(info, raw)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            extra_flags: 0,
            source: dht,
        })
        .collect();
    let ids: Vec<i64> = store
        .insert(&batch)
        .unwrap()
        .into_iter()
        .map(|i| i.id())
        .collect();
    assert!(dht.get() < open.get());
    let a = answer(Some(90), &[("imdb", "tt0119177")], "Gattaca");
    let v1 = Claims {
        name: Some("Gattaca".into()),
        ..Claims::default()
    };
    assert!(store.put_claims(ids[0], dht, &v1, false).unwrap());
    // The last torrent's first row is through a private source, so only
    // its second row logs it.
    assert!(store.put_claims(ids[1], dht, &a, true).unwrap());
    assert!(store.put_claims(ids[1], open, &a, false).unwrap());
    let mut logged = 0;
    for _ in 0..10 {
        let step = store.identify_sweep_step(1).unwrap();
        logged += step.logged;
        if step.done {
            break;
        }
    }
    assert!(store.identify_sweep_state().unwrap().is_done());
    assert_eq!(logged, 1);
}

/// Every enrichment scheme has its field, and every ID field is a scheme.
#[test]
fn the_id_fields_are_the_enrichment_schemes() {
    let fields: Vec<&str> = TagKey::IDS.iter().map(|k| k.as_str()).collect();
    assert_eq!(fields, lodestar_store::claims::ENRICHMENT_SCHEMES);
    for s in lodestar_store::claims::ENRICHMENT_SCHEMES {
        assert_eq!(TagKey::id(s).map(TagKey::as_str), Some(s));
    }
}

#[test]
fn an_empty_store_sweeps_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::open(tmp.path().join("s.db"), StoreConfig::default()).unwrap();
    let step = store.identify_sweep_step(10).unwrap();
    assert!(step.done && step.read == 0);
    assert!(store.identify_sweep_state().unwrap().is_done());
}

#[test]
fn id_fields_parse_print_and_stay_out_of_filter_rules() {
    let tag = |key, v: &str| Node::Tag {
        key,
        any: vec![v.into()],
    };
    for (q, key, v) in [
        ("imdb:111161", TagKey::Imdb, "tt0111161"),
        ("imdb:tt12345678", TagKey::Imdb, "tt12345678"),
        ("tvdb:081189", TagKey::Tvdb, "81189"),
        ("anilist:21", TagKey::Anilist, "21"),
        ("anidb:1", TagKey::Anidb, "1"),
        ("igdb:1942", TagKey::Igdb, "1942"),
        ("steam:570", TagKey::Steam, "570"),
        ("olid:ol45883w", TagKey::Olid, "ol45883w"),
        ("discogs:R249504", TagKey::Discogs, "r249504"),
        ("wikidata:Q42", TagKey::Wikidata, "q42"),
    ] {
        let node = parse(q).unwrap();
        assert_eq!(node, tag(key, v), "{q}");
        // It prints as it parses.
        assert_eq!(parse(&node.to_query()).unwrap(), node, "{q}");
        assert!(node.mentions_provenance(), "{q}");
    }
    for bad in [
        "imdb:nm0000151",
        "imdb:tt",
        "tmdb:abc",
        "tmdb:0",
        "mbid:not-a-uuid",
        "olid:OL1A",
        "discogs:a12",
        "wikidata:P31",
        "steam:123456789012345678901",
    ] {
        assert!(parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_terms_reader_agrees_with_the_harness() {
    let tmp = tempfile::tempdir().unwrap();
    let f = fixture(tmp.path());
    for s in [f.dht, f.open, f.closed, f.open2] {
        assert_eq!(
            f.store.redistributes(s).unwrap(),
            lodestar_pipeline::identify::may_redistribute(&f.store, s).unwrap(),
        );
    }
    assert!(f.store.redistributes(f.open).unwrap());
    assert!(!f.store.redistributes(f.closed).unwrap());
    assert!(!f.store.redistributes(f.dht).unwrap());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any terms record bytes read the same here as in the harness.
    #[test]
    fn any_record_reads_as_the_harness_reads_it(
        v in prop_oneof![
            prop::collection::vec(any::<u8>(), 0..14),
            // Well-formed records, so the readable branch is reached.
            (any::<u8>(), any::<u64>()).prop_map(|(flags, secs)| {
                let mut v = vec![1, flags];
                v.extend(secs.to_be_bytes());
                v
            }),
            (0u8..8, any::<u64>()).prop_map(|(flags, secs)| {
                let mut v = vec![1, flags];
                v.extend(secs.to_be_bytes());
                v
            }),
        ]
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::open(tmp.path().join("s.db"), StoreConfig::default()).unwrap();
        let s = store.register_source("wikidata", "1", 1).unwrap();
        store.set_plugin_state(s, TERMS_KEY, Some(&v)).unwrap();
        prop_assert_eq!(
            store.redistributes(s).unwrap(),
            lodestar_pipeline::identify::may_redistribute(&store, s).unwrap()
        );
    }

    /// An id value reads to its scheme's stored form, lower-cased, or to
    /// nothing; a stored form reads back to itself.
    #[test]
    fn id_values_are_stored_forms(
        key in prop::sample::select(TagKey::IDS.to_vec()),
        v in "[a-zA-Z0-9-]{0,40}",
    ) {
        if let Some(id) = id_value(key, &v) {
            prop_assert_eq!(id.to_ascii_lowercase(), id.clone());
            prop_assert_eq!(id_value(key, &id), Some(id.clone()));
            let stored = match key {
                TagKey::Olid | TagKey::Wikidata => id.to_ascii_uppercase(),
                _ => id.clone(),
            };
            prop_assert!(lodestar_store::claims::id_ok(key.as_str(), &stored), "{key:?} {id}");
        }
    }
}
