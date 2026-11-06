//! ThePornDB answer parsing never panics, and never keeps more than the
//! caps allow. The first byte picks tight caps, so fuzzed answers reach
//! every cap; the rest is the body. `crates/sources-feeds/tests/tpdb.rs`
//! runs the same checks over a mutated corpus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::enrich::Lookup;
use lodestar_sources_feeds::tpdb::{self, LIMITS, Limits, MAX_ID, MAX_NAME, MAX_TITLE, Seen, Stop};
use lodestar_sources_plugins::tpdb as plugin;

fn is_id(s: &str) -> bool {
    (1..=MAX_ID).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn clean(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(hidden_char) && s.trim() == s
}

/// A second, independent reader of the same bytes (`serde_json`), for a
/// differential check of what the plugin accepted. It refuses two things
/// ENR-2b's reader accepts in values it reads past: a lone surrogate escape
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
        max_depth: 1 + usize::from(pick % 8),
        max_values: 1 + usize::from(pick) * 4,
        max_items: 1 + usize::from(pick % 4),
        max_array: usize::from(pick % 6),
        max_text: usize::from(pick) * 4,
        max_performers: usize::from(pick % 5),
    };
    for limits in [&LIMITS, &small] {
        let mut kept = 0usize;
        let page = tpdb::parse_each(body, limits, |f| {
            kept += 1;
            let m = &f.matched;
            assert!(is_id(&m.id));
            assert!(m.title.as_deref().is_none_or(|t| clean(t, MAX_TITLE)));
            assert!(m.studio.as_deref().is_none_or(|s| clean(s, MAX_NAME)));
            assert!(m.released.as_deref().is_none_or(|d| d.len() == 10));
            assert!(f.performers.list.len() <= limits.max_performers);
            for p in &f.performers.list {
                assert!(clean(&p.name, MAX_NAME));
            }
        });
        assert_eq!(page.kept as usize, kept);
        assert!(kept + page.skipped.total() as usize <= limits.max_items);
        // What the page says it saw agrees with what it kept.
        assert!(!page.trailing || page.stop.is_some());
        let items = kept + page.skipped.total() as usize;
        assert!(items == 0 || matches!(page.data, Seen::List | Seen::Object));
        assert!(page.data != Seen::Object || items <= 1);
        assert!(!page.data_repeated || page.data != Seen::Absent);
    }
    // The plugin decides on those flags alone, and never takes a miss or a
    // match from an answer that isn't whole and plain.
    let page = tpdb::parse_each(body, &plugin::ANSWER_LIMITS, drop);
    if let Ok(found) = plugin::parse_answer(body) {
        assert!(matches!(page.stop, None | Some(Stop::Items)));
        assert!(!page.trailing && !page.data_repeated);
        assert_eq!(page.skipped.total(), 0);
        match found {
            Lookup::NoMatch if page.kept == 0 => {
                assert_eq!(page.stop, None);
                assert!(matches!(page.data, Seen::List | Seen::Null));
            }
            // A kept match whose id the section refuses is no match too.
            Lookup::NoMatch | Lookup::Match(_) => {
                assert_eq!(page.kept, 1);
                assert!(matches!(page.data, Seen::List | Seen::Object));
            }
        }
        // The flags are true: a second reader sees the same `data`.
        if let Some(v) = second_reader(body) {
            let data = &v["data"];
            let top = match data {
                serde_json::Value::Array(a) => {
                    assert_eq!(a.len().min(1), page.kept as usize);
                    a.first()
                }
                serde_json::Value::Object(_) => Some(data),
                serde_json::Value::Null => {
                    assert!(v.get("data").is_some(), "a miss needs `data`");
                    None
                }
                other => panic!("accepted a `data` of {other}"),
            };
            if let Ok(Lookup::Match(s)) = plugin::parse_answer(body) {
                assert_eq!(top.and_then(|t| id_of(&t["id"])), Some(s.scene_id));
            }
        }
    }
});
