//! The TMDB parser (ENR-tmdb): the shapes it reads, its caps, and that
//! nothing it's handed (garbage, cut-short, oversize or mutated answers)
//! panics or keeps more than the caps allow. The corpus is the fuzz
//! target's seeds, `fuzz/seeds/tmdb/`.

use lodestar_sources_feeds::tmdb::{
    Form, LIMITS, Limits, MAX_LANGUAGE, MAX_TITLE, Search, Stop, parse_search,
};
use proptest::prelude::*;

const SEEDS: &[(&str, &[u8])] = &[
    (
        "movie.json",
        include_bytes!("../../../fuzz/seeds/tmdb/movie.json"),
    ),
    (
        "tv.json",
        include_bytes!("../../../fuzz/seeds/tmdb/tv.json"),
    ),
    (
        "error.json",
        include_bytes!("../../../fuzz/seeds/tmdb/error.json"),
    ),
];

fn seed(name: &str) -> &'static [u8] {
    SEEDS.iter().find(|(n, _)| *n == name).unwrap().1
}

fn movie(body: &str) -> Search {
    parse_search(body.as_bytes(), Form::Movie, &LIMITS)
}

/// One film result with `extra` fields added after the usual ones.
fn one(extra: &str) -> String {
    format!(
        r#"{{"page":1,"results":[{{"id":7,"title":"Film","original_title":"Film","original_language":"en","release_date":"2001-02-03","genre_ids":[18],"adult":false{extra}}}],"total_results":1}}"#
    )
}

#[test]
fn a_film_answer_is_read() {
    let s = parse_search(seed("movie.json"), Form::Movie, &LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.page.kept, 2);
    let h = &s.hits[0];
    assert_eq!(h.id, 278);
    assert_eq!(h.title.as_deref(), Some("The Shawshank Redemption"));
    assert_eq!(h.original_language.as_deref(), Some("en"));
    assert_eq!(
        (h.year, h.genre_ids.as_slice(), h.adult),
        (Some(1994), &[18, 80][..], Some(false))
    );
    assert!(
        !format!("{s:?}").contains("SENTINEL"),
        "nothing else is kept"
    );
}

#[test]
fn a_series_answer_is_read_by_its_own_keys() {
    let s = parse_search(seed("tv.json"), Form::Tv, &LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    let h = &s.hits[0];
    assert_eq!(h.id, 71446);
    assert_eq!(h.title.as_deref(), Some("Money Heist"));
    assert_eq!(h.original_title.as_deref(), Some("La casa de papel"));
    assert_eq!(
        (h.original_language.as_deref(), h.year),
        (Some("es"), Some(2017))
    );
    // Read as a film answer, a series has no title: kept, but unnamed.
    let s = parse_search(seed("tv.json"), Form::Movie, &LIMITS);
    assert!(s.page.whole());
    assert_eq!(s.hits[0].title, None);
}

#[test]
fn an_error_object_and_other_shapes_are_not_whole() {
    let e = parse_search(seed("error.json"), Form::Movie, &LIMITS);
    assert!(e.page.error && !e.page.whole());
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
    ] {
        assert!(!movie(body).page.whole(), "{body}");
    }
    // An empty list is a whole answer: a miss.
    let s = movie(r#"{"page":1,"results":[],"total_pages":0}"#);
    assert!(s.page.whole() && s.hits.is_empty());
}

#[test]
fn the_adult_marker_is_read_as_given() {
    let without = one("").replace(r#","adult":false"#, "");
    let s = movie(&without);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.hits[0].adult, None, "no key, no marker");
    assert_eq!(movie(&one("")).hits[0].adult, Some(false));
    let marked = one("").replace(r#""adult":false"#, r#""adult":true"#);
    assert_eq!(movie(&marked).hits[0].adult, Some(true));
}

#[test]
fn a_wrong_or_repeated_field_spoils_its_result() {
    assert!(movie(&one("")).page.whole());
    for extra in [
        r#","id":8"#,
        r#","title":"Again""#,
        r#","t\u0069tle":"Escaped""#,
        r#","release_date":"2001""#,
        r#","adult":"no""#,
        r#","genre_ids":[1.5]"#,
        r#","genre_ids":[-1]"#,
        r#","genre_ids":[4294967296]"#,
    ] {
        let s = movie(&one(extra));
        assert!(!s.page.whole() && s.hits.is_empty(), "{extra}");
    }
    for id in ["0", "-7", "7.0", "1e3", "\"7\"", "18446744073709551616"] {
        let s = movie(&format!(r#"{{"results":[{{"id":{id},"title":"Film"}}]}}"#));
        assert!(!s.page.whole() && s.hits.is_empty(), "{id}");
    }
    // A null date or language is none; `xx` is no language.
    let s =
        movie(r#"{"results":[{"id":9,"title":"F","release_date":null,"original_language":"xx"}]}"#);
    assert!(s.page.whole());
    assert_eq!(
        (s.hits[0].year, s.hits[0].original_language.as_deref()),
        (None, None)
    );
}

#[test]
fn the_caps_hold() {
    // Results past `max_items` stop the parse.
    let results: Vec<String> = (1..=LIMITS.max_items + 1)
        .map(|i| format!(r#"{{"id":{i},"title":"F"}}"#))
        .collect();
    let s = movie(&format!(r#"{{"results":[{}]}}"#, results.join(",")));
    assert_eq!(s.page.stop, Some(Stop::Items));
    assert!(s.hits.len() <= LIMITS.max_items && !s.page.whole());
    // Genre ids past `max_genres` are passed over; the result says so.
    let ids: Vec<String> = (1..=40).map(|i| i.to_string()).collect();
    let s = movie(&one(&format!(
        r#","x":0}},{{"id":3,"genre_ids":[{}]"#,
        ids.join(",")
    )));
    let h = s.hits.iter().find(|h| h.id == 3).unwrap();
    assert!(h.truncated && h.genre_ids.len() == LIMITS.max_genres);
    // A long title is cut to `MAX_TITLE`; one over `max_text` spoils.
    let long = "a".repeat(LIMITS.max_text);
    let s = movie(&format!(r#"{{"results":[{{"id":1,"title":"{long}"}}]}}"#));
    assert!(s.hits[0].title.as_ref().unwrap().len() <= MAX_TITLE);
    let longer = "a".repeat(LIMITS.max_text + 1);
    let s = movie(&format!(r#"{{"results":[{{"id":1,"title":"{longer}"}}]}}"#));
    assert_eq!(s.page.skipped.over_cap, 1);
    // Bytes past `max_bytes` stop it.
    let mut big = one("").into_bytes();
    let close = big.pop().unwrap();
    big.resize(LIMITS.max_bytes + 1, b' ');
    big.push(close);
    let s = parse_search(&big, Form::Movie, &LIMITS);
    assert_eq!(s.page.stop, Some(Stop::Bytes));
    // Depth.
    let deep = format!(
        r#"{{"results":[{{"id":1,"x":{}{}}}]}}"#,
        "[".repeat(20),
        "]".repeat(20)
    );
    assert_eq!(movie(&deep).page.stop, Some(Stop::Depth));
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
    let s = parse_search(body.as_bytes(), Form::Movie, &wide);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.hits.len(), 1);
    // Generous for the slowest CI runner in debug; quadratic is minutes.
    assert!(start.elapsed() < std::time::Duration::from_secs(20));
}

fn kept_ok(s: &Search, l: &Limits) -> Result<(), TestCaseError> {
    prop_assert!(s.hits.len() <= l.max_items);
    for h in &s.hits {
        prop_assert!(h.id > 0 && h.genre_ids.len() <= l.max_genres);
        for t in [&h.title, &h.original_title].into_iter().flatten() {
            prop_assert!(!t.is_empty() && t.len() <= MAX_TITLE);
        }
        if let Some(lang) = &h.original_language {
            prop_assert!(lang.len() <= MAX_LANGUAGE);
        }
        prop_assert!(h.year.is_none_or(|y| (1..=9999).contains(&y)));
    }
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        kept_ok(&parse_search(&body, Form::Movie, &LIMITS), &LIMITS)?;
        kept_ok(&parse_search(&body, Form::Tv, &LIMITS), &LIMITS)?;
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
        kept_ok(&parse_search(&body, Form::Movie, &LIMITS), &LIMITS)?;
        kept_ok(&parse_search(&body, Form::Tv, &LIMITS), &LIMITS)?;
    }

    /// A cut-short answer is never whole, so never a miss.
    #[test]
    fn a_cut_answer_is_never_whole(which in 0..2usize, cut in any::<usize>()) {
        let body = SEEDS[which].1;
        let trimmed = body.trim_ascii_end().len();
        let at = cut % trimmed;
        let form = if which == 0 { Form::Movie } else { Form::Tv };
        prop_assert!(!parse_search(&body[..at], form, &LIMITS).page.whole());
    }
}
