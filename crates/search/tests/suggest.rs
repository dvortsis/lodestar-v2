//! Typeahead and natural phrasing (B7) against a real store: every
//! suggestion kind, char-correct ranges, the visibility rules (Adult,
//! filter-hidden, sealed), the title FST after deletes and a rebuild,
//! and natural phrasing goldens and properties.

use std::time::{Duration, Instant};

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::natural::{Piece, natural};
use lodestar_search::query::TagKey;
use lodestar_search::suggest::{self, Kind, Request, Shown, Suggestion};
use lodestar_search::{CancelToken, LiveConfig, LiveIndexer, Node, SearchIndex, parse};
use lodestar_store::{Filtered, Held, NewTorrent, Refiled, SourceId, Store, StoreConfig, flags};
use proptest::prelude::*;

/// Names in id order, with whether a filter rule hid them.
const TORRENTS: &[(&str, bool)] = &[
    ("The.Dark.Knight.2008.1080p.BluRay.x264-GRP.mkv", false),
    (
        "The.Dark.Knight.2008.2160p.UHD.BluRay.x265-OTHER.mkv",
        false,
    ),
    ("The.Dark.Tower.2017.720p.WEB-DL.x264-GRP.mkv", false),
    // Detected adult.
    ("Darkest.Secret.XXX.1080p.mp4", false),
    // Hidden by a filter rule.
    (
        "Darkly.Hidden.Thing.2020.1080p.WEB-DL.x264-HIDGRP.mkv",
        true,
    ),
    ("Amélie.2001.1080p.BluRay.x264-GRP.mkv", false),
    ("Dark.Matter.S01E01.1080p.WEB.h264-GRP.mkv", false),
    ("Knight.Rider.S01E01.720p.HDTV.x264-OTHER.mkv", false),
];

const NOW: i64 = 1_700_000_100;

fn raw(name: &str) -> Vec<u8> {
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    to_vec(&OwnedValue::Dict(
        [
            ("length", OwnedValue::Int(1 << 30)),
            ("name", b(name)),
            ("piece length", OwnedValue::Int(1 << 16)),
            ("pieces", OwnedValue::Bytes(vec![0; 20])),
        ]
        .into_iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v))
        .collect(),
    ))
}

fn add(store: &mut Store, name: &str, hidden: bool) {
    let raw = raw(name);
    let info = Info::parse(&raw).unwrap();
    store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            extra_flags: if hidden { flags::HIDDEN } else { 0 },
            source: SourceId::new(0).unwrap(),
        }])
        .unwrap();
}

struct Fixture {
    store: Store,
    index: SearchIndex,
    db: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// Enrichment, the index and the title set, as the pipeline, the
    /// live indexer and the title builder would leave them.
    fn sync(&mut self) {
        while lodestar_pipeline::enrich::enrich_stored(&mut self.store, 0, 100)
            .unwrap()
            .is_some()
        {}
        let mut w = self.index.writer(15 << 20).unwrap();
        w.catch_up(&mut self.store, 1000).unwrap();
        w.commit().unwrap();
        self.index.titles().rebuild_from(&self.store).unwrap();
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("s.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    for (name, hidden) in TORRENTS {
        add(&mut store, name, *hidden);
    }
    let mut f = Fixture {
        store,
        index: SearchIndex::in_memory().unwrap(),
        db,
        _dir: dir,
    };
    f.sync();
    f
}

const PLAIN: Shown = Shown {
    adult: false,
    hidden: false,
};

/// `/search`'s base query of a clause (`crates/api/src/search.rs`).
fn restrict(shown: Shown) -> impl Fn(Node) -> Node + Sync {
    move |n: Node| {
        let n = if shown.hidden { n } else { n.without_hidden() };
        if shown.adult {
            n
        } else {
            lodestar_search::with_filters(
                n,
                &[Node::Not {
                    of: Box::new(Node::Tag {
                        key: TagKey::Type,
                        any: vec!["adult".into()],
                    }),
                }],
            )
        }
    }
}

/// Suggestions for `marked`, `|` marking the caret (the end if none).
fn suggest_at(index: &SearchIndex, marked: &str, shown: Shown, limit: usize) -> Vec<Suggestion> {
    let pos = marked.find('|').map(|b| marked[..b].chars().count());
    let q = marked.replacen('|', "", 1);
    let r = restrict(shown);
    let req = Request {
        q: &q,
        pos,
        limit,
        shown,
        restrict: &r,
        now: NOW,
    };
    suggest::suggest(index, &req, &CancelToken::new()).unwrap()
}

fn of_kind(items: &[Suggestion], kind: Kind) -> Vec<(String, Option<u64>)> {
    items
        .iter()
        .filter(|s| s.kind == kind)
        .map(|s| (s.insert.clone(), s.count))
        .collect()
}

/// `q` with an item applied.
fn applied(q: &str, s: &Suggestion) -> String {
    let chars: Vec<char> = q.chars().collect();
    let head: String = chars[..s.replace.0].iter().collect();
    let tail: String = chars[s.replace.1..].iter().collect();
    format!("{head}{}{tail}", s.insert)
}

#[test]
fn field_names_come_from_the_field_table() {
    let f = fixture();
    let got = suggest_at(&f.index, "re", PLAIN, 8);
    assert_eq!(
        of_kind(&got, Kind::Field),
        [("res:".to_string(), None), ("resolved:".to_string(), None)]
    );
    // Swarm health's fields (M4) come from the same table; the shorter
    // name first, and the exact name first.
    let got = suggest_at(&f.index, "see", PLAIN, 8);
    assert_eq!(
        of_kind(&got, Kind::Field),
        [("seen:".to_string(), None), ("seeders:".to_string(), None)]
    );
    let got = suggest_at(&f.index, "s", PLAIN, 8);
    assert_eq!(of_kind(&got, Kind::Field)[0].0, "s:");
    // Inside file:(…), only what a file clause takes.
    let got = suggest_at(&f.index, "file:(e|)", PLAIN, 8);
    assert_eq!(of_kind(&got, Kind::Field), [("ext:".to_string(), None)]);
    // The caret in a field's name completes the name, keeping the value.
    let got = suggest_at(&f.index, "re|s:1080p", PLAIN, 8);
    let s = got.iter().find(|s| s.insert == "res:").unwrap();
    assert_eq!(applied("res:1080p", s), "res:1080p");
    assert_eq!(s.replace, (0, 4));
}

#[test]
fn values_come_from_the_term_dictionaries_with_visible_counts() {
    let f = fixture();
    let got = suggest_at(&f.index, "res:", PLAIN, 8);
    // The adult and the hidden torrents' 1080p aren't counted.
    assert_eq!(
        of_kind(&got, Kind::Value),
        [
            ("res:1080p".to_string(), Some(3)),
            ("res:720p".to_string(), Some(2)),
            ("res:2160p".to_string(), Some(1)),
        ]
    );
    let shown = Shown {
        adult: true,
        hidden: true,
    };
    assert_eq!(
        of_kind(&suggest_at(&f.index, "res:1", shown, 8), Kind::Value),
        [("res:1080p".to_string(), Some(5))]
    );
    // Prefix-matched, negation kept outside the range.
    let got = suggest_at(&f.index, "dark -codec:h2", PLAIN, 8);
    let v: Vec<&Suggestion> = got.iter().filter(|s| s.kind == Kind::Value).collect();
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].insert, "codec:h264");
    assert_eq!(v[0].replace, (6, 14));
    assert_eq!(applied("dark -codec:h2", v[0]), "dark -codec:h264");
    // The last of a list.
    let got = suggest_at(&f.index, "ext:mp4,mk", PLAIN, 8);
    assert_eq!(
        of_kind(&got, Kind::Value),
        [("ext:mp4,mkv".to_string(), Some(6))]
    );
    // Flags.
    let got = suggest_at(&f.index, "private:", PLAIN, 8);
    assert_eq!(
        of_kind(&got, Kind::Value),
        [
            ("private:yes".to_string(), Some(0)),
            ("private:no".to_string(), Some(6)),
        ]
    );
}

#[test]
fn values_only_hidden_torrents_carry_are_never_offered() {
    let f = fixture();
    let groups = |shown| {
        of_kind(&suggest_at(&f.index, "group:", shown, 20), Kind::Value)
            .into_iter()
            .map(|(v, _)| v)
            .collect::<Vec<_>>()
    };
    assert_eq!(groups(PLAIN), ["group:grp", "group:other"]);
    let hidden = Shown {
        adult: false,
        hidden: true,
    };
    assert_eq!(groups(hidden), ["group:grp", "group:other", "group:hidgrp"]);
    // `ext:mp4` is only on the adult torrent.
    assert!(of_kind(&suggest_at(&f.index, "ext:mp", PLAIN, 8), Kind::Value).is_empty());
    let adult = Shown {
        adult: true,
        hidden: false,
    };
    assert_eq!(
        of_kind(&suggest_at(&f.index, "ext:mp", adult, 8), Kind::Value),
        [("ext:mp4".to_string(), Some(1))]
    );
    // `type:` never lists adult while it's hidden, nor `adult:` kinds.
    let types = of_kind(&suggest_at(&f.index, "type:", PLAIN, 20), Kind::Value);
    assert!(types.iter().all(|(v, _)| v != "type:adult"), "{types:?}");
    assert!(of_kind(&suggest_at(&f.index, "adult:", PLAIN, 20), Kind::Value).is_empty());
}

#[test]
fn titles_by_prefix_ranked_and_deduplicated() {
    let f = fixture();
    let got = suggest_at(&f.index, "res:1080p the dark", PLAIN, 8);
    let titles = of_kind(&got, Kind::Title);
    assert_eq!(
        titles,
        [
            ("title:\"The Dark Knight\"".to_string(), Some(2)),
            ("title:\"The Dark Tower\"".to_string(), Some(1)),
        ]
    );
    let s = got.iter().find(|s| s.kind == Kind::Title).unwrap();
    assert_eq!(s.text, "The Dark Knight");
    assert_eq!(s.replace, (10, 18));
    assert_eq!(
        applied("res:1080p the dark", s),
        "res:1080p title:\"The Dark Knight\""
    );
    // The applied text parses.
    assert!(parse(&applied("res:1080p the dark", s)).is_ok());
    // Case-insensitive, diacritics folded, char-correct ranges.
    let got = suggest_at(&f.index, "日本 AMEL", PLAIN, 8);
    let s = got.iter().find(|s| s.kind == Kind::Title).unwrap();
    assert_eq!((s.text.as_str(), s.replace), ("Amélie", (3, 7)));
    let got = suggest_at(&f.index, "amé|lie 2001", PLAIN, 8);
    let s = got.iter().find(|s| s.kind == Kind::Title).unwrap();
    assert_eq!(s.replace, (0, 6));
    // In a quoted phrase, and after `title:`.
    let got = suggest_at(&f.index, "\"the dark k", PLAIN, 8);
    assert_eq!(
        of_kind(&got, Kind::Title),
        [("title:\"The Dark Knight\"".to_string(), Some(2))]
    );
    let got = suggest_at(&f.index, "title:knig", PLAIN, 8);
    assert_eq!(
        of_kind(&got, Kind::Title),
        [("title:\"Knight Rider\"".to_string(), Some(1))]
    );
    // One letter isn't enough, and the limit holds.
    assert!(of_kind(&suggest_at(&f.index, "d", PLAIN, 8), Kind::Title).is_empty());
    assert_eq!(suggest_at(&f.index, "the dark", PLAIN, 1).len(), 1);
}

#[test]
fn adult_and_filter_hidden_titles_never_leak() {
    let f = fixture();
    let titles = |q: &str, shown| -> Vec<String> {
        suggest_at(&f.index, q, shown, 20)
            .into_iter()
            .filter(|s| s.kind == Kind::Title)
            .map(|s| s.text)
            .collect()
    };
    assert_eq!(titles("dark", PLAIN), ["Dark Matter"]);
    let adult = Shown {
        adult: true,
        hidden: false,
    };
    assert_eq!(titles("dark", adult), ["Dark Matter", "Darkest Secret XXX"]);
    let hidden = Shown {
        adult: false,
        hidden: true,
    };
    assert_eq!(
        titles("dark", hidden),
        ["Dark Matter", "Darkly Hidden Thing"]
    );
    // Nor through "did you mean": the adult title's word isn't offered.
    let got = suggest_at(&f.index, "darkets ", PLAIN, 20);
    assert!(of_kind(&got, Kind::DidYouMean).is_empty(), "{got:?}");
    let got = suggest_at(&f.index, "darkets ", adult, 20);
    assert_eq!(of_kind(&got, Kind::DidYouMean)[0].0, "darkest");
}

#[test]
fn smart_chips_reuse_the_recognisers() {
    let f = fixture();
    let chip = |q: &str| of_kind(&suggest_at(&f.index, q, PLAIN, 8), Kind::Chip);
    assert_eq!(chip("dark 2160p"), [("res:2160p".to_string(), Some(1))]);
    assert_eq!(chip("x265"), [("codec:h265".to_string(), Some(1))]);
    assert_eq!(chip("4k"), [("res:2160p".to_string(), Some(1))]);
    assert_eq!(chip("mkv"), [("ext:mkv".to_string(), Some(6))]);
    assert_eq!(chip("2008"), [("year:2008".to_string(), Some(2))]);
    assert_eq!(chip("movie"), [("type:movie".to_string(), Some(4))]);
    assert!(chip("knight").is_empty());
    // Only a token that is the value alone: a release name isn't a chip.
    for name in [
        "Naruto.S01E02.1080p",
        "naruto.1080p",
        "x265,naruto",
        "flac/naruto",
        "naruto\u{3000}1080p",
        "4k\u{a0}movies",
    ] {
        assert!(chip(name).is_empty(), "{name}");
    }
    assert_eq!(chip("h.264"), [("codec:h264".to_string(), Some(5))]);
    // Nor two values in one token, nor a bare number.
    for word in ["ita.eng", "x264.x265", "flac.mp3", "720", "300", "007"] {
        assert!(chip(word).is_empty(), "{word}");
    }
    // Negated: the `-` stays.
    let got = suggest_at(&f.index, "-x265", PLAIN, 8);
    let s = got.iter().find(|s| s.kind == Kind::Chip).unwrap();
    assert_eq!(applied("-x265", s), "-codec:h265");
}

#[test]
fn did_you_mean_for_fields_and_rare_words() {
    let f = fixture();
    // An unknown field one edit away (the parser's rule).
    let got = suggest_at(&f.index, "dark rez:1080p", PLAIN, 8);
    let s = &got[0];
    assert_eq!(s.kind, Kind::DidYouMean);
    assert_eq!(
        (s.insert.as_str(), s.text.as_str()),
        ("res:1080p", "dark res:1080p")
    );
    assert_eq!(s.replace, (5, 14));
    // A misspelling in a query that finds nothing, once it's typed.
    let got = suggest_at(&f.index, "dark knigth ", PLAIN, 8);
    let s = got.iter().find(|s| s.kind == Kind::DidYouMean).unwrap();
    assert_eq!(
        (s.insert.as_str(), s.text.as_str()),
        ("knight", "dark knight ")
    );
    assert_eq!(s.replace, (5, 11));
    // Not the word still being typed (the caret at its end).
    let got = suggest_at(&f.index, "dark knigth", PLAIN, 8);
    assert!(of_kind(&got, Kind::DidYouMean).is_empty(), "{got:?}");
    // Nor the word being typed, while it starts a title.
    let got = suggest_at(&f.index, "the dark kni", PLAIN, 8);
    assert!(of_kind(&got, Kind::DidYouMean).is_empty(), "{got:?}");
    assert_eq!(
        of_kind(&got, Kind::Title),
        [("title:\"The Dark Knight\"".to_string(), Some(2))]
    );
    // Only the words nearest the caret are looked at (a fuzzy walk of
    // every dictionary each).
    let far = "knigth wwww xxxx zzzz";
    let got = suggest_at(&f.index, far, PLAIN, 8);
    assert!(of_kind(&got, Kind::DidYouMean).is_empty(), "{got:?}");
    let got = suggest_at(&f.index, &format!("knigth |{}", &far[7..]), PLAIN, 8);
    assert_eq!(of_kind(&got, Kind::DidYouMean)[0].0, "knight");
    // A query with enough hits gets none.
    let got = suggest_at(&f.index, "1080p", PLAIN, 8);
    assert!(of_kind(&got, Kind::DidYouMean).is_empty());
    // `Re:Zero` is text, not a field.
    let got = suggest_at(&f.index, "Re:Zero", PLAIN, 8);
    assert!(got.iter().all(|s| !s.insert.contains("res:")), "{got:?}");
}

#[test]
fn empty_input_limits_and_unicode() {
    let f = fixture();
    assert!(suggest_at(&f.index, "", PLAIN, 8).is_empty());
    assert!(suggest_at(&f.index, "   ", PLAIN, 8).is_empty());
    assert!(suggest_at(&f.index, "dark |", PLAIN, 8).is_empty());
    let long = "a".repeat(5000);
    assert!(suggest_at(&f.index, &long, PLAIN, 8).is_empty());
    // A caret past the end means the end.
    let r = restrict(PLAIN);
    let req = Request {
        q: "re",
        pos: Some(99),
        limit: 99,
        shown: PLAIN,
        restrict: &r,
        now: NOW,
    };
    let got = suggest::suggest(&f.index, &req, &CancelToken::new()).unwrap();
    assert_eq!(got.len(), 2);
    // Every range is inside the query, in chars, for odd input.
    for q in [
        "日本語 ﾃｽﾄ",
        "\"ünï",
        "(ﬁle:(ext:",
        "-!-",
        "a|b|c",
        "Ωmega:x",
    ] {
        let n = q.chars().count();
        for s in suggest_at(&f.index, q, PLAIN, 20) {
            assert!(s.replace.0 <= s.replace.1 && s.replace.1 <= n, "{q} {s:?}");
        }
    }
}

#[test]
fn a_cancelled_request_stops() {
    let f = fixture();
    let r = restrict(PLAIN);
    let req = Request {
        q: "dark knigth",
        pos: None,
        limit: 8,
        shown: PLAIN,
        restrict: &r,
        now: NOW,
    };
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(suggest::suggest(&f.index, &req, &cancel).is_err());
}

/// Quarantines stored torrent `id` with a sealed name, as filtering does
/// when it holds one (§20.3).
fn quarantine(store: &mut Store, id: i64) {
    let row = store.torrent(id).unwrap().unwrap();
    let hashes = [row.info_hash.0];
    let generation = store.class_gen().unwrap();
    store
        .set_enrichments_at(
            generation,
            &[],
            &[Refiled {
                id,
                filtered: Filtered {
                    hashes: &hashes,
                    rule: "CS-test",
                    action: 0,
                    held: Some(Held::Quarantine),
                    sealed: Some(b"sealed"),
                    evidence: &[],
                    at: NOW,
                    expires_at: NOW + 86_400,
                    adult_hold: false,
                },
                flags: 0,
            }],
        )
        .unwrap()
        .unwrap();
}

#[test]
fn purged_and_quarantined_titles_disappear() {
    let mut f = fixture();
    let titles = |index: &SearchIndex, q: &str| -> Vec<String> {
        suggest_at(index, q, PLAIN, 20)
            .into_iter()
            .filter(|s| s.kind == Kind::Title)
            .map(|s| s.text)
            .collect()
    };
    assert_eq!(titles(&f.index, "knight"), ["Knight Rider"]);
    assert_eq!(titles(&f.index, "dark mat"), ["Dark Matter"]);
    // Knight Rider is deleted, Dark Matter quarantined (its name sealed).
    let rider = f.store.torrent(8).unwrap().unwrap().info_hash.0;
    f.store.delete_torrents(&[rider]).unwrap();
    quarantine(&mut f.store, 7);
    assert_eq!(f.store.quarantined(10).unwrap().len(), 1);
    // Out of the index at its next commit: no longer offered, though the
    // title set is a build behind.
    let mut w = f.index.writer(15 << 20).unwrap();
    w.catch_up(&mut f.store, 1000).unwrap();
    w.commit().unwrap();
    let before = f.index.titles().current().unwrap();
    assert_eq!(before.lookup("knight", PLAIN, 5).len(), 1);
    assert!(titles(&f.index, "knight").is_empty());
    assert!(titles(&f.index, "dark mat").is_empty());
    // And out of the next build.
    f.index.titles().rebuild_from(&f.store).unwrap();
    let after = f.index.titles().current().unwrap();
    assert!(after.lookup("knight", PLAIN, 5).is_empty());
    assert!(after.lookup("dark mat", PLAIN, 5).is_empty());
    assert_eq!(after.len() + 2, before.len());
}

#[test]
fn the_live_indexer_rebuilds_titles_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("s.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    add(&mut store, "First.Title.2001.1080p.mkv", false);
    lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100).unwrap();
    // A directory index, as a rebuild's new one is.
    let index = SearchIndex::open(&dir.path().join("index")).unwrap();
    index.titles().set_interval(Duration::from_millis(20));
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let config = LiveConfig {
        heap_bytes: 15 << 20,
        poll: Duration::from_millis(10),
        commit_every: Duration::from_millis(20),
        ..LiveConfig::default()
    };
    let live = LiveIndexer::spawn(index.clone(), reader, config).unwrap();
    let wait = |what: &str, ok: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !ok() {
            assert!(start.elapsed() < Duration::from_secs(30), "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let has = |p: &str| {
        index
            .titles()
            .current()
            .is_some_and(|s| !s.lookup(p, PLAIN, 1).is_empty())
    };
    wait("first build", &|| has("first"));
    add(&mut store, "Second.Title.2002.1080p.mkv", false);
    lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100).unwrap();
    wait("rebuild after a commit", &|| has("second"));
    // A deleted torrent's title leaves within one cycle.
    let first = store.torrent(1).unwrap().unwrap().info_hash.0;
    store.delete_torrents(&[first]).unwrap();
    wait("rebuild after a delete", &|| !has("first"));
    live.stop().unwrap();
    // The fixture's own path: nothing else to check.
    let _ = db;
}

/// Through the real rebuild and swap (B5b): the index serving during a
/// rebuild has its own title set, following the store, and the new
/// directory gets a fresh one at the swap.
#[test]
fn a_rebuild_swap_builds_titles_for_the_new_directory() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use lodestar_search::{IndexVersion, Serving, ServingConfig};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("index");
    let db = dir.path().join("s.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    add(&mut store, "First.Title.2001.1080p.mkv", false);
    lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100).unwrap();
    // An index of this build's version, which a build with a newer
    // tokenizer serves while it rebuilds.
    let old = IndexVersion::CURRENT;
    {
        let index = SearchIndex::open(&root.join(old.dir_name())).unwrap();
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 1000).unwrap();
        w.commit().unwrap();
        w.finish().unwrap();
    }
    let hold = Arc::new(AtomicBool::new(true));
    let config = ServingConfig {
        live: LiveConfig {
            heap_bytes: 15 << 20,
            poll: Duration::from_millis(10),
            commit_every: Duration::from_millis(20),
            ..LiveConfig::default()
        },
        rebuild_commit_docs: 5,
        version: IndexVersion {
            tokenizer: old.tokenizer + 1,
            ..old
        },
        hold_before_finish: Some(hold.clone()),
        ..ServingConfig::default()
    };
    let serving = Serving::open_or_rebuild(&root, &db, config).unwrap();
    assert!(serving.rebuilding());
    let wait = |what: &str, ok: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !ok() {
            assert!(start.elapsed() < Duration::from_secs(30), "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let titled = |index: &SearchIndex| {
        of_kind(&suggest_at(index, "first ti", PLAIN, 8), Kind::Title)
            == [("title:\"First Title\"".to_string(), Some(1))]
    };
    // The old directory serves, with titles.
    let before = serving.index();
    wait("titles while rebuilding", &|| titled(&before));
    hold.store(false, Ordering::Relaxed);
    wait("the swap", &|| !serving.rebuilding());
    // The new directory's index has a set of its own, built once its
    // live indexer starts.
    let after = serving.index();
    assert!(!Arc::ptr_eq(before.titles(), after.titles()));
    wait("titles after the swap", &|| titled(&after));
    drop(before);
    serving.shutdown().unwrap();
}

#[test]
fn a_new_index_directory_gets_its_own_title_set() {
    let f = fixture();
    assert!(f.index.titles().current().is_some());
    // What a rebuild/swap serves next: another index, built from the
    // same store, with a set of its own once its live indexer runs.
    let next = SearchIndex::in_memory().unwrap();
    assert!(next.titles().current().is_none());
    assert!(of_kind(&suggest_at(&next, "the dark", PLAIN, 8), Kind::Title).is_empty());
    let mut store = Store::open(&f.db, StoreConfig::default()).unwrap();
    let mut w = next.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 1000).unwrap();
    w.commit().unwrap();
    next.titles().rebuild_from(&store).unwrap();
    assert_eq!(
        of_kind(&suggest_at(&next, "the dark", PLAIN, 8), Kind::Title).len(),
        2
    );
}

// ---- natural phrasing -------------------------------------------------------

/// Phrasing → the canonical query proposed.
const NATURAL: &[(&str, &str)] = &[
    (
        "4k movies from 2020 over 10gb",
        "res:2160p type:movie year:2020 size:>=10GB",
    ),
    ("1080p tv shows", "res:1080p type:tv"),
    ("the dark knight in 4k", "the dark knight res:2160p"),
    ("movies and tv shows", "type:movie,tv"),
    (
        "flac albums from the 90s",
        "ext:flac type:music year:1990..1999",
    ),
    (
        "x265 movies between 2000 and 2010",
        "codec:h265 type:movie year:2000..2010",
    ),
    ("french movies", "lang:fr type:movie"),
    (
        "english audiobooks under 500mb",
        "lang:en type:audiobook size:<=500MB",
    ),
    ("ebooks added this week", "type:ebook discovered:<7d"),
    ("software under 1 gb", "type:software size:<=1GB"),
    ("games over 50 gb", "type:game size:>=50GB"),
    (
        "4k hdr dolby vision movies",
        "res:2160p hdr:hdr,dv type:movie",
    ),
    ("season 2 of the office", "s:2 the office"),
    ("breaking bad season 1 episode 5", "breaking bad s:1 e:5"),
    ("movies without subtitles", "type:movie -has:subs"),
    ("movies with subtitles", "type:movie has:subs"),
    ("mkv files over 4gb", "ext:mkv size:>=4GB"),
    ("linux iso", "linux ext:iso"),
    ("star wars 1977", "star wars year:1977"),
    ("movies after 2015", "type:movie year:>=2016"),
    ("movies before 1980", "type:movie year:<=1979"),
    ("movies since 2020", "type:movie year:>=2020"),
    ("from 2000 to 2005 movies", "year:2000..2005 type:movie"),
    ("1999-2001 films", "year:1999..2001 type:movie"),
    ("more than 10 files", "files:>=11"),
    ("at least 100 files", "files:>=101"),
    ("between 1 and 4 gb", "size:1GB..4GB"),
    ("between 1gb and 4gb", "size:1GB..4GB"),
    ("last 3 days", "discovered:<3d"),
    ("today 1080p", "discovered:<1d res:1080p"),
    ("recent anime", "discovered:<7d anime"),
    ("find 4k movies", "res:2160p type:movie"),
    ("show me hevc films", "codec:h265 type:movie"),
    ("lord of the rings", "lord of the rings"),
    ("Re:Zero season 2", "Re:Zero s:2"),
    // Explicit syntax stays as written: negations, fields, lists,
    // phrases, operators and groups mean what they meant.
    ("-cam 4k", "-cam res:2160p"),
    ("-x265 movies", "-x265 type:movie"),
    ("!cam", "-cam"),
    ("movies -type:tv", "movies -type:tv"),
    ("res:1080p movies", "res:1080p type:movie"),
    ("ext:mkv,avi", "ext:mkv,avi"),
    ("a OR b", "a | b"),
    (
        "4k movies OR 1080p tv shows",
        "res:2160p type:movie | res:1080p type:tv",
    ),
    ("\"dark knight\" 4k", "\"dark knight\" res:2160p"),
    ("dark NEAR knight 4k", "dark NEAR/5 knight res:2160p"),
    ("(4k | 1080p) movies", "(4k | 1080p) type:movie"),
    ("naruto (2002)", "naruto 2002"),
    // A token with more in it than one value isn't a chip: a pasted
    // release name keeps all its text.
    ("Naruto.S01E02.1080p", "Naruto.S01E02.1080p"),
    ("the.matrix.1999.1080p", "the.matrix.1999.1080p"),
    ("naruto.1080p", "naruto.1080p"),
    ("x265,naruto", "x265,naruto"),
    ("flac/naruto", "flac/naruto"),
    ("naruto\u{3000}1080p", "naruto\u{3000}1080p"),
    ("進撃の巨人\u{3000}4k", "進撃の巨人\u{3000}4k"),
    ("4k\u{a0}movies", "4k\u{a0}movies"),
    ("dd5.1 h.264", "audio:ac3 codec:h264"),
    // A field the query names itself is left alone; values of one field
    // merge across the AND group.
    ("-type:tv tv shows", "-type:tv tv shows"),
    ("movies -type:movie", "movies -type:movie"),
    ("-ext:mkv mkv files", "-ext:mkv mkv files"),
    ("res:1080p 4k", "res:1080p 4k"),
    ("movies -x tv shows", "type:movie,tv -x"),
    ("x ! y", "x ! y"),
    // Two values in one token aren't one value.
    ("ita.eng", "ita.eng"),
    ("french.german", "french.german"),
    ("french-german", "french-german"),
    ("eng.fre", "eng.fre"),
    ("hdr.dv", "hdr.dv"),
    ("aac.dts", "aac.dts"),
    ("flac.mp3", "flac.mp3"),
    ("x264.x265", "x264.x265"),
    ("dts-hd.ma", "audio:dts-hd-ma"),
    // Numbers aren't extensions.
    ("300 movies", "300 type:movie"),
    ("the 100", "the 100"),
    ("007 movies", "007 type:movie"),
    // An explicit AND means all of them: nothing merges across it.
    ("movies AND tv shows", "type:movie type:tv"),
    ("4k AND 1080p", "res:2160p res:1080p"),
    ("movies -x AND tv shows", "type:movie -x type:tv"),
    ("4k AND x265 AND 1080p", "res:2160p codec:h265 res:1080p"),
    // A field the group names in brackets counts too.
    ("-(type:tv) tv shows", "-type:tv tv shows"),
    ("(-type:tv) tv shows", "-type:tv tv shows"),
    ("-(ext:mkv) mkv files", "-ext:mkv mkv files"),
    ("4k -(res:1080p) 1080p", "4k -res:1080p 1080p"),
    ("x OR (type:tv) tv shows", "x | type:tv tv shows"),
    ("Naruto[1080p]", "Naruto[1080p]"),
    ("naruto [2002]", "naruto year:2002"),
    (
        "720p x264 tv series episodes 1 to 10",
        "res:720p codec:h264 type:tv e:1..10",
    ),
    ("no x265", "-codec:h265"),
    ("german dubbed movies", "lang:de type:movie"),
    (
        "movies under 700mb in 720p",
        "type:movie size:<=700MB res:720p",
    ),
    ("a b c", "a b c"),
    ("", ""),
];

#[test]
fn natural_phrasing_goldens() {
    let mut wrong = Vec::new();
    for (phrase, want) in NATURAL {
        let got = natural(phrase).unwrap();
        if got.canonical != *want {
            wrong.push(format!(
                "{phrase:?}: got {:?}, want {want:?}",
                got.canonical
            ));
        }
        assert_eq!(parse(&got.canonical).unwrap(), got.ast, "{phrase}");
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(NATURAL.len() >= 30);
}

#[test]
fn natural_phrasing_chips_point_at_their_words() {
    let p = "Amélie 4k films from 2001";
    let n = natural(p).unwrap();
    let chars: Vec<char> = p.chars().collect();
    let said: Vec<(String, String)> = n
        .chips
        .iter()
        .map(|c| (c.text.clone(), chars[c.from.0..c.from.1].iter().collect()))
        .collect();
    assert_eq!(
        said,
        [
            ("res:2160p".to_string(), "4k".to_string()),
            ("type:movie".to_string(), "films".to_string()),
            ("year:2001".to_string(), "2001".to_string()),
        ]
    );
    assert_eq!(n.text, ["Amélie"]);
}

#[test]
fn natural_phrasing_reads_only_what_the_parser_reads() {
    use lodestar_search::natural::NaturalError;
    for q in ["OR AND NEAR", "a (b", "\"open", "rez:1080p 4k"] {
        assert_eq!(natural(q), Err(NaturalError::Syntax), "{q}");
    }
}

/// Tokens in query syntax: what natural phrasing must keep.
const SYNTAX: &[&str] = &[
    "-x265",
    "!cam",
    "-cam",
    "res:1080p",
    "-type:tv",
    "ext:mkv,avi",
    "\"dark knight\"",
    "(a | b)",
    "(4k | movies)",
    "title:x",
    "size:>1GB",
    "~knigth",
    "comput*",
    "-\"cam rip\"",
    "-(4k | x)",
    "OR",
    "|",
    "AND",
    "NEAR",
    "NEAR/3",
    "year:2001",
    "-ext:mkv",
];

/// `n` with each placeholder word `zzrun{i}` replaced by `run(i)`
/// (`None`: dropped).
fn substitute(n: &Node, run: &dyn Fn(usize) -> Option<Node>) -> Option<Node> {
    Some(match n {
        Node::Word { word, .. } if word.starts_with("zzrun") => {
            return run(word[5..].parse().unwrap());
        }
        Node::And { of } => Node::And {
            of: of.iter().filter_map(|c| substitute(c, run)).collect(),
        },
        Node::Or { of } => Node::Or {
            of: of.iter().filter_map(|c| substitute(c, run)).collect(),
        },
        Node::Not { of } => Node::Not {
            of: Box::new(substitute(of, run)?),
        },
        n => n.clone(),
    })
}

/// Nested ANDs (and ORs) spliced into their parents, one-clause ones
/// unwrapped, empty ones dropped.
fn flat(n: Option<Node>) -> Option<Node> {
    fn splice(of: Vec<Node>, and: bool) -> Vec<Node> {
        let mut out = Vec::new();
        for c in of.into_iter().filter_map(|c| flat(Some(c))) {
            match c {
                Node::And { of } if and => out.extend(of),
                Node::Or { of } if !and => out.extend(of),
                Node::All => {}
                c => out.push(c),
            }
        }
        out
    }
    let wrap = |mut of: Vec<Node>, and: bool| match of.len() {
        0 => None,
        1 => of.pop(),
        _ => Some(if and {
            Node::And { of }
        } else {
            Node::Or { of }
        }),
    };
    match n? {
        Node::And { of } => wrap(splice(of, true), true),
        Node::Or { of } => wrap(splice(of, false), false),
        Node::Not { of } => Some(Node::Not {
            of: Box::new(flat(Some(*of))?),
        }),
        Node::All => None,
        n => Some(n),
    }
}

/// Words the rules know, and others.
fn vocabulary() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec![
            "4k",
            "1080p",
            "movies",
            "tv",
            "shows",
            "from",
            "2020",
            "over",
            "under",
            "10gb",
            "5",
            "gb",
            "between",
            "and",
            "to",
            "1999",
            "after",
            "before",
            "season",
            "episode",
            "last",
            "days",
            "this",
            "week",
            "without",
            "subtitles",
            "flac",
            "mkv",
            "french",
            "x265",
            "dolby",
            "vision",
            "more",
            "than",
            "files",
            "the",
            "80s",
            "-",
            "OR",
            "NEAR",
            "(",
            ")",
            "\"",
            "res:",
            "~ab",
            "a*",
            "|",
            "*",
            "!x",
            ":",
            "title:x",
            "e:5",
            "…",
        ])
        .prop_map(str::to_owned),
        "[a-zA-Z0-9äöü日本\\-:.*~!|()\"']{1,8}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// The proposal always parses, to its own AST, and renders back to
    /// its canonical text.
    #[test]
    fn natural_output_round_trips(words in prop::collection::vec(vocabulary(), 0..12)) {
        let phrase = words.join(" ");
        let Ok(n) = natural(&phrase) else {
            prop_assert!(parse(&phrase).is_err(), "{}", phrase);
            return Ok(());
        };
        let parsed = parse(&n.canonical).unwrap();
        prop_assert_eq!(&parsed, &n.ast);
        prop_assert_eq!(parsed.to_query(), n.canonical.clone());
        for c in &n.chips {
            prop_assert!(parse(&c.text).is_ok(), "{}", c.text);
            prop_assert!(c.from.0 < c.from.1 && c.from.1 <= phrase.chars().count());
        }
    }

    /// Natural phrasing changes only what its plain runs say, never the
    /// query's structure: the proposal is the strict parse with each run
    /// replaced by its reading (nested ANDs flattened), and that skeleton
    /// is the strict parse's.
    #[test]
    fn natural_keeps_explicit_syntax(
        parts in prop::collection::vec(
            prop_oneof![
                prop::sample::select(SYNTAX.to_vec()).prop_map(str::to_owned),
                vocabulary(),
            ],
            1..9,
        ),
    ) {
        let phrase = parts.join(" ");
        let Ok(n) = natural(&phrase) else {
            // Not a query: `/parse` answers it with the syntax error.
            prop_assert!(parse(&phrase).is_err(), "{}", phrase);
            return Ok(());
        };
        let strict = parse(&phrase).unwrap();
        let pieces = lodestar_search::natural::pieces(&phrase).unwrap();
        let ph = |i: usize| format!("zzrun{i}");
        let Ok(skeleton) = parse(&lodestar_search::natural::join(&pieces, Some(&ph))) else {
            return Err(TestCaseError::fail(format!("no skeleton for {phrase}")));
        };
        let runs: Vec<(String, String)> = pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Run { text, read } => Some((text.clone(), read.clone())),
                Piece::Syntax(_) => None,
            })
            .collect();
        // The skeleton with the runs as written is the strict parse.
        let as_written = substitute(&skeleton, &|i| parse(&runs[i].0).ok());
        prop_assert_eq!(flat(as_written), flat(Some(strict.clone())), "{}", phrase);
        // With the runs as read, it's the proposal (unless it fell back
        // to the query as typed).
        if lodestar_search::query::parse(&lodestar_search::natural::join(&pieces, None)).is_ok() {
            let as_read = substitute(&skeleton, &|i| {
                (!runs[i].1.is_empty()).then(|| parse(&runs[i].1).unwrap())
            });
            prop_assert_eq!(flat(as_read), flat(Some(n.ast.clone())), "{}", phrase);
        } else {
            prop_assert_eq!(&n.ast, &strict);
        }
    }

    /// No plain word is lost: each is kept as text, in a chip's range,
    /// or a linking word the rules may drop.
    #[test]
    fn natural_loses_no_word(
        words in prop::collection::vec(
            prop_oneof![
                prop::sample::select(vec![
                    "4k", "1080p", "movies", "tv", "shows", "from", "2020", "over", "10gb",
                    "x265", "flac", "mkv", "french", "season", "2", "naruto.1080p",
                    "Naruto.S01E02.1080p", "x265,naruto", "flac/naruto", "4k\u{a0}movies",
                    "naruto\u{3000}1080p", "the", "and", "!", "dolby", "vision", "dd5.1",
                    "ita.eng", "x264.x265", "300", "720", "h.264", "hevc", "1080i",
                ])
                .prop_map(str::to_owned),
                "[a-z0-9.,/]{1,10}",
            ],
            1..10,
        ),
    ) {
        let phrase = words.join(" ");
        let Ok(n) = natural(&phrase) else {
            // `/x` is regex syntax: not a query yet.
            prop_assert!(parse(&phrase).is_err(), "{}", phrase);
            return Ok(());
        };
        let mut at = 0;
        for w in &words {
            let start = at;
            at += w.chars().count() + 1;
            let end = start + w.chars().count();
            let in_chip = n.read.iter().any(|r| r.0 < end && start < r.1);
            let trimmed = w.trim_matches(|c: char| "[]{},.;:!?'".contains(c));
            let kept = n.text.iter().any(|t| t == w || t == trimmed);
            prop_assert!(
                kept || in_chip || lodestar_search::natural::is_filler(trimmed),
                "{:?} lost from {:?}: {}", w, phrase, n.canonical
            );
            // A word read alone into a keyword chip is that value and
            // nothing more: the release parser reads it as exactly that.
            if let Some(c) = n.chips.iter().find(|c| c.from == (start, end)) {
                let (field, values) = c.text.split_once(':').unwrap();
                let values: Vec<&str> = values.split(',').collect();
                if field == "ext" {
                    prop_assert!(
                        trimmed.bytes().any(|b| b.is_ascii_alphabetic()),
                        "{:?} read as an extension", w
                    );
                }
                if ["res", "codec", "hdr", "audio", "lang"].contains(&field) {
                    let fields = |x: &str| {
                        let mut f = lodestar_release_parser::parse(&format!("A [{x}]")).fields();
                        f.retain(|(k, _)| !matches!(*k, "title" | "channels"));
                        f
                    };
                    // One field, one value, and it's the chip's.
                    let mine = fields(trimmed);
                    prop_assert!(
                        mine.len() == 1 && values.contains(&mine[0].1.to_lowercase().as_str()),
                        "{:?} read as {} ({:?})", w, c.text, mine
                    );
                }
            }
        }
    }

    /// Any input, any caret: suggestions stay inside the query, in
    /// chars, and applying one gives text without a broken char.
    #[test]
    fn suggest_never_panics_and_ranges_fit(
        q in "[a-z0-9 :\"()|\\-!*~,.äé日]{0,24}",
        pos in 0usize..30,
    ) {
        let index = fuzz_index();
        let r = restrict(PLAIN);
        let req = Request { q: &q, pos: Some(pos), limit: 20, shown: PLAIN, restrict: &r, now: NOW };
        let got = suggest::suggest(index, &req, &CancelToken::new()).unwrap();
        let n = q.chars().count();
        for s in &got {
            prop_assert!(s.replace.0 <= s.replace.1 && s.replace.1 <= n);
            let _ = applied(&q, s);
        }
    }
}

/// One fixture for the property test's cases.
fn fuzz_index() -> &'static SearchIndex {
    static INDEX: std::sync::OnceLock<(SearchIndex, tempfile::TempDir)> =
        std::sync::OnceLock::new();
    let (index, _) = INDEX.get_or_init(|| {
        let f = fixture();
        (f.index, f._dir)
    });
    index
}
