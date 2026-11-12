//! The Wikidata parser (ENR-14): the shapes it reads, its caps, and that
//! nothing it's handed (garbage, cut-short, oversize or mutated answers)
//! panics or keeps more than the caps allow. The corpus is the fuzz
//! target's seeds, `fuzz/seeds/wikidata/`.

use lodestar_sources_feeds::wikidata::{
    ENTITY_LIMITS, Limits, MAX_LANGUAGE, MAX_TITLE, Qid, SEARCH_LIMITS, Stop, parse_entities,
    parse_search,
};
use proptest::prelude::*;

const SEEDS: &[(&str, &[u8])] = &[
    (
        "search.json",
        include_bytes!("../../../fuzz/seeds/wikidata/search.json"),
    ),
    (
        "entities.json",
        include_bytes!("../../../fuzz/seeds/wikidata/entities.json"),
    ),
    (
        "missing.json",
        include_bytes!("../../../fuzz/seeds/wikidata/missing.json"),
    ),
    (
        "error.json",
        include_bytes!("../../../fuzz/seeds/wikidata/error.json"),
    ),
];

fn seed(name: &str) -> &'static [u8] {
    SEEDS.iter().find(|(n, _)| *n == name).unwrap().1
}

/// One entity's JSON with `claims` as given.
fn entity(id: &str, claims: &str) -> String {
    format!(
        r#"{{"entities":{{"{id}":{{"type":"item","id":"{id}","labels":{{"en":{{"language":"en","value":"Film"}}}},"claims":{claims}}}}}}}"#
    )
}

fn time(t: &str, precision: u8) -> String {
    format!(
        r#"{{"mainsnak":{{"snaktype":"value","property":"P577","datavalue":{{"value":{{"time":"{t}","precision":{precision}}},"type":"time"}}}},"rank":"normal"}}"#
    )
}

#[test]
fn the_seed_answers_read_whole() {
    let s = parse_search(seed("search.json"), &SEARCH_LIMITS);
    assert!(s.page.whole(), "{:?}", s.page);
    assert_eq!(s.ids, [Qid(172_241), Qid(900_002)]);

    let e = parse_entities(seed("entities.json"), &ENTITY_LIMITS);
    assert!(e.page.whole(), "{:?}", e.page);
    let film = &e.entities[0];
    assert_eq!(film.id, Qid(172_241));
    assert_eq!(film.imdb, ["tt0111161"]);
    assert_eq!(film.tmdb_movie, ["278"]);
    assert_eq!(film.instance_of, [Qid(11_424)]);
    // The deprecated 1993 date is left out.
    assert_eq!(film.years, [1994, 1994]);
    assert!(!film.truncated);
    let all = format!("{e:?}");
    assert!(!all.contains("SENTINEL"), "{all}");

    let m = parse_entities(seed("missing.json"), &ENTITY_LIMITS);
    assert!(m.page.whole() && m.entities.is_empty());
    assert_eq!(m.page.missing, 1);

    let err = parse_entities(seed("error.json"), &ENTITY_LIMITS);
    assert!(err.page.error && !err.page.whole());
}

#[test]
fn only_the_asked_shape_is_whole() {
    for body in [
        &b""[..],
        b"null",
        b"[]",
        b"{}",
        br#"{"search":{}}"#,
        br#"{"search":[],"search":[]}"#,
        br#"{"se\u0061rch":[]}"#,
        br#"{"search":[]} x"#,
        br#"{"search":[{"id":"Q1"},]}"#,
        br#"{"search":[{"id":"Q01"}]}"#,
        br#"{"search":[{"id":"P31"}]}"#,
        br#"{"search":[{"id":1}]}"#,
        br#"{"search":[{"id":"Q1","id":"Q2"}]}"#,
        br#"{"search":[{"i\u0064":"Q1"}]}"#,
        br#"{"search":["Q1"]}"#,
        br#"{"search":[{"id":"Q18446744073709551616"}]}"#,
    ] {
        let s = parse_search(body, &SEARCH_LIMITS);
        assert!(!s.page.whole(), "{}", String::from_utf8_lossy(body));
    }
    let s = parse_search(br#"{"search":[],"success":1}"#, &SEARCH_LIMITS);
    assert!(s.page.whole() && s.ids.is_empty());
}

#[test]
fn entities_outside_the_shape_are_skipped() {
    let ok = |claims: &str| parse_entities(entity("Q5", claims).as_bytes(), &ENTITY_LIMITS);
    assert!(ok("{}").page.whole());
    for claims in [
        // A read property that isn't a list, or a statement that isn't one.
        r#"{"P31":{}}"#,
        r#"{"P31":["Q1"]}"#,
        // The snak names another property; an unknown snak type or rank.
        r#"{"P345":[{"mainsnak":{"snaktype":"value","property":"P346","datavalue":{"value":"tt0111161","type":"string"}},"rank":"normal"}]}"#,
        r#"{"P345":[{"mainsnak":{"snaktype":"odd","property":"P345"},"rank":"normal"}]}"#,
        r#"{"P345":[{"mainsnak":{"snaktype":"novalue","property":"P345"},"rank":"best"}]}"#,
        // A value of the wrong type.
        r#"{"P345":[{"mainsnak":{"snaktype":"value","property":"P345","datavalue":{"value":1,"type":"string"}},"rank":"normal"}]}"#,
        r#"{"P31":[{"mainsnak":{"snaktype":"value","property":"P31","datavalue":{"value":{"id":"P5"},"type":"wikibase-entityid"}},"rank":"normal"}]}"#,
        // A time that isn't a Wikibase time.
        &format!("{{\"P577\":[{}]}}", time("1994-09-10", 11)),
    ] {
        let e = ok(claims);
        assert!(!e.page.whole() && e.entities.is_empty(), "{claims}");
        assert_eq!(e.page.skipped.malformed, 1, "{claims}");
    }
    // A key that isn't the entity's id, or an entity that isn't an item.
    for body in [
        r#"{"entities":{"Q5":{"id":"Q6"}}}"#,
        r#"{"entities":{"Q5":{"id":"Q5","type":"property"}}}"#,
        r#"{"entities":{"Q5":{"id":"Q5","labels":{"en":{"language":"de","value":"x"}}}}}"#,
        r#"{"entities":{"Q5":{"id":"Q5","labels":{"en_x":{"language":"en_x","value":"x"}}}}}"#,
    ] {
        assert!(
            !parse_entities(body.as_bytes(), &ENTITY_LIMITS).page.whole(),
            "{body}"
        );
    }
    let e = parse_entities(br#"{"entities":{"L5":{"id":"L5"}}}"#, &ENTITY_LIMITS);
    assert_eq!(e.page.skipped.bad_id, 1);
}

#[test]
fn no_value_deprecated_and_unknown_ids_keep_nothing_but_read_whole() {
    let claims = format!(
        r#"{{"P345":[{{"mainsnak":{{"snaktype":"novalue","property":"P345"}},"rank":"normal"}},{{"mainsnak":{{"snaktype":"value","property":"P345","datavalue":{{"value":"nm0000209","type":"string"}}}},"rank":"normal"}},{{"mainsnak":{{"snaktype":"value","property":"P345","datavalue":{{"value":"tt0111161","type":"string"}}}},"rank":"deprecated"}}],"P4947":[{{"mainsnak":{{"snaktype":"value","property":"P4947","datavalue":{{"value":"0278","type":"string"}}}},"rank":"normal"}}],"P577":[{},{},{}]}}"#,
        time("-0500-00-00T00:00:00Z", 9),
        time("+1990-00-00T00:00:00Z", 8),
        time("+2001-00-00T00:00:00Z", 9),
    );
    let e = parse_entities(entity("Q5", &claims).as_bytes(), &ENTITY_LIMITS);
    assert!(e.page.whole(), "{:?}", e.page);
    let f = &e.entities[0];
    assert!(f.imdb.is_empty() && f.tmdb_movie.is_empty(), "{f:?}");
    assert_eq!(f.years, [2001]);
}

#[test]
fn the_count_caps_hold() {
    // Entities past the cap stop the parse.
    let many: String = (1..=6)
        .map(|i| format!(r#""Q{i}":{{"id":"Q{i}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let e = parse_entities(
        format!(r#"{{"entities":{{{many}}}}}"#).as_bytes(),
        &ENTITY_LIMITS,
    );
    assert_eq!(e.entities.len(), ENTITY_LIMITS.max_items);
    assert_eq!(e.page.stop, Some(Stop::Items));
    assert!(!e.page.whole());
    let results: String = (1..=11)
        .map(|i| format!(r#"{{"id":"Q{i}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let s = parse_search(
        format!(r#"{{"search":[{results}]}}"#).as_bytes(),
        &SEARCH_LIMITS,
    );
    assert_eq!(s.ids.len(), SEARCH_LIMITS.max_items);
    assert!(!s.page.whole());

    // Labels and aliases, and statements, past their caps are passed over.
    let tight = Limits {
        max_texts: 2,
        max_statements: 1,
        ..ENTITY_LIMITS
    };
    let body = r#"{"entities":{"Q5":{"id":"Q5","labels":{"en":{"language":"en","value":"A"},"de":{"language":"de","value":"B"}},"aliases":{"en":[{"language":"en","value":"C"}]},"claims":{"P31":[{"mainsnak":{"snaktype":"value","property":"P31","datavalue":{"value":{"id":"Q11424"},"type":"wikibase-entityid"}},"rank":"normal"},{"mainsnak":{"snaktype":"value","property":"P31","datavalue":{"value":{"id":"Q24869"},"type":"wikibase-entityid"}},"rank":"normal"}]}}}}"#;
    let e = parse_entities(body.as_bytes(), &tight);
    assert!(e.page.whole());
    let f = &e.entities[0];
    assert_eq!((f.labels.len(), f.aliases.len()), (2, 0));
    assert_eq!(f.instance_of, [Qid(11_424)]);
    assert!(f.truncated);
}

#[test]
fn byte_depth_and_text_caps_stop_or_skip() {
    let body = seed("entities.json");
    let cut = Limits {
        max_bytes: body.len() / 2,
        ..ENTITY_LIMITS
    };
    let e = parse_entities(body, &cut);
    assert_eq!(e.page.stop, Some(Stop::Bytes));
    assert!(!e.page.whole());
    // Entities finished before the cut are reported, but the answer isn't
    // whole, so a plugin uses none of it.
    let truncated = parse_entities(&body[..body.len() - 3], &ENTITY_LIMITS);
    assert_eq!(truncated.page.stop, Some(Stop::Malformed));
    assert!(!truncated.page.whole());
    let deep = format!(r#"{{"entities":{}{}}}"#, "[".repeat(64), "]".repeat(64));
    let e = parse_entities(deep.as_bytes(), &ENTITY_LIMITS);
    assert_eq!(e.page.stop, Some(Stop::Depth));
    let long = format!(
        r#"{{"entities":{{"Q5":{{"id":"Q5","labels":{{"en":{{"language":"en","value":"{}"}}}}}}}}}}"#,
        "a".repeat(ENTITY_LIMITS.max_text + 1)
    );
    let e = parse_entities(long.as_bytes(), &ENTITY_LIMITS);
    assert_eq!(e.page.skipped.over_cap, 1);
    let bad_utf8 = b"{\"search\":[{\"id\":\"Q1\"}]\xff}";
    assert_eq!(
        parse_search(bad_utf8, &SEARCH_LIMITS).page.stop,
        Some(Stop::NotUtf8)
    );
}

fn kept_ok(
    e: &lodestar_sources_feeds::wikidata::Entities,
    l: &Limits,
) -> Result<(), TestCaseError> {
    prop_assert!(e.entities.len() <= l.max_items);
    for f in &e.entities {
        prop_assert!(f.id.0 > 0);
        prop_assert!(f.labels.len() + f.aliases.len() <= l.max_texts);
        let statements = f.instance_of.len()
            + f.imdb.len()
            + f.tmdb_movie.len()
            + f.tmdb_tv.len()
            + f.tvdb.len()
            + f.years.len();
        prop_assert!(statements <= l.max_statements);
        for t in f.labels.iter().chain(&f.aliases) {
            prop_assert!(!t.value.is_empty() && t.value.len() <= MAX_TITLE);
            prop_assert!(t.value.trim() == t.value);
            prop_assert!(!t.language.is_empty() && t.language.len() <= MAX_LANGUAGE);
        }
        prop_assert!(f.imdb.iter().all(|s| s.starts_with("tt")));
        prop_assert!(f.years.iter().all(|y| (1..=9999).contains(y)));
    }
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        let s = parse_search(&body, &SEARCH_LIMITS);
        prop_assert!(s.ids.len() <= SEARCH_LIMITS.max_items);
        kept_ok(&parse_entities(&body, &ENTITY_LIMITS), &ENTITY_LIMITS)?;
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
        let s = parse_search(&body, &SEARCH_LIMITS);
        prop_assert!(s.ids.len() <= SEARCH_LIMITS.max_items);
        kept_ok(&parse_entities(&body, &ENTITY_LIMITS), &ENTITY_LIMITS)?;
    }
}
