//! The Steam plugin (ENR-steam) against the synthetic fixtures in
//! `fixtures/steam/`: the whole list is fetched once, titles are matched
//! locally, and no request carries a title or the key in its URL.

use std::path::PathBuf;
use std::sync::Arc;

use lodestar_sources::identify::{Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::Replayer;
use lodestar_sources::{Fetch, Kit, MemoryState, UserLimits};
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::steam::{self, Steam};

/// A made-up key of the right shape. Not a real key.
const FAKE_KEY: &str = "fakefakefakefakefakefakefakefake";
const HEX_KEY: &str = "0123456789abcdef0123456789abcdef";

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/steam")
        .join(case)
}

fn plugin(key: Option<&'static str>) -> Steam {
    Steam::new(Arc::new(StaticCredentials::new(
        key.map(|k| (steam::API_KEY, k)),
    )))
}

fn kit(fetch: Arc<dyn Fetch>, p: &Steam) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        p.policy(),
        &UserLimits::default(),
    )
}

fn game(title: &str) -> Query {
    Query::new(title, None, MediaKind::Game).unwrap()
}

#[tokio::test(start_paused = true)]
async fn many_lookups_make_one_list_fetch_and_send_no_title() {
    let replayer = Arc::new(Replayer::load(&fixture("steam_list")).unwrap());
    let p = plugin(Some(HEX_KEY));
    let k = kit(replayer.clone(), &p);
    let titles = [
        "Example Quest",
        "sample saga part two",
        "Demo Puzzle 2",
        "Twin Title",
        "Unlisted Adventure",
    ];
    let mut got = Vec::new();
    for t in titles {
        got.push(p.lookup(&k, &game(t)).await.unwrap());
    }
    let ids: Vec<Option<String>> = got
        .iter()
        .map(|l| match l {
            Lookup::Match(i) => Some(format!("{}:{}", i.ids[0].scheme, i.ids[0].value)),
            Lookup::NoMatch => None,
        })
        .collect();
    assert_eq!(
        ids,
        [
            Some("steam:70".into()),
            Some("steam:220".into()),
            Some("steam:620".into()),
            // Two games fold to the same name: no guess.
            None,
            None,
        ]
    );
    replayer.finish().unwrap();
    let reqs = replayer.requests();
    // Two pages, for five lookups: the list, never a title.
    assert_eq!(reqs.len(), 2);
    for (_, r) in &reqs {
        let url = r.url.to_lowercase();
        for t in titles {
            for word in t.to_lowercase().split_whitespace().filter(|w| w.len() > 3) {
                assert!(!url.contains(word), "{url} holds {word}");
            }
        }
        assert!(!url.contains(HEX_KEY));
        assert!(r.body.is_empty());
        assert!(
            r.headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case("x-webapi-key") && v == HEX_KEY)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_cut_short_page_is_an_error_and_waits_before_retrying() {
    let replayer = Arc::new(Replayer::load(&fixture("steam_bad_body")).unwrap());
    let p = plugin(Some(HEX_KEY));
    let k = kit(replayer.clone(), &p);
    let e = p.lookup(&k, &game("Example Quest")).await.unwrap_err();
    assert!(!e.0.contains(HEX_KEY));
    // The retry wait holds: no second request.
    let e = p.lookup(&k, &game("Example Quest")).await.unwrap_err();
    assert_eq!(e.0, steam::HELD);
    assert_eq!(replayer.requests().len(), 1);
}

#[tokio::test]
async fn no_or_bad_key_sends_nothing() {
    let replayer = Arc::new(Replayer::new(Vec::new()));
    for (key, msg) in [(None, steam::NO_CREDENTIALS), (Some(FAKE_KEY), "")] {
        let p = plugin(key);
        let k = kit(replayer.clone(), &p);
        let e = p.lookup(&k, &game("Example Quest")).await.unwrap_err();
        assert!(msg.is_empty() || e.0 == msg);
        assert!(!e.0.contains(FAKE_KEY));
    }
    assert!(replayer.requests().is_empty());
    assert!(Steam::need(&[]).is_some());
}

#[tokio::test]
async fn other_kinds_and_empty_titles_send_nothing() {
    let replayer = Arc::new(Replayer::new(Vec::new()));
    let p = plugin(Some(HEX_KEY));
    let k = kit(replayer.clone(), &p);
    let movie = Query::new("Example Quest", None, MediaKind::Movie).unwrap();
    assert!(matches!(p.lookup(&k, &movie).await, Ok(Lookup::NoMatch)));
    if let Some(q) = Query::new("!!!", None, MediaKind::Game) {
        assert!(matches!(p.lookup(&k, &q).await, Ok(Lookup::NoMatch)));
    }
    assert!(replayer.requests().is_empty());
}
