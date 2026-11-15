//! The Discogs parser (ENR-discogs): the shapes it reads, its caps, and
//! that nothing it's handed (garbage, cut-short, oversize or mutated
//! answers) panics or keeps more than the caps allow. The corpus is the
//! fuzz target's seeds, `fuzz/seeds/discogs/`.

use lodestar_sources_feeds::discogs::{LIMITS, Limits, MAX_TITLE, Search, Stop, parse_search};
use proptest::prelude::*;

const SEEDS: &[(&str, &[u8])] = &[
    (
        "match.json",
        include_bytes!("../../../fuzz/seeds/discogs/match.json"),
    ),
    (
        "candidates.json",
        include_bytes!("../../../fuzz/seeds/discogs/candidates.json"),
    ),
    (
        "empty.json",
        include_bytes!("../../../fuzz/seeds/discogs/empty.json"),
    ),
    (
        "error.json",
        include_bytes!("../../../fuzz/seeds/discogs/error.json"),
    ),
];

fn seed(name: &str) -> &'static [u8] {
    SEEDS.iter().find(|(n, _)| *n == name).unwrap().1
}

fn parse(body: &str) -> Search {
    parse_search(body.as_bytes(), &LIMITS)
}

/// One master result with `extra` fields added after the usual ones.
fn one(extra: &str) -> String {
    format!(
        r#"{{"pagination":{{"page":1}},"results":[{{"id":7,"type":"master","title":"A - B","year":"2001","genre":["Rock"]{extra}}}]}}"#
    )
}

#[test]
fn a_search_answer_is_read() {
    let s = parse_search(seed("match.json"), &LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.page.kept, 2);
    let h = &s.hits[0];
    assert_eq!(h.id, 900001);
    assert_eq!(h.title, "Synthetic Orchestra - Paper Lanterns");
    assert_eq!(h.year, Some(1998));
    assert!(
        !format!("{s:?}").contains("SENTINEL"),
        "nothing else is kept"
    );
    // A year written as a number reads the same.
    let s = parse_search(seed("candidates.json"), &LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.hits[2].year, Some(2004));
}

#[test]
fn an_error_object_and_other_shapes_are_not_whole() {
    let e = parse_search(seed("error.json"), &LIMITS);
    assert!(e.page.error && !e.page.whole());
    assert!(!format!("{e:?}").contains("SENTINEL"));
    for body in [
        "",
        "[]",
        "null",
        "{}",
        r#"{"results":{}}"#,
        r#"{"results":[]} x"#,
        r#"{"results":[],"results":[]}"#,
        r#"{"res\u0075lts":[]}"#,
        r#"{"results":[1]}"#,
        r#"{"results":["#,
        r#"{"results":[],"message":"x"}"#,
    ] {
        assert!(!parse(body).page.whole(), "{body}");
    }
    // An empty list is a whole answer: a miss.
    let s = parse_search(seed("empty.json"), &LIMITS);
    assert!(s.page.whole() && s.hits.is_empty());
}

#[test]
fn a_wrong_or_repeated_field_spoils_its_result() {
    assert!(parse(&one("")).page.whole());
    for extra in [
        r#","id":8"#,
        r#","title":"Again""#,
        r#","t\u0069tle":"Escaped""#,
        r#","year":"2001""#,
        r#","type":"master""#,
    ] {
        let s = parse(&one(extra));
        assert!(!s.page.whole() && s.hits.is_empty(), "{extra}");
    }
    let field = |k: &str, v: &str| {
        parse(&format!(
            r#"{{"results":[{{"id":7,"title":"A - B","{k}":{v}}}]}}"#
        ))
    };
    for (k, v) in [
        ("type", r#""release""#),
        ("type", r#""artist""#),
        ("type", "1"),
        ("title", "1"),
        ("title", "null"),
        ("year", r#""1998-01-01""#),
        ("year", r#""-1""#),
        ("year", r#""10000""#),
        ("year", "1998.0"),
        ("year", "-1"),
        ("year", "true"),
    ] {
        let s = field(k, v);
        assert!(!s.page.whole() && s.hits.is_empty(), "{k}: {v}");
    }
    for id in ["0", "-7", "7.0", "1e3", "\"7\"", "18446744073709551616"] {
        let s = parse(&format!(r#"{{"results":[{{"id":{id},"title":"A"}}]}}"#));
        assert!(!s.page.whole() && s.hits.is_empty(), "{id}");
    }
    // No title, or one of nothing but hidden characters: skipped.
    for t in ["", r#","title":"""#, r#","title":"\u200b""#] {
        let s = parse(&format!(r#"{{"results":[{{"id":1{t}}}]}}"#));
        assert_eq!(s.page.skipped.no_title, 1, "{t}");
    }
    // `0`, `"0"`, `""` and `null` are no year.
    for v in ["0", r#""0""#, r#""""#, "null"] {
        let s = field("year", v);
        assert!(s.page.whole(), "{v}");
        assert_eq!(s.hits[0].year, None, "{v}");
    }
    // No type is a master's answer too.
    let s = parse(r#"{"results":[{"id":1,"title":"A - B"}]}"#);
    assert!(s.page.whole());
}

#[test]
fn the_caps_hold() {
    // Results past `max_items` stop the parse.
    let results: Vec<String> = (1..=LIMITS.max_items + 1)
        .map(|i| format!(r#"{{"id":{i},"title":"F"}}"#))
        .collect();
    let s = parse(&format!(r#"{{"results":[{}]}}"#, results.join(",")));
    assert_eq!(s.page.stop, Some(Stop::Items));
    assert!(s.hits.len() <= LIMITS.max_items && !s.page.whole());
    // A long array inside a result spoils it.
    let long: Vec<&str> = vec!["0"; LIMITS.max_array + 1];
    let s = parse(&one(&format!(r#","barcode":[{}]"#, long.join(","))));
    assert!(!s.page.whole());
    // A long title is cut to `MAX_TITLE`; one over `max_text` spoils.
    let long = "a".repeat(LIMITS.max_text);
    let s = parse(&format!(r#"{{"results":[{{"id":1,"title":"{long}"}}]}}"#));
    assert!(s.hits[0].title.len() <= MAX_TITLE);
    let longer = "a".repeat(LIMITS.max_text + 1);
    let s = parse(&format!(r#"{{"results":[{{"id":1,"title":"{longer}"}}]}}"#));
    assert_eq!(s.page.skipped.over_cap, 1);
    // Bytes past `max_bytes` stop it.
    let mut big = one("").into_bytes();
    let close = big.pop().unwrap();
    big.resize(LIMITS.max_bytes + 1, b' ');
    big.push(close);
    let s = parse_search(&big, &LIMITS);
    assert_eq!(s.page.stop, Some(Stop::Bytes));
    // Depth.
    let deep = format!(
        r#"{{"results":[{{"id":1,"x":{}{}}}]}}"#,
        "[".repeat(20),
        "]".repeat(20)
    );
    assert_eq!(parse(&deep).page.stop, Some(Stop::Depth));
}

/// Hundreds of thousands of unknown keys are read whole, in linear time.
#[test]
fn many_unknown_keys_are_read_in_linear_time() {
    let wide = Limits {
        max_values: 2_000_000,
        max_bytes: 16 * 1024 * 1024,
        ..LIMITS
    };
    let keys: String = (0..300_000).map(|i| format!(r#","k{i}":0"#)).collect();
    let body = one(&keys);
    let start = std::time::Instant::now();
    let s = parse_search(body.as_bytes(), &wide);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.hits.len(), 1);
    // Generous for the slowest CI runner in debug; quadratic is minutes.
    assert!(start.elapsed() < std::time::Duration::from_secs(20));
}

fn kept_ok(s: &Search, l: &Limits) -> Result<(), TestCaseError> {
    prop_assert!(s.hits.len() <= l.max_items);
    for h in &s.hits {
        prop_assert!(h.id > 0);
        prop_assert!(!h.title.is_empty() && h.title.len() <= MAX_TITLE);
        prop_assert!(h.year.is_none_or(|y| (1..=9999).contains(&y)));
    }
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        kept_ok(&parse_search(&body, &LIMITS), &LIMITS)?;
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
        kept_ok(&parse_search(&body, &LIMITS), &LIMITS)?;
    }

    /// A cut-short answer is never whole, so never a miss.
    #[test]
    fn a_cut_answer_is_never_whole(which in 0..3usize, cut in any::<usize>()) {
        let body = SEEDS[which].1;
        let trimmed = body.trim_ascii_end().len();
        let at = cut % trimmed;
        prop_assert!(!parse_search(&body[..at], &LIMITS).page.whole());
    }
}
