//! AniDB answer parsing (ENR-anidb) never panics, never inflates or keeps
//! more than the caps allow, and the plugin's choice never names an anime
//! the answer didn't hold. The first byte picks the parser (bit 0: the
//! titles dump, else one anime's record), tight caps (bit 1) so fuzzed
//! answers reach every cap, and whether the rest is gzipped first
//! (bit 2) so the XML behind the inflater is reached too; the rest is the
//! body. `crates/sources-feeds/tests/anidb.rs` runs the same checks as
//! property tests.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::anidb::{
    ANIME_LIMITS, Aid, Anime, DUMP_LIMITS, Limits, MAX_LANG, MAX_TITLE, XmlLimits, gzip,
    parse_anime, parse_titles,
};
use lodestar_sources_plugins::anidb::choose;

fn tight(pick: u8) -> Limits {
    Limits {
        max_compressed: 256 + usize::from(pick >> 3) * 128,
        xml: XmlLimits {
            max_bytes: 256 + usize::from(pick >> 3) * 256,
            max_depth: 2 + usize::from(pick >> 5),
            max_elements: 4 + usize::from(pick >> 3) * 4,
            max_items: 1 + usize::from(pick >> 6),
            max_attrs: 1 + usize::from(pick >> 5),
            max_attr_len: 4 + usize::from(pick >> 3),
            max_text: 1 + usize::from(pick >> 3) * 4,
        },
        max_titles: usize::from(pick >> 5),
        max_all_titles: usize::from(pick >> 4),
    }
}

fn bounded(a: &Anime, l: &Limits) {
    assert_eq!(Aid::parse(&a.aid.to_string()), Some(a.aid));
    assert!(a.titles.len() <= l.max_titles);
    for t in &a.titles {
        assert!(!t.text.is_empty() && t.text.len() <= MAX_TITLE);
        assert!(t.text.trim() == t.text && !t.text.chars().any(hidden_char));
        assert!(t.lang.len() <= MAX_LANG);
        assert!(t.lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    }
    for y in [a.start_year, a.end_year].into_iter().flatten() {
        assert!((1..=9999).contains(&y));
    }
}

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let gzipped;
    let body = if pick & 4 == 4 {
        gzipped = gzip(body);
        &gzipped[..]
    } else {
        body
    };
    if pick & 1 == 1 {
        let l = if pick & 2 == 2 { tight(pick) } else { DUMP_LIMITS };
        let (all, page) = parse_titles(body, &l);
        assert_eq!(page.kept as usize, all.len());
        assert!(all.len() <= l.xml.max_items);
        assert!(all.iter().map(|a| a.titles.len()).sum::<usize>() <= l.max_all_titles);
        for a in &all {
            bounded(a, &l);
            assert_eq!(a.restricted, None, "a dump entry has no marker");
        }
        return;
    }
    let l = if pick & 2 == 2 { tight(pick) } else { ANIME_LIMITS };
    let (anime, page) = parse_anime(body, &l);
    let Some(a) = anime else {
        assert_eq!(page.kept, 0);
        return;
    };
    assert_eq!(page.kept, 1);
    bounded(&a, &l);
    // Whatever the plugin picks is this anime, carrying its marker.
    for t in a.titles.iter().take(2) {
        for kind in [MediaKind::Tv, MediaKind::Movie] {
            let Some(q) = Query::new(&t.text, a.start_year, kind) else {
                continue;
            };
            if let Lookup::Match(i) = choose(&q, &a) {
                assert_eq!(i.ids.len(), 1);
                assert_eq!(i.ids[0].scheme, "anidb");
                assert_eq!(i.ids[0].value, a.aid.to_string());
                assert_eq!(i.adult, a.restricted);
                assert!(i.into_claims().details.is_some());
            }
        }
    }
});
