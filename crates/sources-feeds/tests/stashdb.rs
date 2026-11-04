//! StashDB answers (ENR-3): golden tests on the synthetic fixtures, the
//! personal-field test (plan Q5), the stop-on-error test, every cap
//! stopping exactly, and property tests: never panics, never exceeds a
//! cap, uses a scene whole or not at all. The last test runs the fuzz
//! target's checks over a long mutated corpus, for machines without
//! cargo-fuzz.

use lodestar_metainfo::hidden_char;
use lodestar_sources_feeds::stashdb::{
    Date, Found, ID_LEN, Kind, LIMITS, Limits, MAX_NAME, MAX_TITLE, Match, Page, Performer, SCENES,
    Scene, Skipped, Stop, Studio, parse, parse_each,
};
use proptest::prelude::*;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/stashdb/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

fn with(body: &[u8], limits: &Limits) -> Found {
    let mut scenes = Vec::new();
    let page = parse_each(body, limits, |s| scenes.push(s));
    Found { scenes, page }
}

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

fn studio(n: u32, name: &str) -> Option<Studio> {
    Some(Studio {
        id: id(n),
        name: name.into(),
    })
}

fn performer(n: u32, name: &str) -> Performer {
    Performer {
        id: id(n),
        name: name.into(),
    }
}

fn day(y: u16, m: u8, d: u8) -> Option<Date> {
    Some(Date {
        year: y,
        month: Some(m),
        day: Some(d),
    })
}

/// What holds for any input: `fuzz/fuzz_targets/stashdb.rs` checks the
/// same.
fn check(f: &Found, limits: &Limits) {
    let p = &f.page;
    assert_eq!(p.kept as usize, f.scenes.len());
    assert!((p.kept + p.skipped.total()) as usize <= limits.max_items);
    let uuid = |s: &str| {
        s.len() == ID_LEN
            && s.bytes().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => b == b'-',
                _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
            })
    };
    let name = |s: &str| {
        !s.is_empty() && s.len() <= MAX_NAME && s.trim() == s && !s.chars().any(hidden_char)
    };
    for s in &f.scenes {
        let m = &s.matched;
        assert_eq!(m.kind, Kind::Scene);
        assert!(uuid(&m.id), "{}", m.id);
        assert_eq!(m.link(), format!("{SCENES}{}", m.id));
        assert!(m.title.as_ref().is_none_or(|t| !t.is_empty()
            && t.len() <= MAX_TITLE
            && t.trim() == t
            && !t.chars().any(hidden_char)));
        assert!(
            m.studio
                .as_ref()
                .is_none_or(|st| uuid(&st.id) && name(&st.name))
        );
        assert!(m.released.is_none_or(|d| {
            (1..=9999).contains(&d.year)
                && d.month.is_none_or(|m| (1..=12).contains(&m))
                && d.day
                    .is_none_or(|x| d.month.is_some() && (1..=31).contains(&x))
        }));
        assert!(s.performers.len() <= limits.max_performers);
        for p in &s.performers {
            assert!(uuid(&p.id) && name(&p.name));
        }
    }
}

// --- Golden tests on the fixtures ------------------------------------

#[test]
fn search_answer() {
    let f = parse(&fixture("search.json"));
    assert_eq!(
        f.page,
        Page {
            kept: 3,
            skipped: Skipped {
                no_id: 1,
                over_cap: 0,
                malformed: 3,
            },
            stop: None,
            errors: 0,
        }
    );
    assert_eq!(
        f.scenes,
        [
            Scene {
                matched: Match {
                    kind: Kind::Scene,
                    id: id(0xa1),
                    title: Some("Synthetic Scene Alpha".into()),
                    studio: studio(0xc1, "Example Studio One"),
                    released: day(2024, 3, 15),
                },
                // The second id was upper case.
                performers: vec![
                    performer(0xe1, "Example Performer One"),
                    performer(0xe2, "Example Performer Two"),
                ],
            },
            Scene {
                matched: Match {
                    kind: Kind::Scene,
                    id: id(0xa2),
                    // Escapes decoded, hidden characters removed, trimmed.
                    title: Some("Synthetic Scene Beta".into()),
                    studio: None,
                    released: Some(Date {
                        year: 2023,
                        month: Some(11),
                        day: None,
                    }),
                },
                performers: vec![],
            },
            Scene {
                matched: Match {
                    kind: Kind::Scene,
                    id: id(0xa3),
                    title: None,
                    studio: studio(0xc2, "Example Studio Two"),
                    released: None,
                },
                performers: vec![performer(0xe1, "Example Performer One")],
            },
        ]
    );
    assert_eq!(
        f.scenes[0].matched.link(),
        "https://stashdb.org/scenes/00000000-0000-4000-8000-0000000000a1"
    );
}

#[test]
fn find_answers() {
    let f = parse(&fixture("find.json"));
    assert_eq!(
        (f.page.kept, f.page.skipped.total(), f.page.stop),
        (1, 0, None)
    );
    assert_eq!(
        f.scenes[0],
        Scene {
            matched: Match {
                kind: Kind::Scene,
                id: id(0xb1),
                title: Some("Synthetic Scene Gamma".into()),
                studio: studio(0xc3, "Example Studio Three"),
                released: Some(Date {
                    year: 2022,
                    month: None,
                    day: None,
                }),
            },
            performers: vec![performer(0xe4, "Example Performer Four")],
        }
    );
    // No such scene is no scene, not a skipped one.
    assert_eq!(parse(&fixture("find_null.json")), Found::default());
}

#[test]
fn errors_are_counted_not_kept() {
    let f = parse(&fixture("errors.json"));
    assert_eq!(
        f,
        Found {
            scenes: vec![],
            page: Page {
                errors: 1,
                ..Page::default()
            },
        }
    );
    // Partial data beside errors is kept; an `errors` that isn't a list
    // counts once.
    let scene = format!(r#"{{"id":"{}"}}"#, id(1));
    let body = format!(r#"{{"errors":{{"message":"x"}},"data":{{"findScene":{scene}}}}}"#);
    let f = parse(body.as_bytes());
    assert_eq!((f.page.kept, f.page.errors), (1, 1));
    let body = r#"{"errors":[{},{},{}],"data":{"searchScene":[]}}"#;
    assert_eq!(parse(body.as_bytes()).page.errors, 3);
}

/// Plan Q5: a server that sends personal fields anyway yields none of
/// them. Every personal value in the fixture is marked `PERSONAL-` or
/// `999…`, and none reaches the output.
#[test]
fn personal_fields_are_never_kept() {
    let body = fixture("personal.json");
    let text = String::from_utf8(body.clone()).unwrap();
    assert!(text.contains("birth_date") && text.contains("measurements"));
    let f = parse(&body);
    assert_eq!(
        (f.page.kept, f.page.skipped.total(), f.page.stop),
        (1, 0, None)
    );
    assert_eq!(
        f.scenes[0],
        Scene {
            matched: Match {
                kind: Kind::Scene,
                id: id(0xd1),
                title: Some("Synthetic Scene Delta".into()),
                studio: studio(0xc4, "Example Studio Four"),
                released: day(2021, 6, 1),
            },
            performers: vec![performer(0xe5, "Example Performer Five")],
        }
    );
    let out = format!("{f:?}");
    assert!(!out.contains("PERSONAL") && !out.contains("999"), "{out}");
}

/// Performers are apart from the match, so dropping them is one move.
#[test]
fn performers_drop_with_the_match_alone() {
    let f = parse(&fixture("search.json"));
    let matches: Vec<Match> = f.scenes.into_iter().map(|s| s.matched).collect();
    assert_eq!(matches.len(), 3);
    assert!(!format!("{matches:?}").contains("Performer"));
}

/// A structural fault mid-value ends the parse: the reader's cursor is
/// mid-token, so nothing after it is read, and the scene it was in is
/// never handed out (#422's review).
#[test]
fn a_mid_value_error_stops_the_parse() {
    let good = format!(r#"{{"id":"{}","title":"Kept"}}"#, id(1));
    let after = format!(r#"{{"id":"{}","title":"Never read"}}"#, id(3));
    for bad in [
        // A bad escape in a kept string.
        format!(r#"{{"id":"{}","title":"a\qb"}}"#, id(2)),
        // A number cut short in a field read past.
        format!(r#"{{"id":"{}","duration":12.,"title":"x"}}"#, id(2)),
        // A literal cut short where a kept field is `null`.
        format!(r#"{{"id":"{}","studio":nul,"title":"x"}}"#, id(2)),
        // A performer cut mid-object.
        format!(
            r#"{{"id":"{}","performers":[{{"performer":{{"id":"{}" "name":"x"}}}}]}}"#,
            id(2),
            id(9)
        ),
        // A raw control character in a name.
        format!(
            r#"{{"id":"{}","studio":{{"id":"{}","name":"a{}b"}}}}"#,
            id(2),
            id(9),
            '\u{1}'
        ),
    ] {
        let body = format!(r#"{{"data":{{"searchScene":[{good},{bad},{after}]}}}}"#);
        let f = parse(body.as_bytes());
        assert_eq!(f.page.stop, Some(Stop::Malformed), "{bad}");
        assert_eq!(f.page.kept, 1, "{bad}");
        assert_eq!(f.page.skipped, Skipped::default(), "{bad}");
        assert_eq!(f.scenes[0].matched.title.as_deref(), Some("Kept"));
    }
    // The same document cut short anywhere keeps only whole scenes.
    let body = format!(r#"{{"data":{{"searchScene":[{good},{good}]}}}}"#);
    for cut in 0..body.len() {
        let f = parse(&body.as_bytes()[..cut]);
        let whole = body[..cut].matches("\"Kept\"}").count();
        assert_eq!(f.page.kept as usize, whole, "{cut}");
        assert_eq!(f.page.stop, Some(Stop::Malformed), "{cut}");
    }
}

#[test]
fn field_faults_skip_the_scene() {
    let s = |fields: &str| {
        format!(
            r#"{{"data":{{"findScene":{{"id":"{}",{fields}}}}}}}"#,
            id(1)
        )
    };
    let perf = |p: &str| s(&format!(r#""performers":[{p}]"#));
    let malformed = [
        s(r#""title":1"#),
        s(r#""title":["a"]"#),
        s(r#""title":true"#),
        s(r#""release_date":"2024-02-30""#),
        s(r#""release_date":20240101"#),
        s(r#""studio":"Example""#),
        s(r#""studio":{"name":"Example Studio"}"#),
        s(&format!(r#""studio":{{"id":"{}"}}"#, id(2))),
        s(&format!(r#""studio":{{"id":"{}","name":"  "}}"#, id(2))),
        s(&format!(r#""studio":{{"id":"{}","name":null}}"#, id(2))),
        s(r#""performers":{}"#),
        perf("null"),
        perf(r#"{"as":"x"}"#),
        perf(r#"{"performer":null}"#),
        perf(&format!(
            r#"{{"performer":{{"id":"{}","name":"​"}}}}"#,
            id(2)
        )),
        perf(r#"{"performer":{"id":"x","name":"Example"}}"#),
        // A repeated key: which one the server meant is unclear.
        s(r#""title":"a","title":"b""#),
        s(&format!(r#""id":"{}""#, id(1))),
        perf(&format!(
            r#"{{"performer":{{"id":"{}","name":"A","name":"B"}}}}"#,
            id(2)
        )),
        perf(&format!(
            r#"{{"performer":{{"id":"{0}","name":"A"}},"performer":{{"id":"{0}","name":"A"}}}}"#,
            id(2)
        )),
        // Not an object.
        r#"{"data":{"findScene":"x"}}"#.to_string(),
        r#"{"data":{"searchScene":[null]}}"#.to_string(),
    ];
    for body in &malformed {
        let f = parse(body.as_bytes());
        assert_eq!(
            (f.page.kept, f.page.skipped, f.page.stop),
            (
                0,
                Skipped {
                    malformed: 1,
                    ..Skipped::default()
                },
                None
            ),
            "{body}"
        );
    }
    let over = [
        s(&format!(r#""title":"{}""#, "t".repeat(MAX_TITLE + 1))),
        s(&format!(
            r#""studio":{{"id":"{}","name":"{}"}}"#,
            id(2),
            "n".repeat(MAX_NAME + 1)
        )),
        s(&format!(r#""title":"{}""#, "t".repeat(LIMITS.max_text + 1))),
        s(&format!(
            r#""tags":[{}]"#,
            vec!["1"; LIMITS.max_array + 1].join(",")
        )),
    ];
    for body in &over {
        let f = parse(body.as_bytes());
        assert_eq!(
            (f.page.kept, f.page.skipped.over_cap, f.page.skipped.total()),
            (0, 1, 1),
            "{}",
            &body[..80]
        );
    }
    // At the caps exactly, kept; nulls and empties are absent fields.
    let at = s(&format!(
        r#""title":"{}","studio":{{"id":"{}","name":"{}"}},"release_date":null,"performers":null"#,
        "t".repeat(MAX_TITLE),
        id(2),
        "n".repeat(MAX_NAME)
    ));
    let f = parse(at.as_bytes());
    assert_eq!(f.page.kept, 1);
    let blank = s(r#""title":" ​ ""#);
    assert_eq!(parse(blank.as_bytes()).scenes[0].matched.title, None);
    // No id, or a null one.
    for body in [
        r#"{"data":{"findScene":{"title":"x"}}}"#,
        r#"{"data":{"findScene":{"id":null}}}"#,
    ] {
        assert_eq!(parse(body.as_bytes()).page.skipped.no_id, 1, "{body}");
    }
    // Unknown keys and shapes are read past.
    for body in [
        r#"{"data":{"queryScenes":{"scenes":[{"id":"x"}]}}}"#,
        r#"{"data":{"searchScene":{"id":"x"}}}"#,
        r#"[{"data":1}]"#,
        r#"{"extensions":{"cost":1}}"#,
        r#"null"#,
    ] {
        assert_eq!(parse(body.as_bytes()), Found::default(), "{body}");
    }
}

// --- Every cap stops exactly -----------------------------------------

/// A search answer with `n` scenes, each with an id and title only.
fn scenes(n: usize, title: &str) -> String {
    let one: Vec<String> = (0..n)
        .map(|i| format!(r#"{{"id":"{}","title":"{title}"}}"#, id(i as u32)))
        .collect();
    format!(r#"{{"data":{{"searchScene":[{}]}}}}"#, one.join(","))
}

proptest! {
    #[test]
    fn item_cap_is_exact(n in 0usize..40, cap in 1usize..40) {
        let limits = Limits { max_items: cap, ..LIMITS };
        let f = with(scenes(n, "t").as_bytes(), &limits);
        prop_assert_eq!(f.scenes.len(), n.min(cap));
        prop_assert_eq!(f.page.stop, (n > cap).then_some(Stop::Items));
    }

    #[test]
    fn value_cap_is_exact(n in 0usize..40, cap in 1usize..200) {
        // The root, `data` and the list, then each scene, its id and
        // its title.
        let values = 3 + 3 * n;
        let limits = Limits { max_values: cap, ..LIMITS };
        let f = with(scenes(n, "t").as_bytes(), &limits);
        prop_assert_eq!(f.page.stop, (values > cap).then_some(Stop::Elements));
        if values <= cap {
            prop_assert_eq!(f.scenes.len(), n);
        } else {
            prop_assert_eq!(f.scenes.len(), cap.saturating_sub(3) / 3);
        }
    }

    #[test]
    fn byte_cap_is_exact(n in 0usize..10, cut in 0usize..2000) {
        let body = scenes(n, "t");
        let limits = Limits { max_bytes: cut, ..LIMITS };
        let f = with(body.as_bytes(), &limits);
        prop_assert_eq!(f.page.stop.is_some(), body.len() > cut);
        if body.len() > cut {
            prop_assert_eq!(f.page.stop, Some(Stop::Bytes));
        }
    }

    #[test]
    fn depth_cap_is_exact(nest in 0usize..20, cap in 1usize..24) {
        // Root, `data`, the list and the scene are four levels; an
        // unknown field nests `nest` more.
        let deep = format!("{}{}", "[".repeat(nest), "]".repeat(nest));
        let deep = if nest == 0 { "1".to_string() } else { deep };
        let body = format!(
            r#"{{"data":{{"searchScene":[{{"id":"{}","x":{deep}}}]}}}}"#,
            id(1)
        );
        let limits = Limits { max_depth: cap, max_array: 100, ..LIMITS };
        let f = with(body.as_bytes(), &limits);
        let depth = 4 + nest;
        prop_assert_eq!(f.page.stop, (depth > cap).then_some(Stop::Depth));
        prop_assert_eq!(f.scenes.len(), usize::from(depth <= cap));
    }

    #[test]
    fn text_cap_is_exact(len in 36usize..120, cap in 36usize..120) {
        // Ids are 36 bytes; the title is `len`.
        let limits = Limits { max_text: cap, ..LIMITS };
        let f = with(scenes(1, &"t".repeat(len)).as_bytes(), &limits);
        prop_assert_eq!(f.page.kept, u32::from(len <= cap));
        prop_assert_eq!(f.page.skipped.over_cap, u32::from(len > cap));
        prop_assert_eq!(f.page.stop, None);
    }

    #[test]
    fn performer_cap_is_exact(n in 0usize..20, cap in 0usize..20) {
        let list: Vec<String> = (0..n)
            .map(|i| format!(r#"{{"performer":{{"id":"{}","name":"P{i}"}}}}"#, id(i as u32)))
            .collect();
        let body = format!(
            r#"{{"data":{{"findScene":{{"id":"{}","performers":[{}]}}}}}}"#,
            id(99),
            list.join(",")
        );
        let limits = Limits { max_performers: cap, ..LIMITS };
        let f = with(body.as_bytes(), &limits);
        prop_assert_eq!(f.page.kept, u32::from(n <= cap));
        prop_assert_eq!(f.page.skipped.over_cap, u32::from(n > cap));
        if n <= cap {
            prop_assert_eq!(f.scenes[0].performers.len(), n);
        }
    }

    #[test]
    fn array_cap_is_exact(n in 0usize..20, cap in 0usize..20) {
        let tags = vec!["{}"; n].join(",");
        let body = format!(
            r#"{{"data":{{"findScene":{{"id":"{}","tags":[{tags}]}}}}}}"#,
            id(1)
        );
        let limits = Limits { max_array: cap, ..LIMITS };
        let f = with(body.as_bytes(), &limits);
        prop_assert_eq!(f.page.kept, u32::from(n <= cap));
        prop_assert_eq!(f.page.skipped.over_cap, u32::from(n > cap));
    }
}

// --- Property tests --------------------------------------------------

/// Tight caps picked by one byte, as the fuzz target does.
fn small(pick: u8) -> Limits {
    Limits {
        max_bytes: 1 + usize::from(pick) * 64,
        max_depth: 1 + usize::from(pick % 9),
        max_values: 1 + usize::from(pick) * 4,
        max_items: 1 + usize::from(pick % 4),
        max_performers: usize::from(pick % 5),
        max_array: usize::from(pick % 6),
        max_text: usize::from(pick) * 2,
    }
}

/// A JSON escape of `c`: a backslash, `u` and four hex digits a unit.
fn escape(c: char, out: &mut String) {
    let mut units = [0u16; 2];
    for u in c.encode_utf16(&mut units) {
        out.push_str(&format!("{}u{:04x}", '\\', u));
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
            c if c < ' ' || (esc && !c.is_ascii()) => escape(c, &mut out),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Pieces of stash-box-shaped JSON, well-formed or not.
fn fragment() -> impl Strategy<Value = String> {
    let u = id(7);
    prop_oneof![
        Just("{".to_string()),
        Just("}".to_string()),
        Just("[".to_string()),
        Just("]".to_string()),
        Just(",".to_string()),
        Just(":".to_string()),
        Just("null".to_string()),
        Just(r#""data":{"#.to_string()),
        Just(r#""searchScene":["#.to_string()),
        Just(r#""findScene":"#.to_string()),
        Just(r#""errors":[{"message":"x"}],"#.to_string()),
        Just(format!(r#""id":"{u}","#)),
        Just(format!(r#"{{"id":"{u}","title":"t"}},"#)),
        Just(r#""title":"Synthetic","#.to_string()),
        Just(r#""release_date":"2024-01-02","#.to_string()),
        Just(format!(r#""studio":{{"id":"{u}","name":"S"}},"#)),
        Just(format!(
            r#""performers":[{{"performer":{{"id":"{u}","name":"P"}}}}],"#
        )),
        Just(r#""birth_date":"PERSONAL","#.to_string()),
        Just(format!(r#""title":"{}d800","#, '\\')),
        Just(format!(r#""title":"{}u202e{}u200b","#, '\\', '\\')),
        "[a-z{}\\[\\]\":,0-9\\\\ \u{202E}\u{200B}\u{FEFF}\u{2028}-]{0,12}",
    ]
}

/// A generated scene: whether it's good, and its parts. Bad ones carry a
/// fault at a random place, after some good fields.
#[derive(Debug, Clone)]
struct Gen {
    id: u32,
    title: String,
    esc: bool,
    date: Option<(u16, u8, u8)>,
    studio: Option<String>,
    performers: Vec<String>,
    /// `None` is a good scene; else which fault, and where it goes.
    fault: Option<(u8, usize)>,
}

fn generated() -> impl Strategy<Value = Gen> {
    (
        any::<u32>(),
        "[a-zA-Z0-9 é—😀\"\\\\/]{1,40}",
        any::<bool>(),
        proptest::option::of((1u16..=9999, 1u8..=12, 1u8..=28)),
        proptest::option::of("[A-Za-z ]{0,10}[A-Za-z]"),
        proptest::collection::vec("[A-Za-z][A-Za-z ]{0,10}[A-Za-z]", 0..5),
        proptest::option::of((0u8..6, any::<usize>())),
    )
        .prop_map(|(id, title, esc, date, studio, performers, fault)| Gen {
            id,
            title,
            esc,
            date,
            studio,
            performers,
            fault,
        })
}

impl Gen {
    fn body(&self) -> String {
        let mut fields = vec![
            format!(r#""id":"{}""#, id(self.id)),
            format!(r#""title":{}"#, json(&self.title, self.esc)),
        ];
        if let Some((y, m, d)) = self.date {
            fields.push(format!(r#""release_date":"{y:04}-{m:02}-{d:02}""#));
        }
        if let Some(s) = &self.studio {
            fields.push(format!(
                r#""studio":{{"id":"{}","name":{}}}"#,
                id(self.id ^ 1),
                json(s, self.esc)
            ));
        }
        let mut ps: Vec<String> = self
            .performers
            .iter()
            .enumerate()
            .map(|(i, n)| {
                format!(
                    r#"{{"performer":{{"id":"{}","name":{},"birth_date":"PERSONAL"}}}}"#,
                    id(self.id.wrapping_add(i as u32 + 2)),
                    json(n, self.esc)
                )
            })
            .collect();
        if let Some((kind, at)) = self.fault {
            let fault = match kind {
                0 => r#"{"performer":{"name":"No Id"}}"#.to_string(),
                1 => format!(r#"{{"performer":{{"id":"{}"}}}}"#, id(5)),
                2 => r#"{"performer":7}"#.to_string(),
                3 => format!(r#""release_date":"{}""#, "2024-13-01"),
                4 => r#""studio":{"id":"bad","name":"S"}"#.to_string(),
                _ => format!(r#""title":"{}""#, "x".repeat(MAX_TITLE + 1)),
            };
            if kind < 3 {
                ps.insert(at % (ps.len() + 1), fault);
            } else {
                // Replaces the field of the same name, if any, so the
                // key isn't repeated.
                let key = &fault[..fault.find(':').unwrap()];
                fields.retain(|f| !f.starts_with(key));
                fields.insert(at % (fields.len() + 1), fault);
            }
        }
        fields.push(format!(r#""performers":[{}]"#, ps.join(",")));
        format!("{{{}}}", fields.join(","))
    }

    fn expect(&self) -> Option<Scene> {
        if self.fault.is_some() {
            return None;
        }
        let clean = |s: &str| s.chars().filter(|&c| !hidden_char(c)).collect::<String>();
        Some(Scene {
            matched: Match {
                kind: Kind::Scene,
                id: id(self.id),
                title: Some(clean(&self.title).trim().to_owned()).filter(|t| !t.is_empty()),
                studio: self.studio.as_ref().map(|s| Studio {
                    id: id(self.id ^ 1),
                    name: s.trim().to_owned(),
                }),
                released: self.date.map(|(y, m, d)| Date {
                    year: y,
                    month: Some(m),
                    day: Some(d),
                }),
            },
            performers: self
                .performers
                .iter()
                .enumerate()
                .map(|(i, n)| Performer {
                    id: id(self.id.wrapping_add(i as u32 + 2)),
                    name: n.clone(),
                })
                .collect(),
        })
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
        for root in [r#"{"data":{"searchScene":["#, r#"{"data":{"findScene":"#, ""] {
            let body = format!("{root}{}", parts.concat());
            check(&parse(body.as_bytes()), &LIMITS);
            let limits = small(pick);
            check(&with(body.as_bytes(), &limits), &limits);
        }
    }

    /// Good scenes read back exactly; a scene with a fault anywhere is
    /// skipped whole, and nothing of it (not one performer) reaches any
    /// other scene.
    #[test]
    fn skipped_scenes_never_leak(gens in proptest::collection::vec(generated(), 0..12)) {
        let body: Vec<String> = gens.iter().map(Gen::body).collect();
        let body = format!(r#"{{"data":{{"searchScene":[{}]}}}}"#, body.join(","));
        let f = parse(body.as_bytes());
        check(&f, &LIMITS);
        prop_assert_eq!(f.page.stop, None);
        let want: Vec<Scene> = gens.iter().filter_map(Gen::expect).collect();
        prop_assert_eq!(&f.scenes, &want);
        let bad = gens.iter().filter(|g| g.fault.is_some()).count();
        prop_assert_eq!(f.page.skipped.total() as usize, bad);
        let out = format!("{:?}", f.scenes);
        prop_assert!(!out.contains("No Id") && !out.contains("PERSONAL"));
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

/// The fuzz target's checks over 200,000 inputs: random bytes, and the
/// fixtures with bytes flipped, inserted, deleted, duplicated and
/// spliced.
#[test]
fn mutated_corpus() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stashdb");
    let mut seeds: Vec<Vec<u8>> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    seeds.sort();
    let tokens: [&[u8]; 12] = [
        b"{",
        b"}",
        b"[",
        b"]",
        b",",
        b"\"",
        b"\\",
        b":",
        b"null",
        b"\"performer\"",
        b"\xff",
        b"\"id\":\"00000000-0000-4000-8000-000000000001\",",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut kept, mut performers) = (0, 0);
    for n in 0..200_000 {
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
        let f = parse(&input);
        check(&f, &LIMITS);
        kept += f.scenes.len();
        performers += f.scenes.iter().map(|s| s.performers.len()).sum::<usize>();
        let limits = small(pick);
        check(&with(&input, &limits), &limits);
    }
    // The mutations leave many answers whole enough to reach the scenes.
    eprintln!("kept {kept} scenes with {performers} performers");
    assert!(kept > 30_000 && performers > 15_000, "{kept} {performers}");
}
