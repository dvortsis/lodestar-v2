//! The AniList parser (ENR-anilist): the shape it reads, its caps, and
//! that nothing it's handed (garbage, cut-short, oversize or mutated
//! answers) panics or keeps more than the caps allow. The corpus is the
//! fuzz target's seeds, `fuzz/seeds/anilist/`.

use lodestar_sources_feeds::anilist::{
    Format, Limits, MAX_TITLE, MediaId, SEARCH_LIMITS, Search, Stop, parse_search,
};
use proptest::prelude::*;

const SEEDS: &[(&str, &[u8])] = &[
    (
        "search.json",
        include_bytes!("../../../fuzz/seeds/anilist/search.json"),
    ),
    (
        "candidates.json",
        include_bytes!("../../../fuzz/seeds/anilist/candidates.json"),
    ),
    (
        "empty.json",
        include_bytes!("../../../fuzz/seeds/anilist/empty.json"),
    ),
    (
        "error.json",
        include_bytes!("../../../fuzz/seeds/anilist/error.json"),
    ),
];

fn seed(name: &str) -> &'static [u8] {
    SEEDS.iter().find(|(n, _)| *n == name).unwrap().1
}

fn parse(body: &str) -> Search {
    parse_search(body.as_bytes(), &SEARCH_LIMITS)
}

/// An answer of one media with `fields` after its id.
fn one(fields: &str) -> String {
    format!(r#"{{"data":{{"Page":{{"media":[{{"id":5{fields}}}]}}}}}}"#)
}

#[test]
fn the_seed_answers_read_whole() {
    let s = parse_search(seed("search.json"), &SEARCH_LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    let ids: Vec<MediaId> = s.media.iter().map(|m| m.id).collect();
    assert_eq!(ids, [MediaId(900_001), MediaId(900_002), MediaId(900_003)]);
    let m = &s.media[0];
    assert_eq!(m.format, Some(Format::Movie));
    assert_eq!(m.adult, Some(false));
    assert_eq!(m.country.as_deref(), Some("JP"));
    assert_eq!((m.season_year, m.start_year), (Some(1997), Some(1997)));
    assert_eq!(m.romaji.as_deref(), Some("Perfect Blue"));
    assert_eq!(m.english.as_deref(), Some("Perfect Blue"));
    assert_eq!(m.native.as_deref(), Some("パーフェクトブルー"));
    assert_eq!(m.synonyms, ["Pāfekuto Burū", "Perfect Blue Stage"]);
    assert!(!m.truncated);
    assert_eq!(s.media[1].adult, Some(true));
    // A `null` season year and titles read as none.
    assert_eq!(s.media[2].season_year, None);
    assert_eq!(s.media[2].english, None);
    // Descriptions and staff the service sent unasked are never read.
    assert!(!format!("{s:?}").contains("SENTINEL description"));
    assert!(!format!("{s:?}").contains("SENTINEL person"));

    let c = parse_search(seed("candidates.json"), &SEARCH_LIMITS);
    assert!(c.page.whole() && c.media.len() == 4);
    assert_eq!(c.media[3].format, Some(Format::Music));
    let e = parse_search(seed("empty.json"), &SEARCH_LIMITS);
    assert!(e.page.whole() && e.media.is_empty());

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
        r#"{"data":null}"#,
        r#"{"data":{}}"#,
        r#"{"data":{"Page":null}}"#,
        r#"{"data":{"Page":{"media":null}}}"#,
        r#"{"data":{"Page":{"media":{}}}}"#,
        r#"{"data":[]}"#,
        r#"{"data":{"Page":5}}"#,
        r#"{"data":{"Page":{"media":[]}}} x"#,
        r#"{"data":{"Page":{"media":[]}},"data":{"Page":{"media":[]}}}"#,
        r#"{"data":{"Page":{"media":[],"media":[]}}}"#,
        concat!(r#"{"d\"#, r#"u0061ta":{"Page":{"media":[]}}}"#),
        r#"{"data":{"Page":{"media":[]}},"errors":[]}"#,
        r#"{"data":{"Page":{"media":["#,
    ] {
        assert!(!parse(body).page.whole(), "{body}");
    }
    // Unknown keys on the way, and their repeats, are passed over.
    let s = parse(
        r#"{"x":1,"x":2,"data":{"y":[],"Page":{"pageInfo":{"total":0},"media":[]}},"extensions":{}}"#,
    );
    assert!(s.page.whole(), "{:?}", s.page);
}

#[test]
fn media_outside_the_shape_spoil_the_answer() {
    for body in [
        // Not an object, no id, or an id that isn't a positive integer.
        r#"{"data":{"Page":{"media":[1]}}}"#.to_owned(),
        r#"{"data":{"Page":{"media":[{"format":"TV"}]}}}"#.into(),
        r#"{"data":{"Page":{"media":[{"id":0}]}}}"#.into(),
        r#"{"data":{"Page":{"media":[{"id":-1}]}}}"#.into(),
        r#"{"data":{"Page":{"media":[{"id":1.5}]}}}"#.into(),
        r#"{"data":{"Page":{"media":[{"id":"5"}]}}}"#.into(),
        r#"{"data":{"Page":{"media":[{"id":null}]}}}"#.into(),
        // Wrong types in what's read.
        one(r#","format":5"#),
        one(r#","isAdult":"false""#),
        one(r#","isAdult":0"#),
        one(r#","countryOfOrigin":["JP"]"#),
        one(r#","seasonYear":"1998""#),
        one(r#","startDate":1998"#),
        one(r#","startDate":{"year":"1998"}"#),
        one(r#","title":"Bebop""#),
        one(r#","title":{"romaji":5}"#),
        one(r#","synonyms":"Bebop""#),
        one(r#","synonyms":[5]"#),
        one(r#","synonyms":[null]"#),
        // A lone surrogate in a title.
        one(r#","title":{"english":"\ud800"}"#),
        // Repeats of read keys, or an escaped key, at every level.
        one(r#","isAdult":false,"isAdult":true"#),
        one(r#","id":5"#),
        one(r#","title":{"romaji":"A","romaji":"B"}"#),
        one(r#","startDate":{"year":1,"year":2}"#),
        one(concat!(r#","is\"#, r#"u0041dult":true"#)),
        one(concat!(r#","title":{"rom\"#, r#"u0061ji":"A"}"#)),
    ] {
        let s = parse(&body);
        assert!(!s.page.whole(), "{body}");
        assert!(s.media.is_empty(), "{body}");
    }
    // Unknown keys and their repeats are passed over, whatever they hold.
    let s = parse(&one(
        r#","description":"x","x":1,"x":{"a":[1]},"title":{"userPreferred":"y","y":[]},"startDate":{"month":1}"#,
    ));
    assert!(s.page.whole(), "{:?}", s.page);
}

#[test]
fn nulls_and_odd_but_well_formed_values_keep_nothing_and_read_whole() {
    let s = parse(&one(
        r#","format":null,"isAdult":null,"countryOfOrigin":"jp","seasonYear":-5,"startDate":null,"title":null,"synonyms":null"#,
    ));
    assert!(s.page.whole(), "{:?}", s.page);
    let m = &s.media[0];
    assert_eq!(
        (m.format, m.adult, m.country.as_deref()),
        (None, None, None)
    );
    assert_eq!((m.season_year, m.start_year), (None, None));
    assert_eq!(m.titles().count(), 0);
    let s = parse(&one(
        r#","format":"HOLOGRAM","countryOfOrigin":"JPN","seasonYear":10000,"startDate":{"year":1e3},"title":{"romaji":" \u200b ","english":null},"synonyms":["","\u0000","B"]"#,
    ));
    assert!(s.page.whole(), "{:?}", s.page);
    let m = &s.media[0];
    assert_eq!(
        (m.format, m.country.as_deref()),
        (Some(Format::Other), None)
    );
    assert_eq!((m.season_year, m.start_year), (None, None));
    assert_eq!(m.romaji, None);
    assert_eq!(m.synonyms, ["B"]);
}

#[test]
fn the_count_caps_hold() {
    let media: Vec<String> = (1..=11).map(|i| format!(r#"{{"id":{i}}}"#)).collect();
    let s = parse(&format!(
        r#"{{"data":{{"Page":{{"media":[{}]}}}}}}"#,
        media.join(",")
    ));
    assert_eq!(s.media.len(), SEARCH_LIMITS.max_items);
    assert_eq!(s.page.stop, Some(Stop::Items));
    assert!(!s.page.whole());

    let tight = Limits {
        max_texts: 2,
        ..SEARCH_LIMITS
    };
    let body = one(r#","title":{"romaji":"A","native":"N"},"synonyms":["B","C"]"#);
    let s = parse_search(body.as_bytes(), &tight);
    assert!(s.page.whole(), "{:?}", s.page);
    let m = &s.media[0];
    assert_eq!(
        (m.romaji.as_deref(), m.native.as_deref()),
        (Some("A"), Some("N"))
    );
    assert!(m.synonyms.is_empty());
    assert!(m.truncated);
    // Titles after the synonyms are passed over too.
    let s = parse_search(
        one(r#","synonyms":["B","C"],"title":{"romaji":"A"}"#).as_bytes(),
        &tight,
    );
    assert_eq!(
        (s.media[0].romaji.as_deref(), s.media[0].synonyms.len()),
        (None, 2)
    );
    assert!(s.media[0].truncated);
}

#[test]
fn byte_depth_and_text_caps_stop_or_skip() {
    let mut big = br#"{"data":{"Page":{"media":[]}}}"#.to_vec();
    big.splice(1..1, std::iter::repeat_n(b' ', SEARCH_LIMITS.max_bytes));
    let s = parse_search(&big, &SEARCH_LIMITS);
    assert_eq!(s.page.stop, Some(Stop::Bytes));
    assert!(!s.page.whole());

    let deep = one(&format!(r#","x":{}{}"#, "[".repeat(20), "]".repeat(20)));
    assert_eq!(parse(&deep).page.stop, Some(Stop::Depth));

    let long = one(&format!(r#","synonyms":["{}"]"#, "a".repeat(5000)));
    let s = parse(&long);
    assert_eq!(s.page.skipped.over_cap, 1);
    assert!(s.media.is_empty());

    let arr = Limits {
        max_array: 2,
        ..SEARCH_LIMITS
    };
    for f in [r#","synonyms":["a","b","c"]"#, r#","x":[1,2,3]"#] {
        let s = parse_search(one(f).as_bytes(), &arr);
        assert_eq!(s.page.skipped.over_cap, 1, "{f}");
    }
    // Media past the array cap stop the read.
    let s = parse_search(
        br#"{"data":{"Page":{"media":[{"id":1},{"id":2},{"id":3}]}}}"#,
        &arr,
    );
    assert_eq!((s.media.len(), s.page.stop), (2, Some(Stop::Items)));

    let bad_utf8 = b"{\"data\":{\"Page\":{\"media\":[]}}\xff}";
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
        one(&format!(
            r#","title":{{"romaji":"{}"}}"#,
            "a".repeat(MAX_TITLE + 10)
        ))
        .as_bytes(),
        &wide,
    );
    assert!(
        s.media
            .iter()
            .all(|m| m.titles().all(|t| t.len() <= MAX_TITLE))
    );
}

/// Repeat detection costs the same a key: a media, its title, and the
/// root, with hundreds of thousands of unknown keys, is read whole.
#[test]
fn many_unknown_keys_are_read_in_linear_time() {
    let n = 200_000;
    let keys: String = (0..n).map(|i| format!(r#""k{i}":0,"#)).collect();
    let body = format!(
        r#"{{{keys}"data":{{"Page":{{"media":[{{{keys}"title":{{{keys}"romaji":"A"}},"id":5}}]}}}}}}"#
    );
    let big = Limits {
        max_bytes: 32 * 1024 * 1024,
        max_values: 2_000_000,
        ..SEARCH_LIMITS
    };
    assert!(body.len() < big.max_bytes);
    let started = std::time::Instant::now();
    let s = parse_search(body.as_bytes(), &big);
    // A generous bound on the slowest debug runner.
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.media.len(), 1);
}

fn kept_ok(s: &Search, l: &Limits) -> Result<(), TestCaseError> {
    prop_assert!(s.media.len() <= l.max_items);
    for m in &s.media {
        prop_assert!(m.titles().count() <= l.max_texts);
        for t in m.titles() {
            prop_assert!(!t.is_empty() && t.len() <= MAX_TITLE);
            prop_assert!(t.trim() == t);
        }
        prop_assert!(m.season_year.is_none_or(|y| (1..=9999).contains(&y)));
        prop_assert!(m.start_year.is_none_or(|y| (1..=9999).contains(&y)));
        prop_assert!(m.country.as_ref().is_none_or(|c| c.len() == 2));
        prop_assert_eq!(MediaId::parse(&m.id.to_string()), Some(m.id));
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
        items in 0usize..4,
        array in 0usize..4,
    ) {
        let l = Limits {
            max_texts: texts,
            max_items: items,
            max_array: array,
            ..SEARCH_LIMITS
        };
        kept_ok(&parse_search(SEEDS[which].1, &l), &l)?;
    }

    /// Any media id read back is the one written.
    #[test]
    fn media_ids_round_trip(n in 1u64..) {
        prop_assert_eq!(MediaId::parse(&n.to_string()), Some(MediaId(n)));
    }
}
