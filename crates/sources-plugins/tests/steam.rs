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

/// A one-page list body of `n` games, ids 10, 20, …, plus `extra`.
fn many(n: u32, extra: &[(u32, &str)]) -> String {
    let names: Vec<(u32, String)> = (1..=n).map(|i| (i * 10, format!("Filler {i}"))).collect();
    let mut apps: Vec<(u32, &str)> = names.iter().map(|(id, n)| (*id, n.as_str())).collect();
    apps.extend_from_slice(extra);
    body(&apps, false, None)
}

/// Steam's answer when it has no apps at all: a `response` with nothing
/// in it, which parses as a whole, empty page.
const EMPTY: &str = r#"{"response":{}}"#;

/// The plugin's warning as kept in its state store.
async fn stored_warning(k: &Kit) -> Option<String> {
    k.state
        .get(steam::WARNING_KEY)
        .await
        .unwrap()
        .map(|v| String::from_utf8(v).unwrap())
}

#[test]
fn which_finished_lists_are_refused() {
    use steam::{SHRINK_FLOOR, suspicious};
    // The first list: only an empty one.
    assert!(suspicious(None, 0).is_some());
    assert!(suspicious(None, 1).is_none());
    // Empty is refused whatever the list in use.
    assert!(suspicious(Some(1), 0).is_some());
    // Under the floor, any shrink is accepted.
    assert!(suspicious(Some(SHRINK_FLOOR - 1), 1).is_none());
    // At or over it, under half is refused; half or more is accepted.
    assert!(suspicious(Some(SHRINK_FLOOR), SHRINK_FLOOR / 2 - 1).is_some());
    assert!(suspicious(Some(SHRINK_FLOOR), SHRINK_FLOOR / 2).is_none());
    assert!(suspicious(Some(1001), 500).is_some());
    assert!(suspicious(Some(1001), 501).is_none());
    assert!(suspicious(Some(200_000), 180_000).is_none());
    assert!(suspicious(Some(200_000), 250_000).is_none());
    // The warning names both sizes and says it will be tried again.
    let w = suspicious(Some(200_000), 1_234).unwrap();
    assert!(w.contains("1234") && w.contains("200000"), "{w}");
    assert!(w.contains("kept") && w.contains("tried again"), "{w}");
}

#[tokio::test(start_paused = true)]
async fn an_empty_first_list_is_refused_and_a_small_one_accepted() {
    let replayer = Arc::new(Replayer::new(vec![
        page(None, 200, EMPTY),
        // An hour on, a list of one game: small, but the first, so used.
        page(None, 200, &body(&[(70, "Example Quest")], false, None)),
    ]));
    let p = plugin(Some(HEX_KEY));
    let k = kit(replayer.clone(), &p);
    let e = p.lookup(&k, &game("Example Quest")).await.unwrap_err();
    let w = p.warning().expect("a warning is kept");
    assert_eq!(e.0, w);
    assert!(w.contains("empty") && w.contains("tried again"), "{w}");
    assert_eq!(stored_warning(&k).await, Some(w));
    // The retry wait holds: nothing more is sent.
    tokio::time::advance(steam::RETRY - Duration::from_secs(1)).await;
    let e = p.lookup(&k, &game("Example Quest")).await.unwrap_err();
    assert_eq!(e.0, steam::HELD);
    assert_eq!(replayer.requests().len(), 1);

    tokio::time::advance(Duration::from_secs(1)).await;
    let got = p.lookup(&k, &game("Example Quest")).await.unwrap();
    assert_eq!(id_of(&got), Some("70".into()));
    // Accepted: the warning is cleared, in memory and in the store.
    assert_eq!(p.warning(), None);
    assert_eq!(stored_warning(&k).await, None);
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_accepted_list_clears_a_warning_left_from_before_a_restart() {
    let replayer = Arc::new(Replayer::new(vec![page(
        None,
        200,
        &body(&[(70, "Example Quest")], false, None),
    )]));
    let p = plugin(Some(HEX_KEY));
    let k = kit(replayer.clone(), &p);
    k.state
        .set(steam::WARNING_KEY, b"left from before".to_vec())
        .await
        .unwrap();
    let got = p.lookup(&k, &game("Example Quest")).await.unwrap();
    assert_eq!(id_of(&got), Some("70".into()));
    assert_eq!(p.warning(), None);
    assert_eq!(stored_warning(&k).await, None);
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_much_smaller_or_empty_list_keeps_the_current_one() {
    let replayer = Arc::new(Replayer::new(vec![
        // The list in use: 1,200 games, Example Quest as 70.
        page(None, 200, &many(1_199, &[(70_000, "Example Quest")])),
        // A day on: 500 games, under half. Refused.
        page(None, 200, &many(499, &[(90_000, "Example Quest")])),
        // An hour on: empty. Refused.
        page(None, 200, EMPTY),
        // An hour on: 700 games, an ordinary shrink. Accepted.
        page(None, 200, &many(699, &[(90_000, "Example Quest")])),
    ]));
    let p = plugin(Some(HEX_KEY));
    let k = kit(replayer.clone(), &p);
    let q = game("Example Quest");
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("70000".into())
    );
    assert_eq!(p.warning(), None);

    tokio::time::advance(steam::REFRESH).await;
    // The new list is refused; the lookup still answers from the old one.
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("70000".into())
    );
    assert_eq!(replayer.requests().len(), 2);
    let w = p.warning().expect("a warning is kept");
    assert!(
        w.contains("500") && w.contains("1200") && w.contains("kept"),
        "{w}"
    );
    assert_eq!(stored_warning(&k).await, Some(w));
    // Filler from the refused list isn't there; the old list's is.
    assert_eq!(
        id_of(&p.lookup(&k, &game("Filler 1199")).await.unwrap()),
        Some("11990".into())
    );
    // Inside the retry wait: answered locally, nothing sent.
    tokio::time::advance(steam::RETRY - Duration::from_secs(1)).await;
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("70000".into())
    );
    assert_eq!(replayer.requests().len(), 2);

    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("70000".into())
    );
    assert_eq!(replayer.requests().len(), 3);
    let w = p.warning().expect("a warning is kept");
    assert!(w.contains("empty") && w.contains("1200"), "{w}");
    assert_eq!(stored_warning(&k).await, Some(w));

    tokio::time::advance(steam::RETRY).await;
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("90000".into())
    );
    assert_eq!(p.warning(), None);
    assert_eq!(stored_warning(&k).await, None);
    replayer.finish().unwrap();
    assert_eq!(replayer.requests().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_request_holds_for_exactly_cancelled() {
    // The hold the decisions file names.
    assert_eq!(steam::CANCELLED, Duration::from_secs(60));
    let t0 = tokio::time::Instant::now();
    let replayer = Arc::new(Replayer::new(vec![page(
        None,
        200,
        &body(&[(70, "Example Quest")], false, None),
    )]));
    // The first request hangs, so the lookup is cancelled during it.
    let slow = Arc::new(Slow {
        inner: replayer.clone(),
        slow: 0,
        calls: AtomicUsize::new(0),
    });
    let p = plugin(Some(HEX_KEY));
    let k = kit(slow, &p);
    let q = game("Example Quest");
    let cut = tokio::time::timeout(Duration::from_secs(30), p.lookup(&k, &q));
    assert!(cut.await.is_err());
    // One second before the hold ends: still held, nothing sent.
    let left = t0 + steam::CANCELLED - Duration::from_secs(1) - tokio::time::Instant::now();
    tokio::time::advance(left).await;
    assert_eq!(p.lookup(&k, &q).await.unwrap_err().0, steam::HELD);
    assert!(replayer.requests().is_empty());
    // At the end of the hold: the refresh runs.
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(id_of(&p.lookup(&k, &q).await.unwrap()), Some("70".into()));
    replayer.finish().unwrap();
    assert_eq!(replayer.requests().len(), 1);
}

/// Cuts a refresh during page two, then waits until `since_start` after
/// the partial list was started and looks up again. `then` is what the
/// replayer expects next.
async fn after_a_cut(since_start: Duration, then: Exchange) -> (Arc<Replayer>, Lookup) {
    let t0 = tokio::time::Instant::now();
    let replayer = Arc::new(Replayer::new(vec![
        page(None, 200, &body(&[(70, "Example Quest")], true, Some(400))),
        then,
    ]));
    let slow = Arc::new(Slow {
        inner: replayer.clone(),
        slow: 1,
        calls: AtomicUsize::new(0),
    });
    let p = plugin(Some(HEX_KEY));
    let k = kit(slow, &p);
    let q = game("Example Quest");
    let cut = tokio::time::timeout(Duration::from_secs(30), p.lookup(&k, &q));
    assert!(cut.await.is_err());
    assert_eq!(replayer.requests().len(), 1);
    tokio::time::advance(t0 + since_start - tokio::time::Instant::now()).await;
    let got = p.lookup(&k, &q).await.unwrap();
    replayer.finish().unwrap();
    assert_eq!(replayer.requests().len(), 2);
    (replayer, got)
}

#[tokio::test(start_paused = true)]
async fn a_partial_list_inside_its_age_limit_resumes() {
    let then = page(
        Some(400),
        200,
        &body(&[(620, "Demo Puzzle 2")], false, None),
    );
    let (_, got) = after_a_cut(steam::REFRESH - Duration::from_secs(1), then).await;
    // Page one's game was kept: the refresh went on from the cursor.
    assert_eq!(id_of(&got), Some("70".into()));
}

#[tokio::test(start_paused = true)]
async fn a_partial_list_at_its_age_limit_starts_again() {
    let then = page(None, 200, &body(&[(90, "Example Quest")], false, None));
    let (replayer, got) = after_a_cut(steam::REFRESH, then).await;
    // Page one was asked for again, and its new answer used.
    assert_eq!(id_of(&got), Some("90".into()));
    assert!(!replayer.requests()[1].1.url.contains("last_appid"));
}

#[test]
fn shrink_sightings_count_within_tolerance_and_need_a_day() {
    use steam::{MAX_APPS, SHRINK_SPAN, Sightings};
    let day = SHRINK_SPAN.as_secs();
    let t = 10 * day;
    let (s1, ok) = Sightings::see(None, 1_200, 500, t);
    assert!(!ok);
    assert_eq!(
        s1,
        Sightings {
            old: 1_200,
            size: 500,
            first: t,
            count: 1
        }
    );
    // 5% of the first sighting either way still counts; the first stays.
    let (s2, ok) = Sightings::see(Some(s1), 1_200, 525, t + 1_000);
    assert!(!ok);
    assert_eq!((s2.size, s2.first, s2.count), (500, t, 2));
    let (s3, ok) = Sightings::see(Some(s2), 1_200, 475, t + day - 1);
    assert!(!ok, "three, but a second short of a day");
    assert_eq!(s3.count, 3);
    assert!(Sightings::see(Some(s2), 1_200, 475, t + day).1);
    // Two sightings over two days aren't enough.
    assert!(!Sightings::see(Some(s1), 1_200, 500, t + 2 * day).1);
    // Past 5% the count starts again at the new size.
    let (r, ok) = Sightings::see(Some(s2), 1_200, 526, t + day);
    assert!(!ok);
    assert_eq!((r.size, r.first, r.count), (526, t + day, 1));
    // A count kept for another list in use starts again.
    let (r, ok) = Sightings::see(Some(s2), 1_300, 500, t + day);
    assert!(!ok);
    assert_eq!((r.old, r.first, r.count), (1_300, t + day, 1));
    // A clock that went back more than a day starts the count again, so
    // it never accepts on a span measured backwards.
    let (r, ok) = Sightings::see(Some(s2), 1_200, 500, t - 2 * day);
    assert!(!ok);
    assert_eq!((r.first, r.count), (t - 2 * day, 1));
    // A first sighting in the future, even the far future, starts again.
    let future = Sightings {
        first: u64::MAX,
        ..s2
    };
    let (r, ok) = Sightings::see(Some(future), 1_200, 500, t);
    assert!(!ok);
    assert_eq!((r.first, r.count), (t, 1));
    // An empty list is never accepted, whatever the record.
    let many = Sightings {
        size: 1,
        count: 99,
        first: 0,
        ..s1
    };
    assert!(!Sightings::see(Some(many), 1_200, 0, t).1);
    // Round trip; anything else, or no real count, is no record.
    assert_eq!(Sightings::decode(&s3.encode()), Some(s3));
    let huge = MAX_APPS + 1;
    for bad in [
        "".to_string(),
        "1 2 3".into(),
        "1 2 3 4 5".into(),
        "a b c d".into(),
        "1200 -1 2 3".into(),
        "1200 0 5 2".into(),
        "0 500 5 2".into(),
        "1200 500 5 0".into(),
        format!("1200 {huge} 5 2"),
        format!("{huge} 500 5 2"),
        format!("1200 {} 0 2", usize::MAX),
    ] {
        assert_eq!(Sightings::decode(bad.as_bytes()), None, "{bad}");
    }
}

/// Starts from a list of 1,200 games (Example Quest as 70000), then for
/// each `(wait, games)` waits and looks up Example Quest against a new list
/// of that many games (Example Quest as 90000). Returns the ids answered,
/// the plugin and its kit.
async fn shrink_run(steps: &[(Duration, u32)]) -> (Vec<String>, Steam, Kit, Arc<Replayer>) {
    let base = tokio::time::Instant::now();
    let mut exchanges = vec![page(None, 200, &many(1_199, &[(70_000, "Example Quest")]))];
    for (_, n) in steps {
        exchanges.push(page(None, 200, &many(n - 1, &[(90_000, "Example Quest")])));
    }
    let replayer = Arc::new(Replayer::new(exchanges));
    // The wall clock moves with the paused test clock.
    let p = plugin(Some(HEX_KEY)).with_clock(Arc::new(move || {
        1_700_000_000 + (tokio::time::Instant::now() - base).as_secs()
    }));
    let k = kit(replayer.clone(), &p);
    let q = game("Example Quest");
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("70000".into())
    );
    let mut ids = Vec::new();
    for (wait, _) in steps {
        tokio::time::advance(*wait).await;
        ids.push(id_of(&p.lookup(&k, &q).await.unwrap()).unwrap());
    }
    replayer.finish().unwrap();
    assert_eq!(replayer.requests().len(), steps.len() + 1);
    (ids, p, k, replayer)
}

async fn stored_sightings(k: &Kit) -> Option<steam::Sightings> {
    let v = k.state.get(steam::SIGHTINGS_KEY).await.unwrap()?;
    Some(steam::Sightings::decode(&v).expect("sightings decode"))
}

const HOUR: Duration = steam::RETRY;

#[tokio::test(start_paused = true)]
async fn two_sightings_of_a_smaller_list_are_refused() {
    let (ids, p, k, _) = shrink_run(&[(steam::REFRESH, 500), (HOUR, 500)]).await;
    assert_eq!(ids, ["70000", "70000"]);
    // Kept in the state store, so the count survives a restart.
    let s = stored_sightings(&k).await.unwrap();
    assert_eq!((s.size, s.count), (500, 2));
    assert!(p.warning().unwrap().contains("under half"));
}

#[tokio::test(start_paused = true)]
async fn three_sightings_inside_a_day_are_refused() {
    let steps = [(steam::REFRESH, 500), (HOUR, 510), (HOUR, 490)];
    let (ids, _, k, _) = shrink_run(&steps).await;
    assert_eq!(ids, ["70000", "70000", "70000"]);
    assert_eq!(stored_sightings(&k).await.unwrap().count, 3);
}

#[tokio::test(start_paused = true)]
async fn three_sightings_spanning_a_day_are_accepted() {
    let last = steam::SHRINK_SPAN - HOUR;
    let steps = [(steam::REFRESH, 500), (HOUR, 510), (last, 495)];
    let (ids, p, k, _) = shrink_run(&steps).await;
    assert_eq!(ids, ["70000", "70000", "90000"]);
    let note = p.warning().expect("a status line");
    assert_eq!(note, steam::shrink_accepted(1_200, 495));
    assert!(note.contains("1200") && note.contains("495"), "{note}");
    assert_eq!(stored_warning(&k).await, Some(note));
    // The count is done with.
    assert_eq!(stored_sightings(&k).await, None);
}

#[tokio::test(start_paused = true)]
async fn a_size_outside_five_percent_starts_the_count_again() {
    let last = steam::SHRINK_SPAN - HOUR;
    // 540 is 8% over 500.
    let steps = [(steam::REFRESH, 500), (HOUR, 500), (last, 540)];
    let (ids, _, k, _) = shrink_run(&steps).await;
    assert_eq!(ids, ["70000", "70000", "70000"]);
    let s = stored_sightings(&k).await.unwrap();
    assert_eq!((s.size, s.count), (540, 1));
}

#[tokio::test(start_paused = true)]
async fn a_normal_list_starts_the_count_again() {
    // Two sightings, then a normal list (1,150 of 1,200), then the small
    // size again a day later: counted from one, so refused.
    let steps = [
        (steam::REFRESH, 500),
        (HOUR, 500),
        (HOUR, 1_150),
        (steam::REFRESH, 500),
    ];
    let (ids, p, k, _) = shrink_run(&steps).await;
    assert_eq!(ids, ["70000", "70000", "90000", "90000"]);
    let s = stored_sightings(&k).await.unwrap();
    assert_eq!((s.size, s.count), (500, 1));
    assert!(p.warning().unwrap().contains("1150"));
}

#[tokio::test(start_paused = true)]
async fn empty_lists_never_count_towards_accepting() {
    // Empty answers for more than a day, an hour apart.
    let mut steps = vec![(steam::REFRESH, 0)];
    steps.extend(std::iter::repeat_n((HOUR, 0), 25));
    let base = tokio::time::Instant::now();
    let mut exchanges = vec![page(None, 200, &many(1_199, &[(70_000, "Example Quest")]))];
    exchanges.extend(steps.iter().map(|_| page(None, 200, EMPTY)));
    let replayer = Arc::new(Replayer::new(exchanges));
    let p = plugin(Some(HEX_KEY)).with_clock(Arc::new(move || {
        1_700_000_000 + (tokio::time::Instant::now() - base).as_secs()
    }));
    let k = kit(replayer.clone(), &p);
    let q = game("Example Quest");
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("70000".into())
    );
    // A count already under way, for this list in use.
    let before = steam::Sightings {
        old: 1_200,
        size: 500,
        first: 1_700_000_000,
        count: 2,
    };
    k.state
        .set(steam::SIGHTINGS_KEY, before.encode())
        .await
        .unwrap();
    for (wait, _) in &steps {
        tokio::time::advance(*wait).await;
        assert_eq!(
            id_of(&p.lookup(&k, &q).await.unwrap()),
            Some("70000".into())
        );
    }
    replayer.finish().unwrap();
    assert_eq!(replayer.requests().len(), steps.len() + 1);
    assert!(p.warning().unwrap().contains("empty"));
    // The count was neither moved on nor reset.
    assert_eq!(stored_sightings(&k).await, Some(before));
}

/// A plugin and kit whose store holds `stored` under the sightings key,
/// with a list of 1,200 games in use, then one refresh a day on against a
/// list of `size`. Returns the id answered and the stored sightings.
async fn one_shrink_after(stored: &[u8], size: u32) -> (String, Option<steam::Sightings>) {
    let replayer = Arc::new(Replayer::new(vec![
        page(None, 200, &many(1_199, &[(70_000, "Example Quest")])),
        page(None, 200, &many(size - 1, &[(90_000, "Example Quest")])),
    ]));
    let base = tokio::time::Instant::now();
    let p = plugin(Some(HEX_KEY)).with_clock(Arc::new(move || {
        1_700_000_000 + (tokio::time::Instant::now() - base).as_secs()
    }));
    let k = kit(replayer.clone(), &p);
    let q = game("Example Quest");
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("70000".into())
    );
    // Written after the first list: as if its clear had been lost.
    k.state
        .set(steam::SIGHTINGS_KEY, stored.to_vec())
        .await
        .unwrap();
    tokio::time::advance(steam::REFRESH).await;
    let id = id_of(&p.lookup(&k, &q).await.unwrap()).unwrap();
    replayer.finish().unwrap();
    (id, stored_sightings(&k).await)
}

#[tokio::test(start_paused = true)]
async fn a_count_left_for_another_list_in_use_starts_again() {
    // Ready to accept, but kept for a list of 1,000, not this one.
    let stale = steam::Sightings {
        old: 1_000,
        size: 500,
        first: 1_600_000_000,
        count: 9,
    };
    let (id, s) = one_shrink_after(&stale.encode(), 500).await;
    assert_eq!(id, "70000");
    let s = s.unwrap();
    assert_eq!((s.old, s.size, s.count), (1_200, 500, 1));
}

#[tokio::test(start_paused = true)]
async fn a_corrupt_count_is_no_count() {
    let stored = format!("1200 {} 0 2", usize::MAX);
    let (id, s) = one_shrink_after(stored.as_bytes(), 500).await;
    assert_eq!(id, "70000");
    let s = s.unwrap();
    assert_eq!((s.old, s.size, s.count), (1_200, 500, 1));
}

/// A state store that can't be read, and drops every write.
#[derive(Debug)]
struct Broken;

impl lodestar_sources::PluginState for Broken {
    fn get<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, lodestar_sources::StateError>> {
        Box::pin(async { Err(lodestar_sources::StateError::Store("down".into())) })
    }
    fn set<'a>(
        &'a self,
        _: &'a str,
        _: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), lodestar_sources::StateError>> {
        Box::pin(async { Err(lodestar_sources::StateError::Store("down".into())) })
    }
    fn remove<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), lodestar_sources::StateError>> {
        Box::pin(async { Err(lodestar_sources::StateError::Store("down".into())) })
    }
    fn compare_and_set<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a [u8]>,
        _: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<bool, lodestar_sources::StateError>> {
        Box::pin(async { Err(lodestar_sources::StateError::Store("down".into())) })
    }
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_store_counts_each_shrink_as_the_first() {
    // Three sightings spanning a day would be accepted with a store.
    let steps = [
        (steam::REFRESH, 500),
        (HOUR, 500),
        (steam::SHRINK_SPAN - HOUR, 500),
    ];
    let base = tokio::time::Instant::now();
    let mut exchanges = vec![page(None, 200, &many(1_199, &[(70_000, "Example Quest")]))];
    for (_, n) in &steps {
        exchanges.push(page(None, 200, &many(n - 1, &[(90_000, "Example Quest")])));
    }
    let replayer = Arc::new(Replayer::new(exchanges));
    let p = plugin(Some(HEX_KEY)).with_clock(Arc::new(move || {
        1_700_000_000 + (tokio::time::Instant::now() - base).as_secs()
    }));
    let k = Kit::new(
        Arc::new(Broken),
        replayer.clone(),
        p.policy(),
        &UserLimits::default(),
    );
    let q = game("Example Quest");
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("70000".into())
    );
    for (wait, _) in &steps {
        tokio::time::advance(*wait).await;
        assert_eq!(
            id_of(&p.lookup(&k, &q).await.unwrap()),
            Some("70000".into())
        );
    }
    replayer.finish().unwrap();
    // The warning still reaches memory.
    assert!(p.warning().unwrap().contains("under half"));
}

#[tokio::test(start_paused = true)]
async fn a_failed_fetch_leaves_the_count_alone() {
    let replayer = Arc::new(Replayer::new(vec![
        page(None, 200, &many(1_199, &[(70_000, "Example Quest")])),
        page(None, 200, &many(499, &[(90_000, "Example Quest")])),
        page(None, 500, "{}"),
    ]));
    let base = tokio::time::Instant::now();
    let p = plugin(Some(HEX_KEY)).with_clock(Arc::new(move || {
        1_700_000_000 + (tokio::time::Instant::now() - base).as_secs()
    }));
    let k = kit(replayer.clone(), &p);
    let q = game("Example Quest");
    p.lookup(&k, &q).await.unwrap();
    tokio::time::advance(steam::REFRESH).await;
    p.lookup(&k, &q).await.unwrap();
    let before = stored_sightings(&k).await.unwrap();
    assert_eq!(before.count, 1);
    tokio::time::advance(HOUR).await;
    assert_eq!(
        id_of(&p.lookup(&k, &q).await.unwrap()),
        Some("70000".into())
    );
    replayer.finish().unwrap();
    assert_eq!(replayer.requests().len(), 3);
    assert_eq!(stored_sightings(&k).await, Some(before));
}
