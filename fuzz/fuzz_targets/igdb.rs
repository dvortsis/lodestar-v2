//! IGDB answer parsing (ENR-igdb) never panics and never keeps more than
//! the caps allow: the `/v4/games` answer and Twitch's token answer. The
//! first byte picks tight caps (bit 0); the rest is the body. A kept token
//! is always header-safe, and the plugin's choice always names a game the
//! answer held, marked adult exactly when it has the Erotic theme.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::hidden_char;
use lodestar_sources::identify::{Lookup, MediaKind, Query};
use lodestar_sources_feeds::igdb::{
    LIMITS, Limits, MAX_EXPIRES_IN, MAX_TITLE, THEME_EROTIC, TOKEN_LIMITS, parse_games, parse_token,
    token_ok,
};
use lodestar_sources_plugins::igdb::choose;

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let l = if pick & 1 == 1 {
        Limits {
            max_bytes: 64 + usize::from(pick >> 1) * 64,
            max_depth: 2 + usize::from(pick >> 5),
            max_values: 4 + usize::from(pick >> 2) * 8,
            max_array: 1 + usize::from(pick >> 4),
            max_text: 1 + usize::from(pick >> 3),
            max_items: usize::from(pick >> 6),
            max_ids: usize::from(pick >> 5),
        }
    } else {
        LIMITS
    };
    let s = parse_games(body, &l);
    assert!(s.hits.len() <= l.max_items);
    for h in &s.hits {
        assert!(h.id > 0 && h.genres.len() <= l.max_ids);
        // The Erotic theme is kept even past the cap, as the one extra.
        if let Some(t) = &h.themes {
            assert!(t.len() <= l.max_ids + 1);
            if t.len() > l.max_ids {
                assert_eq!(t.last(), Some(&THEME_EROTIC));
                assert_eq!(t.iter().filter(|&&x| x == THEME_EROTIC).count(), 1);
            }
        }
        if let Some(t) = &h.title {
            assert!(!t.is_empty() && t.len() <= MAX_TITLE);
            assert!(!t.chars().any(hidden_char));
        }
        assert!(h.year.is_none_or(|y| (1..=9999).contains(&y)));
    }
    for h in s.hits.iter().take(4) {
        let Some(t) = &h.title else { continue };
        let Some(q) = Query::new(t, h.year, MediaKind::Game) else {
            continue;
        };
        if let Lookup::Match(i) = choose(&q, &s.hits) {
            assert_eq!(i.ids.len(), 1);
            let id = i.ids[0].value.parse::<u64>().unwrap();
            assert!(s.hits.iter().any(|x| x.id == id && x.adult() == i.adult));
        }
    }
    if let Some(t) = parse_token(body, &TOKEN_LIMITS) {
        assert!(token_ok(&t.access_token));
        assert!((1..=MAX_EXPIRES_IN).contains(&t.expires_in));
    }
});
