//! The TheTVDB parser (ENR-tvdb): the shapes it reads, its caps, and that
//! nothing it's handed (garbage, cut-short, oversize or mutated answers)
//! panics or keeps more than the caps allow. The corpus is the fuzz
//! target's seeds, `fuzz/seeds/tvdb/`.

use lodestar_sources_feeds::tvdb::{
    LOGIN_LIMITS, Limits, MAX_LANGUAGE, MAX_TITLE, MAX_TOKEN, SEARCH_LIMITS, Search, Stop, Type,
    parse_login, parse_search,
};
use proptest::prelude::*;

const SEEDS: &[(&str, &[u8])] = &[
    (
        "search.json",
        include_bytes!("../../../fuzz/seeds/tvdb/search.json"),
    ),
    (
        "login.json",
        include_bytes!("../../../fuzz/seeds/tvdb/login.json"),
    ),
    (
        "failure.json",
        include_bytes!("../../../fuzz/seeds/tvdb/failure.json"),
    ),
    (
        "null.json",
        include_bytes!("../../../fuzz/seeds/tvdb/null.json"),
    ),
];

fn seed(name: &str) -> &'static [u8] {
    SEEDS.iter().find(|(n, _)| *n == name).unwrap().1
}

/// A search answer holding the results given.
fn answer(results: &str) -> String {
    format!(r#"{{"status":"success","data":[{results}],"links":{{}}}}"#)
}

/// A backslash, for keys written with an escape.
const BS: char = '\\';

/// A distinct run of letters for each `i`: a valid language key.
fn letters(mut i: usize) -> String {
    let mut out = String::from("x");
    loop {
        out.push(char::from(b'a' + (i % 26) as u8));
        i /= 26;
        if i == 0 {
            return out;
        }
    }
}

fn search(body: &str) -> Search {
    parse_search(body.as_bytes(), &SEARCH_LIMITS)
}

#[test]
fn the_seed_answers_read_whole() {
    let s = parse_search(seed("search.json"), &SEARCH_LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    let ids: Vec<(&str, Type)> = s
        .results
        .iter()
        .map(|r| (r.tvdb_id.as_str(), r.kind))
        .collect();
    assert_eq!(
        ids,
        [
            ("900002", Type::Series),
            ("900001", Type::Movie),
            ("81189", Type::Series)
        ]
    );
    let bb = &s.results[2];
    assert_eq!(bb.name.as_deref(), Some("Breaking Bad"));
    assert_eq!(bb.aliases, ["Breaking Bad (2008)", "Во все тяжкие"]);
    let tr: Vec<(&str, &str)> = bb
        .translations
        .iter()
        .map(|t| (t.language.as_str(), t.value.as_str()))
        .collect();
    assert_eq!(
        tr,
        [
            ("eng", "Breaking Bad"),
            ("deu", "Breaking Bad – Die Serie"),
            ("jpn", "ブレイキング・バッド"),
            ("fra", "Breaking Bad"),
        ]
    );
    assert_eq!(bb.primary_language.as_deref(), Some("eng"));
    assert_eq!(bb.years, [2008, 2008]);
    assert_eq!(bb.genres, ["Drama", "Crime", "Thriller"]);
    // The IMDb title and the TMDB id; never EIDR or an IMDb person.
    assert_eq!(bb.imdb, ["tt0903747"]);
    assert_eq!(bb.tmdb, ["1396"]);
    assert!(!format!("{s:?}").contains("SENTINEL"));
    // `null` for the optional fields reads as empty.
    let other = &s.results[0];
    assert!(other.translations.is_empty() && other.imdb.is_empty() && other.genres.is_empty());
    assert_eq!(other.years, [2009]);

    let l = parse_login(seed("login.json"), &LOGIN_LIMITS);
    assert!(l.page.whole(), "{:?}", l.page);
    assert!(l.token.is_some());
    assert_eq!(format!("{:?}", l.token), "Some(Token(len 14))");
    assert_eq!(l.token.unwrap().into_inner(), "fake.token.one");
}

#[test]
fn no_result_is_whole_and_empty() {
    for body in [seed("null.json"), br#"{"status":"success","data":[]}"#] {
        let s = parse_search(body, &SEARCH_LIMITS);
        assert!(s.page.whole(), "{:?}", s.page);
        assert!(s.results.is_empty());
    }
}

#[test]
fn only_the_asked_shape_is_whole() {
    let f = parse_search(seed("failure.json"), &SEARCH_LIMITS);
    assert!(f.page.failed && !f.page.whole());
    for body in [
        "",
        "[]",
        "null",
        r#"{"data":[]}"#,
        r#"{"status":"success"}"#,
        r#"{"status":"success","data":{}}"#,
        r#"{"status":"success","data":"x"}"#,
        r#"{"status":1,"data":[]}"#,
        r#"{"status":"success","data":[]} x"#,
        r#"{"status":"success","status":"success","data":[]}"#,
        r#"{"status":"success","data":[],"data":[]}"#,
        &format!(r#"{{"status":"success","d{BS}u0061ta":[]}}"#),
        "<html>SENTINEL</html>",
    ] {
        assert!(!search(body).page.whole(), "{body}");
    }
    // A login answer is a token in an object, and nothing else.
    for body in [
        r#"{"status":"success","data":[]}"#,
        r#"{"status":"success","data":{}}"#,
        r#"{"status":"success","data":{"token":""}}"#,
        r#"{"status":"success","data":{"token":7}}"#,
        r#"{"status":"success","data":{"token":"a","token":"b"}}"#,
        r#"{"status":"success","data":{"token":"a b"}}"#,
        r#"{"status":"success","data":{"token":"a\r\nX: y"}}"#,
        r#"{"status":"failure","data":{"token":"abc"}}"#,
    ] {
        let l = parse_login(body.as_bytes(), &LOGIN_LIMITS);
        assert!(!l.page.whole() || l.token.is_none(), "{body}");
        assert!(l.token.is_none() || !l.page.whole(), "{body}");
    }
}

#[test]
fn results_outside_the_shape_are_skipped() {
    for (r, bad_id) in [
        (r#"{"type":"series","name":"A"}"#, true),
        (r#"{"tvdb_id":"0123","type":"series"}"#, true),
        (r#"{"tvdb_id":81189,"type":"series"}"#, false),
        (r#"{"tvdb_id":"1","aliases":"A"}"#, false),
        (r#"{"tvdb_id":"1","translations":["A"]}"#, false),
        (r#"{"tvdb_id":"1","translations":{"e":"A"}}"#, false),
        (r#"{"tvdb_id":"1","year":"20O8"}"#, false),
        (r#"{"tvdb_id":"1","first_air_time":"2008"}"#, false),
        (r#"{"tvdb_id":"1","primary_language":"en_US"}"#, false),
        (
            r#"{"tvdb_id":"1","remote_ids":[{"id":"tt0903747"}]}"#,
            false,
        ),
        (r#"{"tvdb_id":"1","remote_ids":[7]}"#, false),
        (r#"{"tvdb_id":"1","genres":[1]}"#, false),
        ("7", false),
    ] {
        let s = search(&answer(r));
        assert!(!s.page.whole() && s.results.is_empty(), "{r}");
        assert_eq!(s.page.skipped.bad_id > 0, bad_id, "{r}");
    }
    // Empty year and date, an unknown type and unknown remote sources
    // are fine.
    let s = search(&answer(
        r#"{"tvdb_id":"1","type":"person","year":"","first_air_time":"","remote_ids":[{"id":"x","sourceName":"EIDR"},{"id":"nm1","sourceName":"IMDB"}]}"#,
    ));
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.results[0].kind, Type::Other);
    assert!(s.results[0].years.is_empty() && s.results[0].imdb.is_empty());
}

#[test]
fn the_count_caps_hold() {
    let one = r#"{"tvdb_id":"1","type":"series","name":"A","aliases":["B","C"],"translations":{"eng":"D","deu":"E"},"genres":["G1","G2","G3"],"remote_ids":[{"id":"tt0000001","sourceName":"IMDB"},{"id":"2","sourceName":"TheMovieDB.com"},{"id":"tt0000003","sourceName":"IMDB"}]}"#;
    let tight = Limits {
        max_texts: 3,
        max_ids: 2,
        max_genres: 1,
        ..SEARCH_LIMITS
    };
    let s = parse_search(answer(one).as_bytes(), &tight);
    assert!(s.page.whole(), "{:?}", s.page);
    let r = &s.results[0];
    assert_eq!(r.titles().count(), 3);
    assert_eq!((r.imdb.len() + r.tmdb.len(), r.genres.len()), (2, 1));
    assert!(r.truncated);
    // More results than `max_items`: the rest are read past and the
    // answer isn't whole.
    let many = [one; SEARCH_LIMITS.max_items + 1].join(",");
    let s = search(&answer(&many));
    assert_eq!(s.results.len(), SEARCH_LIMITS.max_items);
    assert_eq!(s.page.stop, Some(Stop::Items));
    assert!(!s.page.whole());
}

#[test]
fn byte_depth_and_text_caps_stop_or_skip() {
    let mut big = answer(r#"{"tvdb_id":"1"}"#).into_bytes();
    let close = big.pop().unwrap();
    big.resize(SEARCH_LIMITS.max_bytes + 1, b' ');
    big.push(close);
    let s = parse_search(&big, &SEARCH_LIMITS);
    assert_eq!(s.page.stop, Some(Stop::Bytes));
    assert!(!s.page.whole());

    let deep = format!(
        r#"{{"status":"success","data":[{{"tvdb_id":"1","x":{}1{}}}]}}"#,
        "[".repeat(64),
        "]".repeat(64)
    );
    assert_eq!(search(&deep).page.stop, Some(Stop::Depth));

    let long = format!(
        r#"{{"tvdb_id":"1","name":"{}"}}"#,
        "a".repeat(SEARCH_LIMITS.max_text + 1)
    );
    let s = search(&answer(&long));
    assert_eq!(s.page.skipped.over_cap, 1);
    assert!(!s.page.whole());

    // A token over the cap is no token.
    let t = format!(
        r#"{{"status":"success","data":{{"token":"{}"}}}}"#,
        "a".repeat(MAX_TOKEN + 1)
    );
    let l = parse_login(t.as_bytes(), &LOGIN_LIMITS);
    assert!(l.token.is_none() && !l.page.whole());
}

#[test]
fn many_unknown_keys_are_read_in_linear_time() {
    let n = 300_000;
    let keys: String = (0..n).map(|i| format!(r#""k{i}":0,"#)).collect();
    let body = format!(
        r#"{{{keys}"status":"success","data":[{{{keys}"tvdb_id":"5","translations":{{{}"eng":"A"}}}}]}}"#,
        (0..1000)
            .map(|i| format!(r#""{}":"t","#, letters(i)))
            .collect::<String>()
    );
    let big = Limits {
        max_bytes: 16 * 1024 * 1024,
        max_values: 2_000_000,
        ..SEARCH_LIMITS
    };
    let started = std::time::Instant::now();
    let s = parse_search(body.as_bytes(), &big);
    // A generous bound on the slowest debug runner.
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    // Past `max_texts`, translations are passed over unchecked.
    assert!(s.page.whole(), "{:?}", s.page);
    assert!(s.results[0].truncated);
}

#[test]
fn repeats_of_what_is_read_spoil_it() {
    for r in [
        r#"{"tvdb_id":"1","tvdb_id":"2"}"#,
        r#"{"tvdb_id":"1","name":"A","name":"B"}"#,
        r#"{"tvdb_id":"1","remote_ids":[],"remote_ids":[]}"#,
        r#"{"tvdb_id":"1","translations":{"eng":"A","eng":"B"}}"#,
        r#"{"tvdb_id":"1","translations":{"eng":"A","ENG":"B"}}"#,
        r#"{"tvdb_id":"1","remote_ids":[{"id":"1","id":"2","sourceName":"TheMovieDB.com"}]}"#,
        &format!(r#"{{"tvdb_id":"1","t{BS}u0079pe":"series"}}"#),
    ] {
        assert!(!search(&answer(r)).page.whole(), "{r}");
    }
    // Repeats of keys that aren't read are passed over.
    let ok = answer(r#"{"tvdb_id":"1","overview":"x","overview":"y"}"#);
    assert!(search(&ok).page.whole());
}

#[test]
fn arrays_read_are_capped_too() {
    let tight = Limits {
        max_array: 2,
        ..SEARCH_LIMITS
    };
    let s = parse_search(
        answer(r#"{"tvdb_id":"1","aliases":["a","b","c"]}"#).as_bytes(),
        &tight,
    );
    assert_eq!(s.page.skipped.over_cap, 1);
}

fn kept_ok(s: &Search, l: &Limits) -> Result<(), TestCaseError> {
    prop_assert!(s.results.len() <= l.max_items);
    for r in &s.results {
        prop_assert!(!r.tvdb_id.is_empty() && r.tvdb_id.len() <= 20);
        prop_assert!(r.tvdb_id.bytes().all(|b| b.is_ascii_digit()) && !r.tvdb_id.starts_with('0'));
        prop_assert!(r.titles().count() <= l.max_texts);
        prop_assert!(r.genres.len() <= l.max_genres);
        prop_assert!(r.imdb.len() + r.tmdb.len() <= l.max_ids);
        for t in r.titles() {
            prop_assert!(!t.is_empty() && t.len() <= MAX_TITLE && t.trim() == t);
        }
        for t in &r.translations {
            prop_assert!((2..=MAX_LANGUAGE).contains(&t.language.len()));
        }
        prop_assert!(r.imdb.iter().all(|s| s.starts_with("tt")));
        prop_assert!(r.years.iter().all(|y| (1..=9999).contains(y)));
    }
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        kept_ok(&parse_search(&body, &SEARCH_LIMITS), &SEARCH_LIMITS)?;
        let l = parse_login(&body, &LOGIN_LIMITS);
        prop_assert!(l.token.is_none() || l.page.whole());
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
        let l = parse_login(&body, &LOGIN_LIMITS);
        prop_assert!(l.token.is_none() || l.page.whole());
    }
}
