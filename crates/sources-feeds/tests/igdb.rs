//! The IGDB answer parsers (ENR-igdb): golden shapes, strictness, caps and
//! property tests. Synthetic data only.

use lodestar_sources_feeds::igdb::{
    LIMITS, Limits, MAX_TITLE, Stop, TOKEN_LIMITS, parse_games, parse_token, token_ok, year_of,
};
use proptest::prelude::*;

const GAMES: &[u8] = br#"[
  {"id": 900001, "name": "Synthetic Quest", "first_release_date": 1431993600, "themes": [1, 17], "genres": [12, 31], "summary": "SENTINEL"},
  {"id": 900002, "name": "  Second\u200b Game  ", "first_release_date": -252460800, "themes": [42]},
  {"id": 900003, "name": "No Date"}
]"#;

#[test]
fn a_games_answer_is_read() {
    let s = parse_games(GAMES, &LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.page.kept, 3);
    let h = &s.hits[0];
    assert_eq!(
        (h.id, h.title.as_deref(), h.year),
        (900001, Some("Synthetic Quest"), Some(2015))
    );
    assert_eq!(h.themes.as_deref(), Some(&[1, 17][..]));
    assert_eq!(h.genres, [12, 31]);
    assert_eq!(h.adult(), None);
    let h = &s.hits[1];
    assert_eq!(
        (h.title.as_deref(), h.year),
        (Some("Second Game"), Some(1962))
    );
    assert_eq!(h.adult(), Some(true), "the Erotic theme");
    let h = &s.hits[2];
    assert_eq!((h.year, h.themes.as_ref(), h.adult()), (None, None, None));
    assert!(!format!("{s:?}").contains("SENTINEL"));
}

#[test]
fn errors_and_wrong_shapes_are_not_whole() {
    for bad in [
        &br#"{"message":"Authorization Failure"}"#[..],
        br#"[{"title":"Syntax Error","status":400,"cause":"x"}]"#,
        br#"[{"id":1,"name":"A"}] trailing"#,
        br#"[{"id":0,"name":"A"}]"#,
        br#"[{"id":-1,"name":"A"}]"#,
        br#"[{"id":1.5,"name":"A"}]"#,
        br#"[{"id":"1","name":"A"}]"#,
        br#"[{"id":1,"id":2,"name":"A"}]"#,
        br#"[{"id":1,"na\u006de":"A"}]"#,
        br#"[{"id":1,"name":5}]"#,
        br#"[{"id":1,"first_release_date":"2015"}]"#,
        br#"[{"id":1,"first_release_date":1e9}]"#,
        br#"[{"id":1,"themes":[4294967296]}]"#,
        br#"[{"id":1,"themes":{}}]"#,
        br#"[1]"#,
        br#"[{"id":1,"name":"A"#,
        b"",
    ] {
        let s = parse_games(bad, &LIMITS);
        assert!(!s.page.whole(), "{}", String::from_utf8_lossy(bad));
    }
    assert!(parse_games(b"[]", &LIMITS).page.whole());
}

#[test]
fn caps_hold() {
    let many: String = format!(
        "[{}]",
        (1..=21)
            .map(|i| format!(r#"{{"id":{i}}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let s = parse_games(many.as_bytes(), &LIMITS);
    assert_eq!(s.page.stop, Some(Stop::Items));
    assert!(!s.page.whole());

    let ids: String = (0..40).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let body = format!(r#"[{{"id":1,"themes":[{ids}],"genres":[{ids}]}}]"#);
    let s = parse_games(body.as_bytes(), &LIMITS);
    assert!(s.page.whole());
    assert_eq!(s.hits[0].themes.as_ref().unwrap().len(), LIMITS.max_ids);
    assert!(s.hits[0].truncated);

    let long = format!(
        r#"[{{"id":1,"name":"{}"}}]"#,
        "a".repeat(LIMITS.max_text + 1)
    );
    assert_eq!(
        parse_games(long.as_bytes(), &LIMITS).page.skipped.over_cap,
        1
    );

    let deep = format!("[{}{}]", "[".repeat(100), "]".repeat(100));
    assert_eq!(
        parse_games(deep.as_bytes(), &LIMITS).page.stop,
        Some(Stop::Depth)
    );

    let big = vec![b' '; LIMITS.max_bytes + 1];
    assert_eq!(parse_games(&big, &LIMITS).page.stop, Some(Stop::Bytes));
}

#[test]
fn a_token_answer_is_read() {
    let t = parse_token(
        br#"{"access_token":"abcdefghijklmnopqrstuvwxyz0123","expires_in":5011271,"token_type":"bearer"}"#,
        &TOKEN_LIMITS,
    )
    .unwrap();
    assert_eq!(t.expires_in, 5_011_271);
    assert!(token_ok(&t.access_token));
    assert!(!format!("{t:?}").contains("abcdef"));
}

fn tight() -> Limits {
    Limits {
        max_bytes: 4096,
        max_depth: 4,
        max_values: 200,
        max_array: 8,
        max_text: 64,
        max_items: 4,
        max_ids: 2,
    }
}

proptest! {
    #[test]
    fn games_never_panic_and_keep_clean_hits(body in proptest::collection::vec(any::<u8>(), 0..2048)) {
        for l in [LIMITS, tight()] {
            let s = parse_games(&body, &l);
            prop_assert!(s.hits.len() <= l.max_items);
            for h in &s.hits {
                prop_assert!(h.id > 0);
                prop_assert!(h.genres.len() <= l.max_ids);
                prop_assert!(h.themes.as_ref().is_none_or(|t| t.len() <= l.max_ids));
                prop_assert!(h.year.is_none_or(|y| (1..=9999).contains(&y)));
                if let Some(t) = &h.title {
                    prop_assert!(!t.is_empty() && t.len() <= MAX_TITLE);
                    prop_assert!(!t.chars().any(char::is_control));
                }
            }
            let _ = parse_token(&body, &TOKEN_LIMITS);
        }
    }

    #[test]
    fn shaped_games_round_trip(
        id in 1u64..,
        name in "[A-Za-z0-9 :!'-]{1,40}",
        secs in -60_000_000_000i64..200_000_000_000,
        themes in proptest::collection::vec(0u32..100, 0..5),
    ) {
        let t: Vec<String> = themes.iter().map(u32::to_string).collect();
        let body = format!(
            r#"[{{"id":{id},"name":"{name}","first_release_date":{secs},"themes":[{}]}}]"#,
            t.join(",")
        );
        let s = parse_games(body.as_bytes(), &LIMITS);
        prop_assert!(s.page.whole());
        let h = &s.hits[0];
        prop_assert_eq!(h.id, id);
        prop_assert_eq!(h.year, year_of(secs));
        prop_assert_eq!(h.adult() == Some(true), themes.contains(&42));
    }

    #[test]
    fn tokens_kept_are_header_safe(tok in "[A-Za-z0-9 ._:;=\t-]{0,40}", n in 0u64..u64::MAX) {
        let body = format!(r#"{{"access_token":"{tok}","expires_in":{n}}}"#);
        if let Some(t) = parse_token(body.as_bytes(), &TOKEN_LIMITS) {
            prop_assert!(token_ok(&t.access_token));
            prop_assert!(t.expires_in > 0);
        }
    }
}
