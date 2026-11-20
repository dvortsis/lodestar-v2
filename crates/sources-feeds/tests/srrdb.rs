//! The srrDB parsers (ENR-srrdb): the shapes they read, their caps, and
//! that nothing they're handed (garbage, cut-short, oversize or mutated
//! answers) panics or keeps more than the caps allow. The corpus is the
//! fuzz target's seeds, `fuzz/seeds/srrdb/`.

use lodestar_sources_feeds::srrdb::{
    DETAILS_LIMITS, Details, Limits, MAX_RELEASE, SEARCH_LIMITS, Search, Stop, parse_details,
    parse_search,
};
use proptest::prelude::*;

const SEEDS: &[(&str, &[u8])] = &[
    (
        "search.json",
        include_bytes!("../../../fuzz/seeds/srrdb/search.json"),
    ),
    (
        "candidates.json",
        include_bytes!("../../../fuzz/seeds/srrdb/candidates.json"),
    ),
    (
        "empty.json",
        include_bytes!("../../../fuzz/seeds/srrdb/empty.json"),
    ),
    (
        "error.json",
        include_bytes!("../../../fuzz/seeds/srrdb/error.json"),
    ),
    (
        "details.json",
        include_bytes!("../../../fuzz/seeds/srrdb/details.json"),
    ),
];

fn seed(name: &str) -> &'static [u8] {
    SEEDS.iter().find(|(n, _)| *n == name).unwrap().1
}

fn parse(body: &str) -> Search {
    parse_search(body.as_bytes(), &SEARCH_LIMITS)
}

fn details(body: &str) -> Details {
    parse_details(body.as_bytes(), &DETAILS_LIMITS)
}

/// One result's JSON with `extra` fields after its name.
fn one(extra: &str) -> String {
    format!(r#"{{"results":[{{"release":"A.Film.1999-GRP"{extra}}}],"resultsCount":1}}"#)
}

#[test]
fn the_seed_answers_read_whole() {
    let s = parse_search(seed("search.json"), &SEARCH_LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!((s.hits.len(), s.count), (6, Some(6)));
    let h = &s.hits[0];
    assert_eq!(h.release, "Harbour.Lights.2004.1080p.BluRay.x264-QUiETGRP");
    assert_eq!(h.imdb.as_ref().map(|i| i.as_str()), Some("tt0999901"));
    assert_eq!(h.size, Some(8_603_626_447));
    assert_eq!(s.hits[2].imdb, None);
    assert!(!format!("{s:?}").contains("SENTINEL"));

    let c = parse_search(seed("candidates.json"), &SEARCH_LIMITS);
    assert!(c.page.whole() && c.hits.len() == 4);
    assert_eq!(
        c.hits[3].imdb.as_ref().map(|i| i.as_str()),
        Some("tt10888803")
    );

    let e = parse_search(seed("empty.json"), &SEARCH_LIMITS);
    assert!(e.page.whole() && e.hits.is_empty());

    let err = parse_search(seed("error.json"), &SEARCH_LIMITS);
    assert!(err.page.error && !err.page.whole());

    let d = parse_details(seed("details.json"), &DETAILS_LIMITS);
    assert!(d.page.whole(), "{:?}", d.page);
    assert_eq!(
        d.name.as_deref(),
        Some("Harbour.Lights.2004.1080p.BluRay.x264-QUiETGRP")
    );
    assert_eq!(d.files.len(), 3);
    assert_eq!((d.files[0].size, d.files[0].crc), (10_458, 0x0452_3E9B));
    assert_eq!(d.files[1].crc, 0xA1B2_C3D4);
    assert_eq!(d.archived.len(), 1);
    assert_eq!(
        (d.archived[0].size, d.archived[0].crc),
        (8_551_197_647, 0xDEAD_BEEF)
    );
    assert!(!format!("{d:?}").contains("SENTINEL"));
}

#[test]
fn only_the_asked_shape_is_whole() {
    // Read keys repeated or escaped, wrong types, bad ids: not whole.
    for bad in [
        one(r#","release":"B.Film.2000-GRP""#),
        one(r#","imdbId":"0133093","imdbId":"0133093""#),
        one(r#","imdbId":"133093""#),
        one(r#","imdbId":"0000000""#),
        one(r#","imdbId":7"#),
        one(r#","size":"12""#),
        one(r#","size":-1"#),
        one(r#","size":1.5"#),
        one(r#","size":99999999999999999999"#),
        r#"{"results":[{"imdbId":null}]}"#.to_owned(),
        r#"{"results":[{"release":""}]}"#.to_owned(),
        r#"{"results":[{"release":7}]}"#.to_owned(),
        r#"{"results":[7]}"#.to_owned(),
        r#"{"results":{}}"#.to_owned(),
        r#"{"results":[],"results":[]}"#.to_owned(),
        "{\"res\\u0075lts\":[]}".to_owned(),
        r#"{"results":[]} x"#.to_owned(),
        r#"{"results":["#.to_owned(),
        r#"[]"#.to_owned(),
        r#"{}"#.to_owned(),
        String::new(),
        "<html></html>".to_owned(),
    ] {
        let s = parse(&bad);
        assert!(!s.page.whole(), "{bad}: {:?}", s.page);
    }
    // Unknown keys, repeated or not, and null or empty ids are fine.
    for good in [
        one(r#","x":1,"x":2,"date":"2011","hasNFO":"yes""#),
        one(r#","imdbId":null,"size":null"#),
        one(r#","imdbId":"""#),
        one(r#","imdbId":"tt0133093","size":0"#),
    ] {
        let s = parse(&good);
        assert!(s.page.whole(), "{good}: {:?}", s.page);
        assert_eq!(s.hits.len(), 1);
    }
    // A name over MAX_RELEASE spoils its result.
    let long = format!(
        r#"{{"results":[{{"release":"{}"}}]}}"#,
        "a".repeat(MAX_RELEASE + 1)
    );
    assert_eq!(parse(&long).page.skipped.over_cap, 1);

    for bad in [
        r#"{"name":"R","files":[{"name":"a","size":1,"crc":"0452E9B"}]}"#,
        r#"{"name":"R","files":[{"name":"a","size":1,"crc":"04523E9G"}]}"#,
        r#"{"name":"R","files":[{"name":"a","crc":"04523E9B"}]}"#,
        r#"{"name":"R","files":[{"size":1,"crc":"04523E9B"}]}"#,
        r#"{"name":"R","files":[{"name":"a","size":1,"crc":"04523E9B","crc":"04523E9B"}]}"#,
        r#"{"name":"R","files":{}}"#,
        r#"{"name":"","files":[]}"#,
        r#"{"name":"R","name":"R"}"#,
        r#"{"files":[]}"#,
    ] {
        let d = details(bad);
        assert!(!d.page.whole(), "{bad}: {:?}", d.page);
    }
    let d = details(r#"{"name":"R","files":[],"archived-files":[],"adds":[{"x":1}]}"#);
    assert!(d.page.whole(), "{:?}", d.page);
}

#[test]
fn the_caps_hold() {
    let rows = |n: usize| {
        let r: Vec<String> = (0..n)
            .map(|i| format!(r#"{{"release":"F.{i}.1999-G","imdbId":null}}"#))
            .collect();
        format!(r#"{{"results":[{}]}}"#, r.join(","))
    };
    let l = Limits {
        max_items: 3,
        ..SEARCH_LIMITS
    };
    let s = parse_search(rows(3).as_bytes(), &l);
    assert!(s.page.whole() && s.hits.len() == 3);
    let s = parse_search(rows(4).as_bytes(), &l);
    assert_eq!((s.page.stop, s.hits.len()), (Some(Stop::Items), 3));
    assert!(!s.page.whole());
    // Bytes.
    let body = rows(10);
    let l = Limits {
        max_bytes: body.len() - 1,
        ..SEARCH_LIMITS
    };
    assert!(!parse_search(body.as_bytes(), &l).page.whole());
    // Depth.
    let deep = format!(
        r#"{{"results":[{{"release":"F.1999-G","x":{}1{}}}]}}"#,
        "[".repeat(20),
        "]".repeat(20)
    );
    assert_eq!(parse(&deep).page.stop, Some(Stop::Depth));
    // A text over max_text.
    let l = Limits {
        max_text: 8,
        ..SEARCH_LIMITS
    };
    let s = parse_search(one("").as_bytes(), &l);
    assert_eq!(s.page.skipped.over_cap, 1);
    // Details' file lists each stop at max_items.
    let files: Vec<String> = (0..5)
        .map(|i| format!(r#"{{"name":"f{i}","size":1,"crc":"00000000"}}"#))
        .collect();
    let body = format!(r#"{{"name":"R","files":[{}]}}"#, files.join(","));
    let l = Limits {
        max_items: 4,
        ..DETAILS_LIMITS
    };
    let d = parse_details(body.as_bytes(), &l);
    assert_eq!((d.page.stop, d.files.len()), (Some(Stop::Items), 4));
}

#[test]
fn many_unknown_keys_are_read_in_linear_time() {
    let mut body = String::from(r#"{"results":[{"release":"F.1999-G""#);
    for i in 0..300_000 {
        body.push_str(&format!(r#","k{i}":0"#));
    }
    body.push_str("}]");
    for i in 0..300_000 {
        body.push_str(&format!(r#","r{i}":0"#));
    }
    body.push('}');
    let l = Limits {
        max_bytes: 64 * 1024 * 1024,
        max_values: 2_000_000,
        ..SEARCH_LIMITS
    };
    let started = std::time::Instant::now();
    let s = parse_search(body.as_bytes(), &l);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.hits.len(), 1);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

fn kept_ok(s: &Search, l: &Limits) -> Result<(), TestCaseError> {
    prop_assert!(s.hits.len() <= l.max_items);
    for h in &s.hits {
        prop_assert!(!h.release.is_empty() && h.release.len() <= MAX_RELEASE);
        if let Some(i) = &h.imdb {
            prop_assert!(i.as_str().starts_with("tt") && (9..=12).contains(&i.as_str().len()));
        }
    }
    Ok(())
}

fn details_ok(d: &Details, l: &Limits) -> Result<(), TestCaseError> {
    prop_assert!(d.files.len() <= l.max_items && d.archived.len() <= l.max_items);
    for f in d.files.iter().chain(&d.archived) {
        prop_assert!(!f.name.is_empty() && f.name.len() <= MAX_RELEASE);
    }
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        kept_ok(&parse_search(&body, &SEARCH_LIMITS), &SEARCH_LIMITS)?;
        details_ok(&parse_details(&body, &DETAILS_LIMITS), &DETAILS_LIMITS)?;
    }

    #[test]
    fn mutated_seeds_never_panic(
        which in 0..SEEDS.len(),
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
        cut in any::<usize>(),
    ) {
        let mut body = SEEDS[which].1.to_vec();
        for (at, b) in edits {
            let at = at % body.len();
            body[at] = b;
        }
        body.truncate(cut % (body.len() + 1));
        kept_ok(&parse_search(&body, &SEARCH_LIMITS), &SEARCH_LIMITS)?;
        details_ok(&parse_details(&body, &DETAILS_LIMITS), &DETAILS_LIMITS)?;
    }

    #[test]
    fn well_formed_results_read_back(
        names in proptest::collection::vec("[A-Za-z0-9._-]{1,60}", 0..20),
        ids in proptest::collection::vec(proptest::option::of(1_000_000u32..99_999_999), 20),
        sizes in proptest::collection::vec(any::<u64>(), 20),
    ) {
        let rows: Vec<String> = names.iter().enumerate().map(|(i, n)| {
            let id = ids[i].map_or("null".to_owned(), |d| format!("\"{d:07}\""));
            format!(r#"{{"release":"{n}","imdbId":{id},"size":{}}}"#, sizes[i] >> 1)
        }).collect();
        let body = format!(r#"{{"results":[{}],"resultsCount":{}}}"#, rows.join(","), rows.len());
        let s = parse(&body);
        prop_assert!(s.page.whole(), "{:?}", s.page);
        prop_assert_eq!(s.hits.len(), names.len());
        for (i, h) in s.hits.iter().enumerate() {
            prop_assert_eq!(&h.release, &names[i]);
            prop_assert_eq!(h.size, Some(sizes[i] >> 1));
            let want = ids[i].map(|d| format!("tt{d:07}"));
            prop_assert_eq!(h.imdb.as_ref().map(|x| x.as_str().to_owned()), want);
        }
    }
}
