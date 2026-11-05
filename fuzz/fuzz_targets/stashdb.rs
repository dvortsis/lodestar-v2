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
    }
});
