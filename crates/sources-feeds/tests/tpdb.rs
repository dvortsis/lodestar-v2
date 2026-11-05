//! ThePornDB answers: golden fixtures (synthetic, from the public API
//! docs; see `tests/fixtures/tpdb/README.md`), every cap stopping exactly
//! at its value, errors stopping the read, no personal field reaching the
//! output, and property tests: never panics, never exceeds a cap, uses a
//! match whole or not at all. The last test runs the fuzz target's checks
//! over a mutated corpus, for machines without cargo-fuzz.

use lodestar_metainfo::hidden_char;
use lodestar_sources_feeds::tpdb::{
    Answer, Found, Kind, LIMITS, Limits, MAX_ID, MAX_NAME, MAX_TITLE, Match, Performer, Performers,
    Seen, Skipped, Stop, parse, parse_each,
};
use proptest::prelude::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tpdb");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{DIR}/{name}")).unwrap()
}

fn with(body: &[u8], limits: &Limits) -> Answer {
    let mut found = Vec::new();
    let page = parse_each(body, limits, |f| found.push(f));
    Answer { found, page }
}

fn performer(name: &str) -> Performer {
    Performer { name: name.into() }
}

fn is_id(s: &str) -> bool {
    (1..=MAX_ID).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn clean(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(hidden_char) && s.trim() == s
}

/// What holds for any input: `fuzz/fuzz_targets/tpdb.rs` checks the same.
fn check(a: &Answer, limits: &Limits) {
    let p = &a.page;
    assert_eq!(p.kept as usize, a.found.len());
    assert!((p.kept + p.skipped.total()) as usize <= limits.max_items);
    // What the page says it saw agrees with what it kept (X-enr-page-flag).
    assert!(!p.trailing || p.stop.is_some());
    let items = p.kept + p.skipped.total();
    assert!(items == 0 || matches!(p.data, Seen::List | Seen::Object));
    assert!(p.data != Seen::Object || items <= 1);
    assert!(!p.data_repeated || p.data != Seen::Absent);
    for f in &a.found {
        let m = &f.matched;
        assert!(is_id(&m.id));
        assert!(m.title.as_deref().is_none_or(|t| clean(t, MAX_TITLE)));
        assert!(m.studio.as_deref().is_none_or(|s| clean(s, MAX_NAME)));
        assert!(m.released.as_deref().is_none_or(|d| d.len() == 10));
        assert!(f.performers.list.len() <= limits.max_performers);
        for p in &f.performers.list {
            assert!(clean(&p.name, MAX_NAME));
        }
    }
}

// --- Golden fixtures ----------------------------------------------------

#[test]
fn scene_search() {
    let a = parse(&fixture("scenes_search.json"));
    let p = a.page;
    assert_eq!((p.stop, p.kept, p.total), (None, 2, Some(5)));
    assert_eq!(
        p.skipped,
        Skipped {
            no_id: 1,
            bad_id: 1,
            malformed: 1,
            ..Skipped::default()
        }
    );
    assert_eq!(
        a.found[0],
        Found {
            matched: Match {
                kind: Some(Kind::Scene),
                id: "00000000-0000-4000-8000-000000000001".into(),
                title: Some("Synthetic Scene One".into()),
                studio: Some("Example Studio".into()),
                released: Some("2024-02-29".into()),
            },
            performers: Performers {
                list: vec![
                    performer("Example Performer One"),
                    performer("Example Performer Two"),
                    // Ids aren't read, so a missing one changes nothing.
                    performer("Example Performer Without Id"),
                ],
                truncated: false,
            },
        }
    );
    assert_eq!(
        a.found[1],
        Found {
            matched: Match {
                kind: Some(Kind::Jav),
                id: "00000000-0000-4000-8000-000000000002".into(),
                // The zero-width space is removed.
                title: Some("Synthetic Scene Two".into()),
                studio: None,
                released: None,
            },
            performers: Performers::default(),
        }
    );
}

#[test]
fn lookups() {
    let a = parse(&fixture("scene_hash.json"));
    assert_eq!((a.page.stop, a.page.kept, a.page.total), (None, 1, None));
    assert_eq!(
        a.found[0],
        Found {
            matched: Match {
                kind: Some(Kind::Scene),
                id: "00000000-0000-4000-8000-000000000003".into(),
                title: Some("Synthetic Scene Found By Hash".into()),
                studio: Some("Example Network Site".into()),
                released: Some("2023-11-04".into()),
            },
            performers: Performers {
                list: vec![performer("Example Performer Three")],
                truncated: false,
            },
        }
    );
    let m = parse(&fixture("movie.json"));
    assert_eq!((m.page.stop, m.page.kept), (None, 1));
    let f = &m.found[0];
    assert_eq!(f.matched.kind, Some(Kind::Movie));
    assert_eq!(f.matched.title.as_deref(), Some("Synthetic Movie"));
    assert_eq!(f.matched.studio.as_deref(), Some("Example Movie Studio"));
    assert_eq!(f.performers.list.len(), 2);
    for miss in ["miss.json", "empty_search.json"] {
        let a = parse(&fixture(miss));
        assert_eq!((a.page.stop, a.page.kept), (None, 0), "{miss}");
        assert_eq!(a.page.skipped.total(), 0, "{miss}");
    }
    assert_eq!(parse(&fixture("empty_search.json")).page.total, Some(0));
}

/// Plan Q5: no birthday or other personal field of a person, nor anything
/// else the parser doesn't keep, ever reaches the output.
#[test]
fn no_personal_fields() {
    for name in fixtures() {
        let body = std::fs::read(&name).unwrap();
        let out = format!("{:?}", parse(&body));
        for never in [
            "SENTINEL",
            "1990-01-02",
            "631238400",
            "https://",
            "Synthetic Tag",
            "fedcba9876543210",
            // Performer ids (owner §2.37, Q4): none is kept.
            "0000000000a",
            "2000001",
            "2000004",
        ] {
            assert!(!out.contains(never), "{name:?} leaked {never}: {out}");
        }
    }
}

/// Owner §2.37 (Q4): a performer's id is neither kept nor needed. The
/// type has no id field; this checks no id text leaks through the name
/// either, in any id shape the service uses. This slice sends no request;
/// ENR-4 builds them and checks its own.
#[test]
fn no_performer_ids() {
    let body = br#"{"data":[{"id":"s","performers":[
        {"id":"perf-uuid-1111","_id":987654321,"slug":"perf-slug-2222","name":"Name One",
         "parent":{"id":"perf-uuid-3333","_id":123123123,"name":"Parent"}},
        {"id":444455556,"name":"Name Two"}]}]}"#;
    let a = parse(body);
    let names: Vec<&str> = a.found[0]
        .performers
        .list
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["Name One", "Name Two"]);
    let out = format!("{a:?}");
    for id in [
        "perf-uuid",
        "perf-slug",
        "987654321",
        "123123123",
        "444455556",
    ] {
        assert!(!out.contains(id), "{id} leaked: {out}");
    }
}

/// Both enrichment parsers cut a title the same way (`common::title`,
/// the runner's D4): a multi-byte character across `MAX_TITLE` is dropped
/// whole, never split, and the result is trimmed after the cut.
#[test]
fn both_parsers_cut_titles_alike() {
    use lodestar_sources_feeds::stashdb;
    let long = "a".repeat(MAX_TITLE - 1);
    let fits = "a".repeat(MAX_TITLE - 2);
    for (raw, want) in [
        (format!("{long}é…"), long.clone()),
        (format!("{long}\\u00e9…"), long.clone()),
        (format!("{fits}é…"), format!("{fits}é")),
        (format!(" {long} b"), long.clone()),
    ] {
        let t = parse(format!(r#"{{"data":{{"id":"a","title":"{raw}"}}}}"#).as_bytes());
        let s = stashdb::parse(
            format!(
                r#"{{"data":{{"findScene":{{"id":"00000000-0000-4000-8000-000000000001","title":"{raw}"}}}}}}"#
            )
            .as_bytes(),
        );
        let tt = t.found[0].matched.title.as_deref();
        let st = s.scenes[0].matched.title.as_deref();
        assert_eq!(tt, Some(want.as_str()));
        assert_eq!(st, tt);
    }
}

/// The two enrichment parsers degrade alike (runner, ENR-2b review): the
/// performer cap counts kept performers, and a wrong-typed studio or
/// performers list drops that field and keeps the match.
#[test]
fn both_parsers_degrade_alike() {
    use lodestar_sources_feeds::stashdb;
    const SID: &str = "00000000-0000-4000-8000-000000000001";
    let stash = |fields: &str, max_performers| {
        let body = format!(r#"{{"data":{{"findScene":{{"id":"{SID}",{fields}}}}}}}"#);
        let mut out = Vec::new();
        let l = stashdb::Limits {
            max_performers,
            ..stashdb::LIMITS
        };
        let page = stashdb::parse_each(body.as_bytes(), &l, |s| out.push(s));
        assert_eq!((page.kept, page.skipped.total()), (1, 0), "{body}");
        out.remove(0)
    };
    let tpdb = |fields: &str, max_performers| {
        let body = format!(r#"{{"data":{{"id":"a",{fields}}}}}"#);
        let l = Limits {
            max_performers,
            ..LIMITS
        };
        let a = with(body.as_bytes(), &l);
        assert_eq!((a.page.kept, a.page.skipped.total()), (1, 0), "{body}");
        a.found.into_iter().next().unwrap()
    };
    // `[bad, A, B, C]` with a cap of 2 keeps A and B in both.
    let t = tpdb(
        r#""performers":[{"name":5},{"name":"A"},{"name":"B"},{"name":"C"}]"#,
        2,
    );
    let s = stash(
        r#""performers":[{"performer":{"name":5}},{"performer":{"name":"A"}},
            {"performer":{"name":"B"}},{"performer":{"name":"C"}}]"#,
        2,
    );
    let tn: Vec<String> = t.performers.list.iter().map(|p| p.name.clone()).collect();
    let sn: Vec<String> = s.performers.iter().map(|p| p.name.clone()).collect();
    assert_eq!(tn, ["A", "B"]);
    assert_eq!(sn, tn);
    assert!(t.performers.truncated && s.performers_truncated);
    // A wrong-typed studio or performers list: dropped, the match kept.
    for (tf, sf) in [
        (r#""site":"Example""#, r#""studio":"Example""#),
        (r#""site":[1]"#, r#""studio":[1]"#),
    ] {
        assert_eq!(tpdb(tf, 64).matched.studio, None);
        assert_eq!(stash(sf, 64).matched.studio, None);
    }
    for f in [r#""performers":{}"#, r#""performers":"x""#] {
        let (t, s) = (tpdb(f, 64), stash(f, 64));
        assert!(t.performers.list.is_empty() && t.performers.truncated);
        assert!(s.performers.is_empty() && s.performers_truncated);
    }
    // Leading whitespace doesn't use up the title budget in either.
    let raw = format!("{}{}", " ".repeat(10), "a".repeat(1100));
    let f = format!(r#""title":"{raw}""#);
    let want = "a".repeat(MAX_TITLE);
    assert_eq!(tpdb(&f, 64).matched.title.as_deref(), Some(want.as_str()));
    assert_eq!(stash(&f, 64).matched.title.as_deref(), Some(want.as_str()));
}

fn fixtures() -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(DIR)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    v.sort();
    v
}

// --- Caps stop exactly at their value -------------------------------------

const WIDE: Limits = Limits {
    max_bytes: 1 << 20,
    max_depth: 16,
    max_values: 10_000,
    max_items: 100,
    max_array: 100,
    max_text: 1000,
    max_performers: 100,
};

fn ids(a: &Answer) -> Vec<&str> {
    a.found.iter().map(|f| f.matched.id.as_str()).collect()
}

#[test]
fn byte_cap() {
    let body = br#"{"data":[{"id":"a"}]}"#;
    let at = |n| {
        with(
            body,
            &Limits {
                max_bytes: n,
                ..WIDE
            },
        )
    };
    assert_eq!(
        (at(body.len()).page.stop, at(body.len()).page.kept),
        (None, 1)
    );
    let cut = at(body.len() - 1);
    // Cut short: the finished item is kept, the root never closes.
    assert_eq!((cut.page.stop, ids(&cut)), (Some(Stop::Bytes), vec!["a"]));
}

#[test]
fn depth_cap() {
    // root > data > item > performers > performer > parent > extras: 7.
    let body = br#"{"data":[{"id":"a","performers":[{"parent":{"extras":{}}}]}]}"#;
    let at = |n| {
        with(
            body,
            &Limits {
                max_depth: n,
                ..WIDE
            },
        )
    };
    assert_eq!((at(7).page.stop, at(7).page.kept), (None, 1));
    assert_eq!((at(6).page.stop, at(6).page.kept), (Some(Stop::Depth), 0));
    let deep = format!(r#"{{"data":{{"id":"a","x":{}}}}}"#, "[".repeat(100_000));
    assert_eq!(parse(deep.as_bytes()).page.stop, Some(Stop::Depth));
}

#[test]
fn value_cap() {
    // The root, `data`, two items and their ids: 6 values (keys aren't).
    let body = br#"{"data":[{"id":"a"},{"id":"b"}]}"#;
    let at = |n| {
        with(
            body,
            &Limits {
                max_values: n,
                ..WIDE
            },
        )
    };
    assert_eq!((at(6).page.stop, ids(&at(6))), (None, vec!["a", "b"]));
    assert_eq!(
        (at(5).page.stop, ids(&at(5))),
        (Some(Stop::Elements), vec!["a"])
    );
}

#[test]
fn item_cap() {
    let body = br#"{"data":[{"id":"a"},"x",{"id":"c"}]}"#;
    let at = |n| {
        with(
            body,
            &Limits {
                max_items: n,
                ..WIDE
            },
        )
    };
    assert_eq!((at(3).page.stop, ids(&at(3))), (None, vec!["a", "c"]));
    // Skipped items count too.
    let two = at(2);
    assert_eq!((two.page.stop, ids(&two)), (Some(Stop::Items), vec!["a"]));
    assert_eq!(two.page.skipped.malformed, 1);
}

#[test]
fn array_cap() {
    for (key, arr) in [("tags", "[1,2,3]"), ("directors", r#"[{"x":[1,2,3]}]"#)] {
        let body = format!(r#"{{"data":[{{"id":"a","{key}":{arr}}},{{"id":"b"}}]}}"#);
        let at = |n| {
            with(
                body.as_bytes(),
                &Limits {
                    max_array: n,
                    ..WIDE
                },
            )
        };
        assert_eq!(ids(&at(3)), vec!["a", "b"], "{key}");
        // Over it the item is skipped, not the read stopped.
        let over = at(2);
        assert_eq!((over.page.stop, ids(&over)), (None, vec!["b"]), "{key}");
        assert_eq!(over.page.skipped.over_cap, 1, "{key}");
    }
    // In `site`, an over-long array drops only the studio.
    let body = br#"{"data":{"id":"a","site":{"name":"S","x":[1,2,3]}}}"#;
    for (n, want) in [(3, Some("S")), (2, None)] {
        let a = with(
            body,
            &Limits {
                max_array: n,
                ..WIDE
            },
        );
        assert_eq!((a.page.kept, a.page.skipped.total()), (1, 0));
        assert_eq!(a.found[0].matched.studio.as_deref(), want);
    }
    // The performers list itself isn't capped by `max_array` (D3):
    // `max_values` and `max_bytes` bound it, `max_performers` what's kept.
    let body = br#"{"data":{"id":"a","performers":[{"name":"A"},{"name":"B"},{"name":"C"}]}}"#;
    let a = with(
        body,
        &Limits {
            max_array: 2,
            ..WIDE
        },
    );
    assert_eq!((a.page.kept, a.found[0].performers.list.len()), (1, 3));
}

#[test]
fn text_cap() {
    // Every string of the item the parser reads is measured raw: title,
    // type, date and id. Over the cap the item is skipped: a title can't
    // be cut without decoding it, so an over-long one isn't kept cut.
    for (field, n) in [
        (r#""title":"abcdef""#, 6),
        (r#""type":"Scene\u0020""#, 11),
        (r#""date":"2024-01-01""#, 10),
        (r#""id":"abcdef""#, 6),
    ] {
        let lead = if field.starts_with(r#""id""#) {
            ""
        } else {
            r#""id":"a","#
        };
        let body = format!(r#"{{"data":[{{{lead}{field}}},{{"id":"b"}}]}}"#);
        let at = |n| {
            with(
                body.as_bytes(),
                &Limits {
                    max_text: n,
                    ..WIDE
                },
            )
        };
        assert_eq!(at(20).page.kept, 2, "{field}");
        assert_eq!(at(n).page.kept, 2, "{field}");
        let over = at(n - 1);
        assert_eq!((over.page.stop, ids(&over)), (None, vec!["b"]), "{field}");
        assert_eq!(over.page.skipped.over_cap, 1, "{field}");
    }
    // A studio name over the cap drops only the studio.
    let studio = br#"{"data":{"id":"a","site":{"name":"abcdef"}}}"#;
    for (n, want) in [(6, Some("abcdef")), (5, None)] {
        let a = with(
            studio,
            &Limits {
                max_text: n,
                ..WIDE
            },
        );
        assert_eq!((a.page.kept, a.page.skipped.total()), (1, 0));
        assert_eq!(a.found[0].matched.studio.as_deref(), want);
    }
    // A performer's id is never decoded, so its length doesn't matter.
    let long_id = br#"{"data":{"id":"a","performers":[{"id":"abcdef","name":"N"}]}}"#;
    let a = with(
        long_id,
        &Limits {
            max_text: 5,
            ..WIDE
        },
    );
    assert_eq!(a.found[0].performers.list, vec![performer("N")]);
    // A performer's name is measured, but over the cap only the
    // performer is dropped: the match doesn't depend on whether
    // performers are read (plan Q4).
    for field in [
        r#"{"id":"p","name":"abcdef"}"#,
        r#"{"id":"p","name":"\ud800"}"#,
    ] {
        let body =
            format!(r#"{{"data":{{"id":"a","performers":[{field},{{"id":"q","name":"Q"}}]}}}}"#);
        for max_performers in [0, 2] {
            let a = with(
                body.as_bytes(),
                &Limits {
                    max_text: 5,
                    max_performers,
                    ..WIDE
                },
            );
            assert_eq!((a.page.kept, a.page.skipped.total()), (1, 0), "{field}");
            let p = &a.found[0].performers;
            let want = if max_performers == 0 {
                vec![]
            } else {
                vec![performer("Q")]
            };
            assert_eq!((&p.list, p.truncated), (&want, true), "{field}");
        }
        let a = with(
            body.as_bytes(),
            &Limits {
                max_text: 6,
                ..WIDE
            },
        );
        let n = if field.contains("ud800") { 1 } else { 2 };
        assert_eq!(a.found[0].performers.list.len(), n, "{field}");
    }
    // Any fault inside an entry, an over-long array included, drops only
    // that entry, whether performers are read or not (D3).
    let body = br#"{"data":{"id":"a","performers":[
        {"name":"P","x":[1,2,3]},{"name":[1,2,3]},{"name":5},"x",{"name":"Q"}]}}"#;
    for (max_performers, want) in [(0, vec![]), (5, vec![performer("Q")])] {
        let l = Limits {
            max_array: 2,
            max_performers,
            ..WIDE
        };
        let a = with(body, &l);
        assert_eq!((a.page.kept, a.page.skipped.total()), (1, 0));
        let p = &a.found[0].performers;
        assert_eq!((&p.list, p.truncated), (&want, true));
    }
    // A long description is never read, so it spoils nothing.
    let long = format!(
        r#"{{"data":{{"id":"a","description":"{}"}}}}"#,
        "x".repeat(5000)
    );
    assert_eq!(with(long.as_bytes(), &WIDE).page.kept, 1);
}

#[test]
fn performer_cap() {
    let body = br#"{"data":{"id":"a","performers":[
        {"id":"p1","name":"One"},{"id":"p2","name":"Two"},{"id":"p3","name":"Three"}]}}"#;
    let at = |n| {
        let a = with(
            body,
            &Limits {
                max_performers: n,
                ..WIDE
            },
        );
        assert_eq!(a.page.kept, 1);
        a.found.into_iter().next().unwrap().performers
    };
    let all = at(3);
    assert_eq!((all.list.len(), all.truncated), (3, false));
    let two = at(2);
    assert_eq!((two.list.len(), two.truncated), (2, true));
    assert_eq!(two.list[1], performer("Two"));
    // 0 reads none: the way to drop them at the source.
    let none = at(0);
    assert_eq!((none.list.len(), none.truncated), (0, true));
}

// --- Errors and field rules -----------------------------------------------

/// A structural error mid-value stops the read there: nothing after it is
/// read, and the item it was in is never used.
#[test]
fn a_mid_value_error_stops_the_parse() {
    for bad in [
        r#"{"data":[{"id":"a"},{"id":"b","title":tru},{"id":"c"}]}"#,
        r#"{"data":[{"id":"a"},{"id":"b","date":nul},{"id":"c"}]}"#,
        r#"{"data":[{"id":"a"},{"id":"b","duration":1.},{"id":"c"}]}"#,
        r#"{"data":[{"id":"a"},{"id":"b","title":"x\q"},{"id":"c"}]}"#,
        r#"{"data":[{"id":"a"},{"id":"b","performers":[{"name":"N",}]},{"id":"c"}]}"#,
        r#"{"data":[{"id":"a"},{"id":"b","site":{"name" "x"}},{"id":"c"}]}"#,
        r#"{"data":[{"id":"a"},{"id":"b","title":"cut"#,
    ] {
        let a = parse(bad.as_bytes());
        assert_eq!(a.page.stop, Some(Stop::Malformed), "{bad}");
        assert_eq!(ids(&a), vec!["a"], "{bad}");
        assert_eq!(a.page.skipped.total(), 0, "{bad}");
    }
    // Text after the root is an error too, but the items are finished.
    let a = parse(br#"{"data":[{"id":"a"}]} x"#);
    assert_eq!((a.page.stop, ids(&a)), (Some(Stop::Malformed), vec!["a"]));
}

#[test]
fn field_rules() {
    let a = parse(
        br#"{"data":[
        {"id":1234,"title":"  T  ","title":"second","type":null,"date":null,
         "site":"Example Studio","performers":{"id":"p"}},
        {"id":-1},{"id":1.5},{"id":null},{"id":"a_b"},
        {"id":"lone","title":"\ud800"},
        {"id":"f1","type":"VR"},{"id":"f2","date":"2023-02-29"},{"id":"f3","date":20240101},
        {"id":"f4","type":7},{"id":"f5","title":5},{"id":"f6","date":"2024-13-01","type":"VR"},
        {"id":"x","performers":[null,true,{"id":"p","name":"\u202e"},{"id":"q","name":"Q","name":"R"}]},
        {"id":"y","performers":null,"site":{"name":null,"name":"late"}},
        {"id":"z","site":{"name":5}},{"id":"z2","title":null,"site":{"name":"\ud800","x":1}}
    ],"data":[{"id":"second-data"}],"meta":{"total":"5"}}"#,
    );
    assert_eq!(a.page.stop, None);
    assert_eq!(ids(&a), vec!["1234", "x", "y", "z", "z2"]);
    // A wrong-typed value, an unknown `type` or a bad date skips the item.
    assert_eq!(
        a.page.skipped,
        Skipped {
            bad_id: 4,
            malformed: 7,
            ..Skipped::default()
        }
    );
    let first = &a.found[0];
    assert_eq!(
        first.matched,
        Match {
            kind: None,
            id: "1234".into(),
            title: Some("T".into()),
            studio: None,
            released: None,
        }
    );
    assert!(first.performers.truncated && first.performers.list.is_empty());
    // Non-objects and a name of only hidden characters are dropped; the
    // first `name` wins.
    let x = &a.found[1].performers;
    assert_eq!((x.list.clone(), x.truncated), (vec![performer("Q")], true));
    let y = &a.found[2];
    assert_eq!(
        (y.matched.studio.as_deref(), y.performers.truncated),
        (None, false)
    );
    // A fault in the studio's name drops only the studio.
    for f in &a.found[3..] {
        assert_eq!(
            (f.matched.studio.as_deref(), f.matched.title.as_deref()),
            (None, None)
        );
    }
    // Titles are trimmed after hidden characters go, and after the cut.
    let t = |raw: &str| {
        let body = format!(r#"{{"data":{{"id":"a","title":{raw}}}}}"#);
        parse(body.as_bytes()).found[0].matched.title.clone()
    };
    assert_eq!(t(r#"" a \u200b""#).as_deref(), Some("a"));
    assert_eq!(t(r#""\u200b \u200b""#), None);
    let cut = format!("\"{} b\"", "a".repeat(MAX_TITLE - 1));
    assert_eq!(t(&cut), Some("a".repeat(MAX_TITLE - 1)));
    // A multi-byte character across `MAX_TITLE` is dropped whole.
    let long = "a".repeat(MAX_TITLE - 1);
    // Escaped and raw: the cut is on the decoded text.
    assert_eq!(t(&format!("\"{long}\\u00e9…\"")), Some(long.clone()));
    assert_eq!(t(&format!("\"{long}é…\"")), Some(long));
    // `meta.total` is a number or nothing.
    assert_eq!(a.page.total, None);
    // Not an object at the root: nothing, no stop.
    for root in ["[]", "null", "\"x\"", "{}", r#"{"data":"x"}"#] {
        let a = parse(root.as_bytes());
        assert_eq!((a.page.stop, a.page.kept), (None, 0), "{root}");
    }
    // A byte-order mark is fine; bytes that aren't UTF-8 stop the read.
    assert_eq!(parse(b"\xef\xbb\xbf{\"data\":{\"id\":\"a\"}}").page.kept, 1);
    let a = parse(b"{\"data\":[{\"id\":\"a\"},{\"id\":\"b\xff\"}]}");
    assert_eq!((a.page.stop, ids(&a)), (Some(Stop::NotUtf8), vec!["a"]));
}

// --- Properties -------------------------------------------------------------

/// Tight caps picked by one byte, as the fuzz target does.
fn small(pick: u8) -> Limits {
    Limits {
        max_bytes: 1 + usize::from(pick) * 64,
        max_depth: 1 + usize::from(pick % 8),
        max_values: 1 + usize::from(pick) * 4,
        max_items: 1 + usize::from(pick % 4),
        max_array: usize::from(pick % 6),
        max_text: usize::from(pick) * 4,
        max_performers: usize::from(pick % 5),
    }
}

/// `s` as a JSON string, non-ASCII escaped when `esc` says so.
fn json(s: &str, esc: bool) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c < ' ' || (esc && !c.is_ascii()) => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("{".to_string()),
        Just("}".to_string()),
        Just("[".to_string()),
        Just("]".to_string()),
        Just(",".to_string()),
        Just(":".to_string()),
        Just(r#""data":["#.to_string()),
        Just(r#""data":{"#.to_string()),
        Just(r#""performers":["#.to_string()),
        Just(r#"{"id":"p","name":"N"},"#.to_string()),
        Just(r#""id":"00000000-0000-4000-8000-000000000001","#.to_string()),
        Just(r#""id":42,"#.to_string()),
        Just(r#""title":"t","#.to_string()),
        Just(r#""type":"Scene","#.to_string()),
        Just(r#""date":"2024-02-29","#.to_string()),
        Just(r#""site":{"name":"s"},"#.to_string()),
        Just(r#""extra":{"birthday":"1990-01-02"},"#.to_string()),
        Just(r#""meta":{"total":3},"#.to_string()),
        Just(r#""title":"\ud800","#.to_string()),
        Just(r#""title":"\u202e\u200b","#.to_string()),
        Just("null,".to_string()),
        "[a-z{}\\[\\]\":,0-9\\\\ \u{202E}\u{200B}\u{FEFF}]{0,12}",
    ]
}

/// One generated item: its parts, and how (if at all) it's spoiled.
#[derive(Debug, Clone)]
struct Gen {
    id: String,
    title: String,
    studio: String,
    date: Option<(u16, u8, u8)>,
    performers: Vec<(String, String)>,
    kind: u8,
    esc: bool,
    /// 0: whole; 1: a lone surrogate in a late field; 2: an over-long
    /// array after the kept fields; 3: an over-long string; 4: a bad id.
    spoil: u8,
}

fn gen_item() -> impl Strategy<Value = Gen> {
    (
        "[A-Za-z0-9-]{1,64}",
        "[a-zA-Z0-9 é—😀\"\\\\/]{1,40}",
        "[a-zA-Z0-9 é]{1,30}",
        proptest::option::of((1u16..=9999, 1u8..=12, 1u8..=28)),
        proptest::collection::vec(("[a-z0-9-]{1,20}", "[A-Za-z é]{1,30}"), 0..6),
        0u8..4,
        any::<bool>(),
        prop_oneof![4 => Just(0u8), 1 => 1u8..=4],
    )
        .prop_map(
            |(id, title, studio, date, performers, kind, esc, spoil)| Gen {
                id,
                title,
                studio,
                date,
                performers,
                kind,
                esc,
                spoil,
            },
        )
}

/// The last is `null`: no kind, and no fault.
const KINDS: [&str; 4] = ["\"Scene\"", "\"Movie\"", "\"JAV\"", "null"];
/// Caps the generated items fit unless spoiled.
const GEN: Limits = Limits {
    max_array: 8,
    max_text: 512,
    ..LIMITS
};

impl Gen {
    fn body(&self, marker: usize) -> String {
        let perf: Vec<String> = self
            .performers
            .iter()
            .map(|(id, name)| {
                format!(
                    r#"{{"id":"{id}","name":{},"extra":{{"birthday":"1990-01-02"}}}}"#,
                    json(&format!("{name} M{marker}"), self.esc)
                )
            })
            .collect();
        let id = if self.spoil == 4 {
            format!("{} bad", self.id)
        } else {
            self.id.clone()
        };
        let date = self.date.map_or("null".into(), |(y, m, d)| {
            format!("\"{y:04}-{m:02}-{d:02}\"")
        });
        let tail = match self.spoil {
            1 => r#","directors":[],"site2":0,"type":"\ud83d""#.to_string(),
            2 => r#","tags":[1,2,3,4,5,6,7,8,9]"#.to_string(),
            3 => format!(r#","type":"{}""#, "x".repeat(GEN.max_text + 1)),
            _ => String::new(),
        };
        // The spoiling field comes last, after every kept field was read,
        // so a half-used item would show.
        let (kind, tail) = if self.spoil == 1 || self.spoil == 3 {
            (String::new(), tail)
        } else {
            (format!(r#""type":{},"#, KINDS[self.kind as usize]), tail)
        };
        format!(
            r#"{{{kind}"id":"{id}","title":{},"date":{date},"site":{{"name":{}}},"performers":[{}]{tail}}}"#,
            json(&format!("{} M{marker}", self.title), self.esc),
            json(&format!("{} M{marker}", self.studio), self.esc),
            perf.join(","),
        )
    }
}

proptest! {
    #[test]
    fn any_bytes_are_safe(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        check(&parse(&body), &LIMITS);
    }

    #[test]
    fn any_fragments_are_safe(
        parts in proptest::collection::vec(fragment(), 0..60),
        pick in any::<u8>(),
    ) {
        for root in [r#"{"data":["#, r#"{"data":{"#, ""] {
            let body = format!("{root}{}", parts.concat());
            check(&parse(body.as_bytes()), &LIMITS);
            let limits = small(pick);
            check(&with(body.as_bytes(), &limits), &limits);
        }
    }

    /// Whole items read back exactly; spoiled ones leave no trace: not
    /// their title, studio, performers or id.
    #[test]
    fn items_are_whole_or_absent(items in proptest::collection::vec(gen_item(), 0..12)) {
        let body: Vec<String> = items.iter().enumerate().map(|(i, g)| g.body(i)).collect();
        let body = format!(r#"{{"data":[{}],"meta":{{"total":{}}}}}"#, body.join(","), items.len());
        let a = with(body.as_bytes(), &GEN);
        prop_assert_eq!(a.page.stop, None);
        prop_assert_eq!(a.page.total, Some(items.len() as u64));
        let whole: Vec<(usize, &Gen)> =
            items.iter().enumerate().filter(|(_, g)| g.spoil == 0).collect();
        prop_assert_eq!(a.found.len(), whole.len());
        prop_assert_eq!(a.page.skipped.total() as usize, items.len() - whole.len());
        for (f, (i, g)) in a.found.iter().zip(&whole) {
            let m = &f.matched;
            let t = |s: &str| format!("{s} M{i}").trim().to_owned();
            prop_assert_eq!(&m.id, &g.id);
            prop_assert_eq!(m.title.clone(), Some(t(&g.title)));
            prop_assert_eq!(m.studio.clone(), Some(t(&g.studio)));
            let want_kind = [Some(Kind::Scene), Some(Kind::Movie), Some(Kind::Jav), None];
            prop_assert_eq!(m.kind, want_kind[g.kind as usize]);
            prop_assert_eq!(
                m.released.clone(),
                g.date.map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
            );
            let want: Vec<Performer> = g
                .performers
                .iter()
                .map(|(_, n)| performer(&t(n)))
                .collect();
            prop_assert_eq!(&f.performers.list, &want);
            prop_assert!(!f.performers.truncated);
        }
        let out = format!("{:?}", a.found);
        for (i, g) in items.iter().enumerate() {
            if g.spoil != 0 {
                prop_assert!(!out.contains(&format!(" M{i}\"")), "item {} leaked: {}", i, out);
            }
        }
        prop_assert!(!out.contains("1990-01-02"));
    }
}

/// xorshift64*: a fixed, dependency-free source of mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// The fuzz target's checks over 100,000 inputs: random bytes, and the
/// fixtures with bytes flipped, inserted, deleted, duplicated and spliced.
#[test]
fn mutated_corpus() {
    let seeds: Vec<Vec<u8>> = fixtures()
        .iter()
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    let tokens: [&[u8]; 12] = [
        b"{", b"}", b"[", b"]", b",", b"\"", b"\\", b":", b"\"id\"", b"null", b"\xff", b"1e999",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut kept, mut performers) = (0, 0);
    for n in 0..100_000 {
        let mut input = if n % 10 == 0 {
            (0..rng.below(256)).map(|_| rng.next() as u8).collect()
        } else {
            seeds[rng.below(seeds.len())].clone()
        };
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(input.len() + 1);
            match rng.below(6) {
                0 if at < input.len() => input[at] ^= 1 << rng.below(8),
                1 => input.insert(at, rng.next() as u8),
                2 if at < input.len() => {
                    input.remove(at);
                }
                3 => {
                    let t = tokens[rng.below(tokens.len())];
                    input.splice(at..at, t.iter().copied());
                }
                4 => {
                    let end = (at + rng.below(64)).min(input.len());
                    let chunk = input[at..end].to_vec();
                    input.splice(at..at, chunk);
                }
                _ => {
                    let other = &seeds[rng.below(seeds.len())];
                    let from = rng.below(other.len());
                    input.truncate(at);
                    input.extend_from_slice(&other[from..]);
                }
            }
        }
        let pick = rng.next() as u8;
        let a = parse(&input);
        check(&a, &LIMITS);
        kept += a.found.len();
        performers += a
            .found
            .iter()
            .map(|f| f.performers.list.len())
            .sum::<usize>();
        let limits = small(pick);
        check(&with(&input, &limits), &limits);
    }
    // The mutations leave many answers whole enough to reach the items.
    eprintln!("kept {kept} matches with {performers} performers");
    assert!(kept > 5_000 && performers > 5_000, "{kept} {performers}");
}

// --- What the parser saw (X-enr-page-flag) ----------------------------------

/// The page reports the root's `data` kind, a repeated `data`, the other
/// root keys and trailing bytes, so a caller needs no second read.
#[test]
fn the_page_reports_the_root() {
    let page = |b: &str| parse(b.as_bytes()).page;
    for (body, data) in [
        (r#"{"data":[]}"#, Seen::List),
        (r#"{"data":{"id":"a"}}"#, Seen::Object),
        (r#"{"data":null}"#, Seen::Null),
        (r#"{"data":"x"}"#, Seen::Other),
        (r#"{"data":5}"#, Seen::Other),
        (r#"{"data":false}"#, Seen::Other),
        (r#"{"message":"data"}"#, Seen::Absent),
        (r#"{"meta":{"data":[]}}"#, Seen::Absent),
        (r#"[{"data":[]}]"#, Seen::Absent),
        (r#""data""#, Seen::Absent),
    ] {
        assert_eq!(page(body).data, data, "{body}");
    }
    // The first `data` is read; a repeat is reported, not used.
    let a = parse(br#"{"data":[],"data":[{"id":"a"}]}"#);
    assert_eq!(
        (a.page.data, a.page.data_repeated, a.found.len()),
        (Seen::List, true, 0)
    );
    let a = parse(br#"{"data":null,"data":{"id":"a"}}"#);
    assert_eq!(
        (a.page.data, a.page.data_repeated, a.found.len()),
        (Seen::Null, true, 0)
    );
    // Root keys are compared unescaped (`~` stands for a backslash here).
    let esc = |s: &str| s.replace('~', &char::from(92).to_string());
    let p = parse(esc(r#"{"d~u0061ta":[]}"#).as_bytes()).page;
    assert_eq!((p.data, p.others), (Seen::List, 0));
    let a = parse(esc(r#"{"data":null,"d~u0061ta":{"id":"a"}}"#).as_bytes());
    assert_eq!(
        (a.page.data, a.page.data_repeated, a.found.len()),
        (Seen::Null, true, 0)
    );
    let p = parse(esc(r#"{"m~u0065ta":5,"~u0064ata":[]}"#).as_bytes()).page;
    assert_eq!((p.data, p.others, p.data_repeated), (Seen::List, 0, false));
    // A key that doesn't decode stays as written: never `data`.
    let p = parse(esc(r#"{"~ud800":[],"data":null}"#).as_bytes()).page;
    assert_eq!((p.data, p.others, p.data_repeated), (Seen::Null, 1, false));
    assert!(!page(r#"{"data":[],"meta":{},"meta":{}}"#).data_repeated);
    // Other root keys are counted; `meta` isn't one, whatever its type.
    assert_eq!(
        page(r#"{"data":[],"links":{},"meta":{"total":1}}"#).others,
        1
    );
    assert_eq!(page(r#"{"message":"x","error":1,"meta":5}"#).others, 2);
    assert_eq!(page(r#"{"data":[]}"#).others, 0);
    // Trailing bytes.
    for (body, trailing) in [
        (r#"{"data":[]} x"#, true),
        (r#"{"data":[]}{}"#, true),
        ("{\"data\":[]} \n\t", false),
        (r#"{"data":[]"#, false),
        (r#"null 1"#, true),
    ] {
        let p = page(body);
        assert_eq!(p.trailing, trailing, "{body}");
        if trailing {
            assert_eq!(p.stop, Some(Stop::Malformed), "{body}");
        }
    }
    // Bytes after the root of a body cut at its first bad UTF-8 byte: the
    // earlier stop stays (found by the fuzz target).
    let p = parse(b"0t\xbb").page;
    assert_eq!((p.trailing, p.stop), (true, Some(Stop::NotUtf8)));
    // A well-formed answer reports only what it is.
    let p = parse(&fixture("scenes_search.json")).page;
    assert_eq!(
        (p.data, p.data_repeated, p.trailing),
        (Seen::List, false, false)
    );
}

/// Past the item cap, the rest of the document is still read: a later
/// fault, repeat or trailing byte is reported, and a fault replaces the
/// cap's stop.
#[test]
fn the_rest_is_read_past_the_item_cap() {
    let one = Limits {
        max_items: 1,
        ..WIDE
    };
    let at = |b: &str| with(b.as_bytes(), &one);
    let a = at(r#"{"data":[{"id":"a"},{"id":"bad id!"},{"id":"c"}],"meta":{"total":3}}"#);
    assert_eq!((a.page.stop, ids(&a)), (Some(Stop::Items), vec!["a"]));
    assert_eq!((a.page.skipped.total(), a.page.total), (0, Some(3)));
    let a = at(r#"{"data":[{"id":"a"},{"id":"b"}],"data":[]}"#);
    assert_eq!(
        (a.page.stop, a.page.data_repeated),
        (Some(Stop::Items), true)
    );
    let a = at(r#"{"data":[{"id":"a"},{"id":"b"}]} x"#);
    assert_eq!(
        (a.page.stop, a.page.trailing),
        (Some(Stop::Malformed), true)
    );
    for cut in [
        r#"{"data":[{"id":"a"},{"id":"b"}"#,
        r#"{"data":[{"id":"a"},{"id":tru}]}"#,
        r#"{"data":[{"id":"a"},{"id":"b"}],"links":{"x":"\q"}}"#,
    ] {
        let a = at(cut);
        assert_eq!(
            (a.page.stop, ids(&a)),
            (Some(Stop::Malformed), vec!["a"]),
            "{cut}"
        );
    }
    let deep = format!(
        r#"{{"data":[{{"id":"a"}},{}1{}]}}"#,
        "[".repeat(20),
        "]".repeat(20)
    );
    assert_eq!(at(&deep).page.stop, Some(Stop::Depth));
    // A byte cap stays the stop, whatever follows.
    let body = br#"{"data":[{"id":"a"},{"id":"b"}]}"#;
    let cut = with(
        body,
        &Limits {
            max_bytes: body.len() - 3,
            ..one
        },
    );
    assert_eq!(cut.page.stop, Some(Stop::Bytes));
}
