//! The MusicBrainz parser (ENR-mb): the shape it reads, its caps, and
//! that nothing it's handed (garbage, cut-short, oversize or mutated
//! answers) panics or keeps more than the caps allow. The corpus is the
//! fuzz target's seeds, `fuzz/seeds/musicbrainz/`.

use lodestar_sources_feeds::musicbrainz::{
    Limits, MAX_TITLE, Primary, SEARCH_LIMITS, Search, Stop, parse_search,
};
use proptest::prelude::*;

const SEEDS: &[(&str, &[u8])] = &[
    (
        "match.json",
        include_bytes!("../../../fuzz/seeds/musicbrainz/match.json"),
    ),
    (
        "candidates.json",
        include_bytes!("../../../fuzz/seeds/musicbrainz/candidates.json"),
    ),
    (
        "empty.json",
        include_bytes!("../../../fuzz/seeds/musicbrainz/empty.json"),
    ),
    (
        "error.json",
        include_bytes!("../../../fuzz/seeds/musicbrainz/error.json"),
    ),
];

const ID: &str = "a1000000-0000-4000-8000-000000000001";

fn seed(name: &str) -> &'static [u8] {
    SEEDS.iter().find(|(n, _)| *n == name).unwrap().1
}

fn parse(body: &str) -> Search {
    parse_search(body.as_bytes(), &SEARCH_LIMITS)
}

/// One result's JSON with `extra` fields after its id.
fn one(extra: &str) -> String {
    format!(r#"{{"release-groups":[{{"id":"{ID}","title":"T"{extra}}}]}}"#)
}

#[test]
fn the_seed_answers_read_whole() {
    let s = parse_search(seed("match.json"), &SEARCH_LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.hits.len(), 3);
    let h = &s.hits[0];
    assert_eq!(h.id.as_str(), ID);
    assert_eq!(h.title, "Harbour Lights");
    assert_eq!((h.year, h.primary), (Some(2004), Some(Primary::Album)));
    assert_eq!(h.credit.as_deref(), Some("The Quiet Engines"));
    assert_eq!(h.artists, ["The Quiet Engines"]);
    assert!(!h.truncated);
    // A two-name credit is printed with its join phrase, and each name
    // kept once.
    let two = &s.hits[1];
    assert_eq!(
        two.credit.as_deref(),
        Some("Marla Venn & The Quiet Engines Orchestra")
    );
    assert_eq!(two.artists, ["Marla Venn", "The Quiet Engines Orchestra"]);
    assert_eq!(two.year, Some(1998));
    // Tags, sort names, aliases, comments: never read.
    assert!(!format!("{s:?}").contains("SENTINEL"));

    let c = parse_search(seed("candidates.json"), &SEARCH_LIMITS);
    assert!(c.page.whole() && c.hits.len() == 4);
    assert_eq!(c.hits[2].primary, Some(Primary::Single));
    assert_eq!(c.hits[3].primary, Some(Primary::Ep));

    let e = parse_search(seed("empty.json"), &SEARCH_LIMITS);
    assert!(e.page.whole() && e.hits.is_empty());

    let err = parse_search(seed("error.json"), &SEARCH_LIMITS);
    assert!(err.page.error && !err.page.whole());
}

#[test]
fn only_the_asked_shape_is_whole() {
    for body in [
        "",
        "null",
        "[]",
        "{}",
        r#"{"release-groups":{}}"#,
        r#"{"release-groups":[],"release-groups":[]}"#,
        r#"{"release-group\u0073":[]}"#,
        r#"{"release-groups":[]} x"#,
        r#"{"release-groups":[],"error":"x"}"#,
        r#"{"release-groups":["x"]}"#,
        r#"{"release-groups":[{"title":"T"}]}"#,
        r#"{"release-groups":[{"id":"x","title":"T"}]}"#,
        r#"{"release-groups":[{"id":"A1000000-0000-4000-8000-000000000001","title":"T"}]}"#,
        &format!(r#"{{"release-groups":[{{"id":"{ID}"}}]}}"#),
        &one(r#","title":"U""#),
        &one(r#","i\u0064":"x""#),
        &one(r#","title2":1,"first-release-date":"2004/01/01""#),
        &one(r#","first-release-date":2004"#),
        &one(r#","primary-type":1"#),
        &one(r#","artist-credit":{}"#),
        &one(r#","artist-credit":[{"joinphrase":""}]"#),
        &one(r#","artist-credit":[{"name":1}]"#),
        &one(r#","artist-credit":[{"name":"A","name":"B"}]"#),
        &one(r#","artist-credit":[{"name":"A","artist":"B"}]"#),
        &one(r#","artist-credit":[{"name":"A","artist":{"name":"B","name":"C"}}]"#),
        &format!(
            r#"{{"release-groups":[{{"id":"{ID}","title":" {} "}}]}}"#,
            '\u{200b}'
        ),
    ] {
        assert!(!parse(body).page.whole(), "{body}");
    }
    // Unknown keys, repeated or not, and nulls in the optional fields.
    let ok = one(
        r#","x":1,"x":2,"first-release-date":null,"primary-type":null,"artist-credit":[{"name":"A","joinphrase":null}],"tags":[]"#,
    );
    let s = parse(&ok);
    assert!(s.page.whole(), "{:?}", s.page);
    let h = &s.hits[0];
    assert_eq!(
        (h.year, h.primary, h.credit.as_deref()),
        (None, None, Some("A"))
    );
    let s = parse(&one(
        r#","first-release-date":"","primary-type":"Audio drama""#,
    ));
    assert!(s.page.whole());
    assert_eq!(s.hits[0].primary, Some(Primary::Other));
}

#[test]
fn the_caps_hold() {
    // Results past the cap stop the parse.
    let many: String = (1..=11)
        .map(|i| format!(r#"{{"id":"a1000000-0000-4000-8000-{i:012}","title":"T"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let s = parse(&format!(r#"{{"release-groups":[{many}]}}"#));
    assert_eq!(s.hits.len(), SEARCH_LIMITS.max_items);
    assert_eq!(s.page.stop, Some(Stop::Items));
    assert!(!s.page.whole());

    // Credits past their cap are passed over; the printed credit is gone.
    let tight = Limits {
        max_credits: 1,
        ..SEARCH_LIMITS
    };
    let body = one(r#","artist-credit":[{"name":"A","joinphrase":" & "},{"name":"B"}]"#);
    let s = parse_search(body.as_bytes(), &tight);
    assert!(s.page.whole());
    assert!(s.hits[0].truncated && s.hits[0].credit.is_none());
    assert_eq!(s.hits[0].artists, ["A"]);

    // An array over `max_array`, read or passed over, spoils its result.
    let tight = Limits {
        max_array: 2,
        ..SEARCH_LIMITS
    };
    for extra in [
        r#","artist-credit":[{"name":"A"},{"name":"B"},{"name":"C"}]"#,
        r#","releases":[1,2,3]"#,
    ] {
        let s = parse_search(one(extra).as_bytes(), &tight);
        assert_eq!(s.page.skipped.over_cap, 1, "{extra}");
    }

    // Bytes, depth, text and UTF-8.
    let body = seed("match.json");
    let cut = Limits {
        max_bytes: body.len() / 2,
        ..SEARCH_LIMITS
    };
    assert_eq!(parse_search(body, &cut).page.stop, Some(Stop::Bytes));
    let truncated = parse_search(&body[..body.len() - 3], &SEARCH_LIMITS);
    assert_eq!(truncated.page.stop, Some(Stop::Malformed));
    let deep = format!(
        r#"{{"release-groups":{}{}}}"#,
        "[".repeat(64),
        "]".repeat(64)
    );
    assert_eq!(parse(&deep).page.stop, Some(Stop::Depth));
    let long = one(&format!(
        r#","primary-type":"{}""#,
        "a".repeat(SEARCH_LIMITS.max_text + 1)
    ));
    assert_eq!(parse(&long).page.skipped.over_cap, 1);
    let bad_utf8 = b"{\"release-groups\":[]\xff}";
    assert_eq!(
        parse_search(bad_utf8, &SEARCH_LIMITS).page.stop,
        Some(Stop::NotUtf8)
    );
}

/// Repeat detection costs the same a key: a result with hundreds of
/// thousands of unknown keys is read whole.
#[test]
fn many_unknown_keys_are_read_in_linear_time() {
    let n = 300_000;
    let keys: String = (0..n).map(|i| format!(r#""k{i}":0,"#)).collect();
    let body = format!(r#"{{{keys}"release-groups":[{{{keys}"id":"{ID}","title":"T"}}]}}"#);
    let big = Limits {
        max_bytes: 16 * 1024 * 1024,
        max_values: 1_000_000,
        ..SEARCH_LIMITS
    };
    let started = std::time::Instant::now();
    let s = parse_search(body.as_bytes(), &big);
    // A generous bound on the slowest debug runner.
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.hits.len(), 1);
}

fn kept_ok(s: &Search, l: &Limits) -> Result<(), TestCaseError> {
    prop_assert!(s.hits.len() <= l.max_items);
    for h in &s.hits {
        prop_assert_eq!(h.id.as_str().len(), 36);
        prop_assert!(!h.title.is_empty() && h.title.len() <= MAX_TITLE);
        prop_assert!(h.title.trim() == h.title);
        prop_assert!(h.year.is_none_or(|y| (1..=9999).contains(&y)));
        prop_assert!(h.artists.len() <= 2 * l.max_credits);
        for a in h.artists.iter().chain(&h.credit) {
            prop_assert!(!a.is_empty() && a.len() <= MAX_TITLE && a.trim() == a);
        }
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
        tight in any::<u8>(),
    ) {
        let mut body = SEEDS[which].1.to_vec();
        for (at, b) in edits {
            let i = at % body.len();
            body[i] = b;
        }
        body.truncate(cut % (body.len() + 1));
        kept_ok(&parse_search(&body, &SEARCH_LIMITS), &SEARCH_LIMITS)?;
        let l = Limits {
            max_array: 1 + usize::from(tight >> 4),
            max_text: 1 + usize::from(tight),
            max_items: usize::from(tight & 7),
            max_credits: usize::from(tight >> 5),
            ..SEARCH_LIMITS
        };
        kept_ok(&parse_search(&body, &l), &l)?;
    }

    /// Any title, credit name and date in the shape round-trip: the parser
    /// keeps the cleaned text and the year.
    #[test]
    fn well_formed_results_read_back(
        title in "[A-Za-z0-9 ]{0,8}[A-Za-z0-9]{1,8}",
        name in "[A-Za-z0-9]{1,12}",
        year in 1u16..=9999,
    ) {
        let body = one(&format!(
            r#","first-release-date":"{year:04}-01","artist-credit":[{{"name":"{name}","joinphrase":""}}]"#
        ))
        .replace(r#""title":"T""#, &format!(r#""title":"{title}""#));
        let s = parse(&body);
        prop_assert!(s.page.whole(), "{:?}", s.page);
        prop_assert_eq!(&s.hits[0].title, title.trim());
        prop_assert_eq!(s.hits[0].year, Some(year));
        prop_assert_eq!(s.hits[0].credit.as_deref(), Some(name.as_str()));
    }
}
