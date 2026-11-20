//! The Steam plugin (ENR-steam) against the synthetic fixtures in
//! `fixtures/steam/`: the whole list is fetched once, titles are matched
//! locally, and no request carries a title or the key in its URL.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lodestar_sources::identify::{Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::{Exchange, Replayer, parse_exchange};
use lodestar_sources::{
    BoxFuture, Fetch, HttpError, Kit, MemoryState, Request, Response, UserLimits,
};
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::steam::{self, Limits, Steam};

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
    for (key, msg) in [
        (None, steam::NO_CREDENTIALS),
        (Some(FAKE_KEY), steam::BAD_CREDENTIALS),
    ] {
        let p = plugin(key);
        let k = kit(replayer.clone(), &p);
        let e = p.lookup(&k, &game("Example Quest")).await.unwrap_err();
        assert_eq!(e.0, msg);
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
    // Accepted as a title, but nothing is left once folded.
    let q = Query::new("!!!", None, MediaKind::Game).unwrap();
    assert!(steam::fold(q.title()).is_empty());
    assert!(matches!(p.lookup(&k, &q).await, Ok(Lookup::NoMatch)));
    assert!(replayer.requests().is_empty());
}

/// One synthetic page exchange: the list request after `after`, and
/// `status` with `body`.
fn page(after: Option<u32>, status: u16, body: &str) -> Exchange {
    let mut url = format!(
        "https://{}{}?include_games=true&include_dlc=false&include_software=false&include_videos=false&include_hardware=false&max_results={}",
        steam::HOST,
        steam::API,
        steam::PAGE
    );
    if let Some(a) = after {
        url.push_str(&format!("&last_appid={a}"));
    }
    let head = format!(
        "GET {url}\nAccept: application/json\n\nHTTP {status}\nContent-Type: application/json\n"
    );
    parse_exchange(&head, body.as_bytes().to_vec(), Vec::new()).unwrap()
}

/// A page body: `apps` as (id, name), then the paging fields.
fn body(apps: &[(u32, &str)], more: bool, last: Option<u32>) -> String {
    let apps: Vec<String> = apps
        .iter()
        .map(|(id, n)| format!(r#"{{"appid":{id},"name":"{n}"}}"#))
        .collect();
    let last = last.map_or(String::new(), |l| format!(r#","last_appid":{l}"#));
    format!(
        r#"{{"response":{{"apps":[{}],"have_more_results":{more}{last}}}}}"#,
        apps.join(",")
    )
}

fn id_of(l: &Lookup) -> Option<String> {
    match l {
        Lookup::Match(i) => Some(i.ids[0].value.clone()),
        Lookup::NoMatch => None,
    }
}

/// Runs one lookup against `exchanges` and returns its error; checks
/// that every exchange was asked for and that a second lookup, inside
/// the retry wait, sends nothing more.
async fn fails_with(limits: Limits, exchanges: Vec<Exchange>) -> String {
    let n = exchanges.len();
    let replayer = Arc::new(Replayer::new(exchanges));
    let p = Steam::with_limits(
        Arc::new(StaticCredentials::new(Some((steam::API_KEY, HEX_KEY)))),
        limits,
    );
    let k = kit(replayer.clone(), &p);
    let e = p.lookup(&k, &game("Example Quest")).await.unwrap_err();
    replayer.finish().unwrap();
    assert_eq!(
        p.lookup(&k, &game("Example Quest")).await.unwrap_err().0,
        steam::HELD
    );
    assert_eq!(replayer.requests().len(), n, "paging went on after {}", e.0);
    e.0
}

#[tokio::test(start_paused = true)]
async fn a_stuck_backward_or_missing_cursor_stops_paging() {
    let first = page(None, 200, &body(&[(70, "Example Quest")], true, Some(400)));
    for second in [
        body(&[(500, "Other")], true, Some(400)),
        body(&[(500, "Other")], true, Some(300)),
        body(&[(500, "Other")], true, None),
    ] {
        let e = fails_with(
            Limits::DEFAULT,
            vec![first.clone(), page(Some(400), 200, &second)],
        )
        .await;
        assert_eq!(e, steam::BAD_ANSWER, "{second}");
    }
    // Missing on the first page too.
    let e = fails_with(
        Limits::DEFAULT,
        vec![page(None, 200, &body(&[(70, "Example Quest")], true, None))],
    )
    .await;
    assert_eq!(e, steam::BAD_ANSWER);
}

#[tokio::test(start_paused = true)]
async fn each_cap_gives_too_long() {
    let two = body(
        &[(70, "Example Quest"), (80, "Another One")],
        true,
        Some(80),
    );
    let pages = Limits {
        pages: 1,
        ..Limits::DEFAULT
    };
    let apps = Limits {
        apps: 1,
        ..Limits::DEFAULT
    };
    let bytes = Limits {
        bytes: two.len() - 1,
        ..Limits::DEFAULT
    };
    for lim in [pages, apps, bytes] {
        let e = fails_with(lim, vec![page(None, 200, &two)]).await;
        assert_eq!(e, steam::TOO_LONG, "{lim:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn refused_and_rate_limited_answers() {
    for (status, msg) in [
        (401, steam::REFUSED),
        (403, steam::REFUSED),
        (429, steam::RATE_LIMITED),
    ] {
        let e = fails_with(Limits::DEFAULT, vec![page(None, status, "{}")]).await;
        assert_eq!(e, msg, "{status}");
        assert!(!e.contains(HEX_KEY));
    }
}

#[tokio::test(start_paused = true)]
async fn a_failed_refresh_keeps_the_old_index_and_waits_to_retry() {
    let replayer = Arc::new(Replayer::new(vec![
        page(None, 200, &body(&[(70, "Example Quest")], false, Some(70))),
        // The refresh a day later fails.
        page(None, 500, "{}"),
        // The retry an hour after that brings a new list.
        page(None, 200, &body(&[(90, "Example Quest")], false, Some(90))),
    ]));
    let p = plugin(Some(HEX_KEY));
    let k = kit(replayer.clone(), &p);
    let q = Query::new("Example Quest", Some(2001), MediaKind::Game).unwrap();
    let first = p.lookup(&k, &q).await.unwrap();
    assert_eq!(id_of(&first), Some("70".into()));
    // The list has no years: the item's own year isn't echoed back.
    let Lookup::Match(m) = &first else {
        unreachable!()
    };
    assert_eq!(m.year, None);

    // Inside the day: answered locally.
    tokio::time::advance(steam::REFRESH - Duration::from_secs(1)).await;
    assert_eq!(id_of(&p.lookup(&k, &q).await.unwrap()), Some("70".into()));
    assert_eq!(replayer.requests().len(), 1);

    // A day on: fetched again; it fails, and the old match stands.
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(id_of(&p.lookup(&k, &q).await.unwrap()), Some("70".into()));
    assert_eq!(replayer.requests().len(), 2);

    // Nothing is sent until the retry wait has passed.
    tokio::time::advance(steam::RETRY - Duration::from_secs(1)).await;
    assert_eq!(id_of(&p.lookup(&k, &q).await.unwrap()), Some("70".into()));
    assert_eq!(replayer.requests().len(), 2);

    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(id_of(&p.lookup(&k, &q).await.unwrap()), Some("90".into()));
    replayer.finish().unwrap();
    assert_eq!(replayer.requests().len(), 3);
}

/// Answers from a [`Replayer`], but holds one call (by number) for a
/// day first, so the lookup making it can be cancelled mid-request.
struct Slow {
    inner: Arc<Replayer>,
    slow: usize,
    calls: AtomicUsize,
}

impl Fetch for Slow {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) == self.slow {
                tokio::time::sleep(steam::REFRESH).await;
            }
            self.inner.fetch(req).await
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_refresh_resumes_at_its_cursor() {
    let replayer = Arc::new(Replayer::new(vec![
        page(None, 200, &body(&[(70, "Example Quest")], true, Some(400))),
        page(
            Some(400),
            200,
            &body(&[(620, "Demo Puzzle 2")], false, Some(620)),
        ),
    ]));
    let slow = Arc::new(Slow {
        inner: replayer.clone(),
        slow: 1,
        calls: AtomicUsize::new(0),
    });
    let p = plugin(Some(HEX_KEY));
    let k = kit(slow, &p);
    // The runtime's lookup timeout drops the lookup during page two,
    // 30 s in, which is inside the `CANCELLED` hold.
    let q = game("Example Quest");
    let cut = tokio::time::timeout(Duration::from_secs(30), p.lookup(&k, &q));
    assert!(cut.await.is_err());
    assert_eq!(replayer.requests().len(), 1);
    // Cancelled mid-request: the next try still waits out `CANCELLED`.
    let e = p.lookup(&k, &game("Example Quest")).await.unwrap_err();
    assert_eq!(e.0, steam::HELD);
    tokio::time::advance(steam::CANCELLED).await;
    // It goes on from page two; page one's game was kept.
    let got = p.lookup(&k, &game("Example Quest")).await.unwrap();
    assert_eq!(id_of(&got), Some("70".into()));
    assert_eq!(
        id_of(&p.lookup(&k, &game("Demo Puzzle 2")).await.unwrap()),
        Some("620".into())
    );
    replayer.finish().unwrap();
    assert_eq!(replayer.requests().len(), 2);
}
