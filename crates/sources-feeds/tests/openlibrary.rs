//! The Open Library parser (ENR-ol): the shape it reads, its caps, and
//! that nothing it's handed (garbage, cut-short, oversize or mutated
//! answers) panics or keeps more than the caps allow. The corpus is the
//! fuzz target's seeds, `fuzz/seeds/openlibrary/`.

use lodestar_sources_feeds::openlibrary::{
    Limits, MAX_TITLE, Olid, SEARCH_LIMITS, Search, Stop, parse_search,
};
use proptest::prelude::*;

const SEEDS: &[(&str, &[u8])] = &[
    (
        "search.json",
        include_bytes!("../../../fuzz/seeds/openlibrary/search.json"),
    ),
    (
        "candidates.json",
        include_bytes!("../../../fuzz/seeds/openlibrary/candidates.json"),
    ),
    (
        "empty.json",
        include_bytes!("../../../fuzz/seeds/openlibrary/empty.json"),
    ),
    (
        "error.json",
        include_bytes!("../../../fuzz/seeds/openlibrary/error.json"),
    ),
];

fn seed(name: &str) -> &'static [u8] {
    SEEDS.iter().find(|(n, _)| *n == name).unwrap().1
}

fn parse(body: &str) -> Search {
    parse_search(body.as_bytes(), &SEARCH_LIMITS)
}

/// An answer of one work with `fields` after its key.
fn one(fields: &str) -> String {
    format!(r#"{{"docs":[{{"key":"/works/OL5W"{fields}}}]}}"#)
}

#[test]
fn the_seed_answers_read_whole() {
    let s = parse_search(seed("search.json"), &SEARCH_LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    let ids: Vec<Olid> = s.works.iter().map(|w| w.id).collect();
    assert_eq!(ids, [Olid(900_001), Olid(900_002), Olid(900_003)]);
    let w = &s.works[0];
    assert_eq!(w.title.as_deref(), Some("Frankenstein"));
    assert_eq!(
        w.alternatives,
        [
            "Frankenstein; or, The Modern Prometheus",
            "Frankenstein",
            "Frankenstein, ou le Prométhée moderne"
        ]
    );
    assert_eq!(w.first_year, Some(1818));
    assert_eq!(w.years, [1818, 1831, 1999, 2003]);
    assert!(!w.truncated);
    // A work may leave out its years and alternatives.
    assert_eq!(s.works[2].years, Vec::<u16>::new());
    assert_eq!(s.works[1].alternatives, Vec::<String>::new());
    // Authors and subjects the service sent unasked are never read.
    assert!(!format!("{s:?}").contains("SENTINEL"));

    let c = parse_search(seed("candidates.json"), &SEARCH_LIMITS);
    assert!(c.page.whole() && c.works.len() == 3);
    let e = parse_search(seed("empty.json"), &SEARCH_LIMITS);
    assert!(e.page.whole() && e.works.is_empty());

    let err = parse_search(seed("error.json"), &SEARCH_LIMITS);
    assert!(err.page.error && !err.page.whole());
    assert!(!format!("{err:?}").contains("SENTINEL"));
}

#[test]
fn only_the_asked_shape_is_whole() {
    for body in [
        "",
        "[]",
        "null",
        "{}",
        r#"{"docs":{}}"#,
        r#"{"docs":null}"#,
        r#"{"docs":[]} x"#,
        r#"{"docs":[],"docs":[]}"#,
        r#"{"do\u0063s":[]}"#,
        r#"{"docs":[],"error":"x"}"#,
        r#"{"docs":["#,
    ] {
        assert!(!parse(body).page.whole(), "{body}");
    }
    // Unknown root keys, and their repeats, are passed over.
    let s = parse(r#"{"numFound":0,"x":1,"x":2,"docs":[],"q":"","offset":null}"#);
    assert!(s.page.whole(), "{:?}", s.page);
}

#[test]
fn works_outside_the_shape_spoil_the_answer() {
    for body in [
        // Not an object, no key, or a key that isn't a work's.
        r#"{"docs":[1]}"#.to_owned(),
        r#"{"docs":[{"title":"Dune"}]}"#.into(),
        r#"{"docs":[{"key":"/authors/OL1A"}]}"#.into(),
        r#"{"docs":[{"key":"/books/OL1M"}]}"#.into(),
        r#"{"docs":[{"key":"/works/OL01W"}]}"#.into(),
        r#"{"docs":[{"key":5}]}"#.into(),
        // Wrong types in what's read.
        one(r#","title":5"#),
        one(r#","title":null"#),
        one(r#","alternative_title":"Dune""#),
        one(r#","alternative_title":[5]"#),
        one(r#","first_publish_year":"1965""#),
        one(r#","first_publish_year":null"#),
        one(r#","publish_year":1965"#),
        one(r#","publish_year":["1965"]"#),
        // A lone surrogate in a title.
        one(r#","title":"\ud800""#),
        // Repeats of read keys, or an escaped key.
        one(r#","title":"A","title":"B""#),
        one(r#","key":"/works/OL5W""#),
        one(r#","publish_year":[],"publish_year":[]"#),
        one(r#","ti\u0074le":"A""#),
    ] {
        let s = parse(&body);
        assert!(!s.page.whole(), "{body}");
        assert!(s.works.is_empty(), "{body}");
    }
    // Unknown keys and their repeats are passed over, whatever they hold.
    let s = parse(&one(r#","author_name":["x"],"x":1,"x":{"a":[1]}"#));
    assert!(s.page.whole(), "{:?}", s.page);
}

#[test]
fn odd_but_well_formed_values_keep_nothing_and_read_whole() {
    let s = parse(&one(
        r#","title":" \u200b ","alternative_title":["","\u0000"],"first_publish_year":-5,"publish_year":[0,10000,1.5,1e3,99999999999999999999,2001]"#,
    ));
    assert!(s.page.whole(), "{:?}", s.page);
    let w = &s.works[0];
    assert_eq!(w.title, None);
    assert!(w.alternatives.is_empty());
    assert_eq!(w.first_year, None);
    assert_eq!(w.years, [2001]);
    // A bare work id is read too.
    let s = parse(r#"{"docs":[{"key":"OL7W"}]}"#);
    assert_eq!(s.works[0].id, Olid(7));
}

#[test]
fn the_count_caps_hold() {
    let docs: Vec<String> = (1..=11)
        .map(|i| format!(r#"{{"key":"/works/OL{i}W"}}"#))
        .collect();
    let s = parse(&format!(r#"{{"docs":[{}]}}"#, docs.join(",")));
    assert_eq!(s.works.len(), SEARCH_LIMITS.max_items);
    assert_eq!(s.page.stop, Some(Stop::Items));
    assert!(!s.page.whole());

    let tight = Limits {
        max_texts: 2,
        max_years: 2,
        ..SEARCH_LIMITS
    };
    let body = one(r#","title":"A","alternative_title":["B","C","D"],"publish_year":[1,2,3,4]"#);
    let s = parse_search(body.as_bytes(), &tight);
    assert!(s.page.whole(), "{:?}", s.page);
    let w = &s.works[0];
    assert_eq!(w.alternatives, ["B"]);
    assert_eq!(w.years, [1, 2]);
    assert!(w.truncated);
    // The title past the cap is passed over too.
    let s = parse_search(
        one(r#","alternative_title":["B","C"],"title":"A""#).as_bytes(),
        &tight,
    );
    assert_eq!(s.works[0].title, None);
    assert!(s.works[0].truncated);
}

#[test]
fn byte_depth_and_text_caps_stop_or_skip() {
    let mut big = b"{\"docs\":[]}".to_vec();
    big.splice(1..1, std::iter::repeat_n(b' ', SEARCH_LIMITS.max_bytes));
    let s = parse_search(&big, &SEARCH_LIMITS);
    assert_eq!(s.page.stop, Some(Stop::Bytes));
    assert!(!s.page.whole());

    let deep = format!(
        r#"{{"docs":[{{"key":"/works/OL5W","x":{}{}}}]}}"#,
        "[".repeat(20),
        "]".repeat(20)
    );
    assert_eq!(parse(&deep).page.stop, Some(Stop::Depth));

    let long = one(&format!(r#","title":"{}""#, "a".repeat(5000)));
    let s = parse(&long);
    assert_eq!(s.page.skipped.over_cap, 1);
    assert!(s.works.is_empty());

    let arr = Limits {
        max_array: 2,
        ..SEARCH_LIMITS
    };
    for f in [
        r#","publish_year":[1,2,3]"#,
        r#","alternative_title":["a","b","c"]"#,
        r#","x":[1,2,3]"#,
    ] {
        let s = parse_search(one(f).as_bytes(), &arr);
        assert_eq!(s.page.skipped.over_cap, 1, "{f}");
    }

    let bad_utf8 = b"{\"docs\":[{\"key\":\"/works/OL1W\"}]\xff}";
    assert_eq!(
        parse_search(bad_utf8, &SEARCH_LIMITS).page.stop,
        Some(Stop::NotUtf8)
    );
    // A kept title is never longer than the cap, even at the text cap.
    let wide = Limits {
        max_text: 64 * 1024,
        ..SEARCH_LIMITS
    };
    let s = parse_search(
        one(&format!(r#","title":"{}""#, "a".repeat(MAX_TITLE + 10))).as_bytes(),
        &wide,
    );
    assert!(
        s.works
            .iter()
            .all(|w| w.title.as_ref().is_none_or(|t| t.len() <= MAX_TITLE))
    );
}

/// Repeat detection costs the same a key: a work, and the root, with
/// hundreds of thousands of unknown keys is read whole.
#[test]
fn many_unknown_keys_are_read_in_linear_time() {
    let n = 300_000;
    let keys: String = (0..n).map(|i| format!(r#""k{i}":0,"#)).collect();
    let body = format!(r#"{{{keys}"docs":[{{{keys}"key":"/works/OL5W"}}]}}"#);
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
    assert_eq!(s.works.len(), 1);
}

fn kept_ok(s: &Search, l: &Limits) -> Result<(), TestCaseError> {
    prop_assert!(s.works.len() <= l.max_items);
    for w in &s.works {
        let texts = w.title.iter().count() + w.alternatives.len();
        prop_assert!(texts <= l.max_texts);
        prop_assert!(w.years.len() <= l.max_years);
        for t in w.title.iter().chain(&w.alternatives) {
            prop_assert!(!t.is_empty() && t.len() <= MAX_TITLE);
            prop_assert!(t.trim() == t);
        }
        prop_assert!(w.first_year.is_none_or(|y| (1..=9999).contains(&y)));
        prop_assert!(w.years.iter().all(|y| (1..=9999).contains(y)));
        prop_assert_eq!(Olid::parse(&w.id.to_string()), Some(w.id));
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
        texts in 0usize..4,
        years in 0usize..4,
        items in 0usize..4,
        array in 0usize..4,
    ) {
        let l = Limits {
            max_texts: texts,
            max_years: years,
            max_items: items,
            max_array: array,
            ..SEARCH_LIMITS
        };
        kept_ok(&parse_search(SEEDS[which].1, &l), &l)?;
    }

    /// Any work id read back is the one written.
    #[test]
    fn work_ids_round_trip(n in 1u64..) {
        let key = format!("/works/OL{n}W");
        prop_assert_eq!(Olid::parse(&key), Some(Olid(n)));
        prop_assert_eq!(Olid(n).to_string(), format!("OL{n}W"));
    }
}
