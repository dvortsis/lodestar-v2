//! The PreDB parser (ENR-predb): the shape it reads, its caps, and that
//! nothing it's handed (garbage, cut-short, oversize or mutated answers)
//! panics or keeps more than the caps allow. The corpus is the fuzz
//! target's seeds, `fuzz/seeds/predb/`.

use lodestar_metainfo::hidden_char;
use lodestar_sources_feeds::predb::{
    Limits, MAX_GENRE, MAX_PRETIME, MAX_REASON, MIN_PRETIME, Nuke, SEARCH_LIMITS, Search, Stop,
    parse_search, release_name, section,
};
use proptest::prelude::*;

const SEEDS: &[(&str, &[u8])] = &[
    (
        "search.json",
        include_bytes!("../../../fuzz/seeds/predb/search.json"),
    ),
    (
        "candidates.json",
        include_bytes!("../../../fuzz/seeds/predb/candidates.json"),
    ),
    (
        "empty.json",
        include_bytes!("../../../fuzz/seeds/predb/empty.json"),
    ),
    (
        "error.json",
        include_bytes!("../../../fuzz/seeds/predb/error.json"),
    ),
];

fn seed(name: &str) -> &'static [u8] {
    SEEDS.iter().find(|(n, _)| *n == name).unwrap().1
}

fn parse(body: &str) -> Search {
    parse_search(body.as_bytes(), &SEARCH_LIMITS)
}

/// An answer of one record with `fields` after its name.
fn one(fields: &str) -> String {
    format!(r#"{{"status":"success","data":[{{"release":"A.Film.2020-GRP"{fields}}}]}}"#)
}

#[test]
fn the_seed_answers_read_whole() {
    let s = parse_search(seed("search.json"), &SEARCH_LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.releases.len(), 4);
    let r = &s.releases[1];
    assert_eq!(r.name, "Quiet.Harbor.2021.1080p.WEB.H264-SYNTH");
    assert_eq!(r.pretime, Some(1_700_000_200));
    assert_eq!(r.section.as_deref(), Some("X264-HD"));
    assert_eq!(r.nuke, Nuke::Nuked);
    assert_eq!(r.reason.as_deref(), Some("mislabeled.source"));
    assert_eq!(r.genre.as_deref(), Some("Drama"));
    assert_eq!(s.releases[2].nuke, Nuke::Unnuked);
    assert_eq!(s.releases[3].nuke, Nuke::Deleted);
    // A clean release keeps no reason; an empty genre is none.
    assert_eq!(
        (s.releases[0].nuke, &s.releases[0].reason),
        (Nuke::Clean, &None)
    );
    assert_eq!(s.releases[2].genre, None);
    // The group, links and NFO are never read.
    assert!(!format!("{s:?}").contains("SENTINEL"));

    let e = parse_search(seed("empty.json"), &SEARCH_LIMITS);
    assert!(e.page.whole() && e.releases.is_empty(), "{:?}", e.page);
    let x = parse_search(seed("error.json"), &SEARCH_LIMITS);
    assert!(x.page.error && !x.page.whole());
    assert!(!format!("{x:?}").contains("SENTINEL"));
    let c = parse_search(seed("candidates.json"), &SEARCH_LIMITS);
    assert!(c.page.whole() && c.releases.len() == 3);
}

#[test]
fn only_the_asked_shape_is_whole() {
    for body in [
        "",
        "[]",
        "null",
        r#"{"data":[]}"#,
        r#"{"status":"maybe","data":[]}"#,
        r#"{"status":1,"data":[]}"#,
        r#"{"status":"success","data":{}}"#,
        r#"{"status":"success","status":"success"}"#,
        r#"{"status":"success","data":[],"data":[]}"#,
        r#"{"st\u0061tus":"success"}"#,
        r#"{"status":"success"} x"#,
        r#"{"status":"success","data":["#,
    ] {
        assert!(!parse(body).page.whole(), "{body}");
    }
    assert!(parse(r#"{"status":"success","data":[]}"#).page.whole());
    assert!(
        parse(r#"{"other":[1,{"a":2}],"status":"success"}"#)
            .page
            .whole()
    );
}

#[test]
fn records_outside_the_shape_spoil_the_answer() {
    for fields in [
        r#","pretime":"1700000000""#,
        r#","pretime":1.5"#,
        r#","pretime":1e9"#,
        r#","pretime":null"#,
        r#","pretime":1700000000,"status":"1""#,
        r#","pretime":1700000000,"section":7"#,
        r#","pretime":1700000000,"genre":[]"#,
        r#","pretime":1700000000,"release":"B-GRP""#,
        r#","pretime":1700000000,"pretime":1700000001"#,
        r#","pretime":1700000000,"re\u0061son":"x""#,
        // No pre time at all.
        "",
    ] {
        let s = parse(&one(fields));
        assert!(!s.page.whole(), "{fields}");
        assert!(s.releases.is_empty(), "{fields}");
    }
    let s = parse(r#"{"status":"success","data":[7]}"#);
    assert_eq!(s.page.skipped.malformed, 1);
    let s = parse(r#"{"status":"success","data":[{"pretime":1700000000}]}"#);
    assert_eq!(s.page.skipped.missing, 1);
}

#[test]
fn odd_but_well_formed_values_keep_nothing_and_read_whole() {
    // A name the scene wouldn't write is dropped, but the rest is used.
    let s = parse(
        r#"{"status":"success","data":[{"release":"Not a scene name","pretime":1700000000},
        {"release":"A.Film.2020-GRP","pretime":1700000000}]}"#,
    );
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!((s.page.skipped.foreign, s.releases.len()), (1, 1));
    // Out-of-range times, an odd section, a long genre and an unknown
    // state keep nothing of those fields.
    let long_genre = "g".repeat(MAX_GENRE + 1);
    let s = parse(&one(&format!(
        r#","pretime":12,"section":"A B","status":9,"reason":"r","genre":"{long_genre}""#
    )));
    assert!(s.page.whole(), "{:?}", s.page);
    let r = &s.releases[0];
    assert_eq!(
        (r.pretime, &r.section, r.nuke),
        (None, &None, Nuke::Unknown)
    );
    assert_eq!((r.reason.as_deref(), &r.genre), (Some("r"), &None));
    let s = parse(&one(&format!(r#","pretime":{}"#, MAX_PRETIME + 1)));
    assert_eq!(s.releases[0].pretime, None);
    // A long reason is cut, hidden characters removed.
    let long = "x".repeat(MAX_REASON + 50);
    let s = parse(&one(&format!(
        r#","pretime":1700000000,"status":1,"reason":"\u200b{long}""#
    )));
    assert_eq!(
        s.releases[0].reason.as_deref().map(str::len),
        Some(MAX_REASON)
    );
}

#[test]
fn the_count_caps_hold() {
    let rec = r#"{"release":"A.Film.2020-GRP","pretime":1700000000}"#;
    let many = |n: usize| {
        let recs = vec![rec; n].join(",");
        format!(r#"{{"status":"success","data":[{recs}]}}"#)
    };
    let s = parse(&many(SEARCH_LIMITS.max_items));
    assert!(s.page.whole());
    assert_eq!(s.releases.len(), SEARCH_LIMITS.max_items);
    let s = parse(&many(SEARCH_LIMITS.max_items + 1));
    assert_eq!(s.page.stop, Some(Stop::Items));
    assert_eq!(s.releases.len(), SEARCH_LIMITS.max_items);
    let tight = Limits {
        max_array: 2,
        ..SEARCH_LIMITS
    };
    let s = parse_search(many(3).as_bytes(), &tight);
    assert!(!s.page.whole());
    assert_eq!(s.releases.len(), 2);
}

#[test]
fn byte_depth_and_text_caps_stop_or_skip() {
    let body = seed("search.json");
    let short = Limits {
        max_bytes: 100,
        ..SEARCH_LIMITS
    };
    let s = parse_search(body, &short);
    assert_eq!(s.page.stop, Some(Stop::Bytes));
    assert!(!s.page.whole());
    let deep = format!(
        r#"{{"status":"success","x":{}1{}}}"#,
        "[".repeat(10),
        "]".repeat(10)
    );
    assert_eq!(parse(&deep).page.stop, Some(Stop::Depth));
    let long = "a".repeat(SEARCH_LIMITS.max_text + 1);
    let s = parse(&format!(
        r#"{{"status":"success","data":[{{"release":"{long}","pretime":1700000000}}]}}"#
    ));
    assert_eq!(s.page.skipped.over_cap, 1);
    assert!(!s.page.whole());
    let values = Limits {
        max_values: 10,
        ..SEARCH_LIMITS
    };
    assert_eq!(parse_search(body, &values).page.stop, Some(Stop::Elements));
    // Not UTF-8 past a point.
    let mut bad = body.to_vec();
    bad.insert(40, 0xff);
    assert_eq!(
        parse_search(&bad, &SEARCH_LIMITS).page.stop,
        Some(Stop::NotUtf8)
    );
}

/// Repeat detection costs the same a key: a record, and the root, with
/// hundreds of thousands of unknown keys is read whole.
#[test]
fn many_unknown_keys_are_read_in_linear_time() {
    let n = 300_000;
    let keys: String = (0..n).map(|i| format!(r#""k{i}":0,"#)).collect();
    let body = format!(
        r#"{{{keys}"status":"success","data":[{{{keys}"release":"A.Film.2020-GRP","pretime":1700000000}}]}}"#
    );
    let big = Limits {
        max_bytes: 16 * 1024 * 1024,
        max_values: 2_000_000,
        ..SEARCH_LIMITS
    };
    assert!(body.len() < big.max_bytes);
    let started = std::time::Instant::now();
    let s = parse_search(body.as_bytes(), &big);
    // A generous bound on the slowest debug runner.
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.releases.len(), 1);
}

fn kept_ok(s: &Search, l: &Limits) -> Result<(), TestCaseError> {
    prop_assert!(s.releases.len() <= l.max_items);
    for r in &s.releases {
        prop_assert_eq!(release_name(&r.name), Some(r.name.as_str()));
        prop_assert!(r.section.as_deref().is_none_or(|t| section(t).is_some()));
        prop_assert!(
            r.pretime
                .is_none_or(|t| (MIN_PRETIME..=MAX_PRETIME).contains(&t))
        );
        for (t, max) in [(&r.reason, MAX_REASON), (&r.genre, MAX_GENRE)] {
            if let Some(t) = t {
                prop_assert!(!t.is_empty() && t.len() <= max);
                prop_assert!(t.trim() == t && !t.chars().any(hidden_char));
            }
        }
        prop_assert!(r.reason.is_none() || r.nuke != Nuke::Clean);
    }
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        kept_ok(&parse_search(&body, &SEARCH_LIMITS), &SEARCH_LIMITS)?;
    }

    #[test]
    fn mutated_seeds_never_panic(
        which in 0..SEEDS.len(),
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
        cut in any::<usize>(),
    ) {
        let mut body = SEEDS[which].1.to_vec();
        for (at, b) in edits {
            let i = at % body.len();
            body[i] = b;
        }
        body.truncate(cut % (body.len() + 1));
        kept_ok(&parse_search(&body, &SEARCH_LIMITS), &SEARCH_LIMITS)?;
    }

    #[test]
    fn tight_caps_hold(
        which in 0..SEEDS.len(),
        items in 0usize..4,
        array in 0usize..4,
        text in 0usize..64,
    ) {
        let l = Limits {
            max_items: items,
            max_array: array,
            max_text: text,
            ..SEARCH_LIMITS
        };
        kept_ok(&parse_search(SEEDS[which].1, &l), &l)?;
    }
}
