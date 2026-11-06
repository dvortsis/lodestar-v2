//! StashDB GraphQL answer parsing never panics, and never keeps more than
//! the caps allow. The first byte picks tight caps, so fuzzed answers
//! reach every cap; the rest is the body. `crates/sources-feeds/tests/
//! stashdb.rs` runs the same checks over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::enrich::Lookup;
use lodestar_sources_feeds::stashdb::{
    self, ID_LEN, Kind, LIMITS, Limits, MAX_NAME, MAX_TITLE, SCENES, Seen, Stop,
};
use lodestar_sources_plugins::stashdb as plugin;

fn uuid(s: &str) -> bool {
    s.len() == ID_LEN
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

fn name(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME && s.trim() == s && !s.chars().any(hidden_char)
}

/// A second, independent reader of the same bytes (`serde_json`), for a
/// differential check of what the plugin accepted. It refuses two things
/// ENR-3's reader accepts in values it reads past: a lone surrogate escape
/// and a number out of `f64` range; `None` then.
fn second_reader(body: &[u8]) -> Option<serde_json::Value> {
    let root = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
    match serde_json::from_slice(root) {
        Ok(v) => Some(v),
        Err(e) => {
            let msg = e.to_string();
            let surrogate = root.windows(3).any(|w| {
                w[0] == b'u'
                    && matches!(w[1], b'd' | b'D')
                    && matches!(w[2], b'8'..=b'9' | b'a'..=b'f' | b'A'..=b'F')
            });
            assert!(
                msg.starts_with("number out of range")
                    || (surrogate
                        && (msg.starts_with("unexpected end of hex escape")
                            || msg.starts_with("lone leading surrogate"))),
                "the plugin accepted what serde_json refuses: {msg}"
            );
            None
        }
    }
}

/// An id as `serde_json` reads it: a string, or a number's digits.
fn id_of(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let small = Limits {
        max_bytes: 1 + usize::from(pick) * 64,
        max_depth: 1 + usize::from(pick % 9),
        max_values: 1 + usize::from(pick) * 4,
        max_items: 1 + usize::from(pick % 4),
        max_performers: usize::from(pick % 5),
        max_array: usize::from(pick % 6),
        max_text: usize::from(pick) * 2,
    };
    for limits in [&LIMITS, &small] {
        let mut kept = 0usize;
        let page = stashdb::parse_each(body, limits, |s| {
            kept += 1;
            let m = &s.matched;
            assert_eq!(m.kind, Kind::Scene);
            assert!(uuid(&m.id));
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
                assert!(name(&p.name));
            }
        });
        assert_eq!(page.kept as usize, kept);
        assert!(kept + page.skipped.total() as usize <= limits.max_items);
        // What the page says it saw agrees with what it kept.
        assert!(!page.trailing || page.stop.is_some());
        let items = kept + page.skipped.total() as usize;
        assert!(items == 0 || page.search == Seen::List || page.find);
    }
    // The plugin decides on those flags alone: never a miss or a match
    // beside GraphQL errors, a `findScene`, a repeated key, or an answer
    // that isn't whole.
    let page = stashdb::parse_each(body, &plugin::ANSWER_LIMITS, drop);
    if let Ok(found) = plugin::parse_answer(body) {
        assert!(matches!(page.stop, None | Some(Stop::Items)));
        assert!(!page.trailing && !page.repeated && !page.find);
        assert_eq!(page.errors, 0);
        assert_eq!(page.search, Seen::List);
        assert_eq!(page.skipped.total(), 0);
        match found {
            Lookup::NoMatch => assert_eq!((page.kept, page.stop), (0, None)),
            Lookup::Match(_) => assert_eq!(page.kept, 1),
        }
        // The flags are true: a second reader sees a `searchScene` list
        // whose first entry is the match, no `findScene` and no `errors`.
        if let Some(v) = second_reader(body) {
            let data = v["data"].as_object().expect("`data` is an object");
            assert!(!data.contains_key("findScene"));
            let list = data.get("searchScene").and_then(|l| l.as_array());
            let list = list.expect("`searchScene` is a list");
            assert_eq!(list.len().min(1), page.kept as usize);
            let errors = &v["errors"];
            assert!(errors.is_null() || errors.as_array().is_some_and(Vec::is_empty));
            if let Ok(Lookup::Match(s)) = plugin::parse_answer(body) {
                let top = list.first().and_then(|t| id_of(&t["id"]));
                assert_eq!(top.map(|t| t.to_ascii_lowercase()), Some(s.scene_id));
            }
        }
    }
});
