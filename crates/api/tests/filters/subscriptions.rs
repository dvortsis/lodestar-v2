//! List subscriptions through the filters API (§20.13, owner's decision
//! §2.9): nothing is subscribed by default, connecting needs no password
//! and shows its strictness, and removing, pausing or accepting a version
//! that drops entries needs the password and releases exactly what its
//! warning counted.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::ConnectInfo;
use lodestar_filter::lists::{IpRange, IpSet, KIND_IP};
use proptest::prelude::*;

use super::*;

/// Password confirms are rate-limited per address (5, the login
/// included), so tests that confirm often send from addresses of their
/// own.
async fn apply_from(
    f: &Fixture,
    cookie: &str,
    from: u8,
    change: Value,
    password: Option<&str>,
) -> (StatusCode, Value) {
    send(&f.app, cookie, from, change, password).await
}

async fn send(
    app: &Router,
    cookie: &str,
    from: u8,
    change: Value,
    password: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = post(
        "/api/v1/filters/apply",
        Some(cookie),
        json!({ "change": change, "password": password }),
    );
    let addr = SocketAddr::from((Ipv4Addr::new(192, 0, 2, from), 1));
    req.extensions_mut().insert(ConnectInfo(addr));
    let (status, _, body) = call(app, req).await;
    (status, body)
}

async fn preview(f: &Fixture, cookie: &str, change: Value) -> (StatusCode, Value) {
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/preview",
            Some(cookie),
            json!({ "change": change }),
        ),
    )
    .await;
    (status, body)
}

async fn listing(f: &Fixture, cookie: &str) -> Value {
    let (status, _, body) = call(&f.app, get("/api/v1/filters/subscriptions", cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn add_list(f: &Fixture, url: &str, ip: bool, floor: bool) -> i64 {
    f.api
        .store
        .lock()
        .unwrap()
        .add_subscription(&lodestar_store::NewSubscription {
            url,
            name: url,
            kind: if ip { KIND_IP } else { 0 },
            format: Some(if ip { "cidr" } else { "hashes" }),
            action: Action::Drop.code(),
            floor,
            interval_secs: 86_400,
        })
        .unwrap()
}

/// Records a fetched version (`held`: one the sanity gates held back) and
/// applies the lists, as the refresh task does.
fn version(f: &Fixture, id: i64, data: &[u8], held: bool) {
    let mut s = f.api.store.lock().unwrap();
    let entries = (data.len() / 20) as i64;
    let r = if held {
        lodestar_store::Refresh::Held {
            data,
            entries,
            etag: None,
            last_modified: None,
            why: "gated",
        }
    } else {
        lodestar_store::Refresh::Updated {
            data,
            entries,
            etag: None,
            last_modified: None,
        }
    };
    s.record_refresh(id, 1, &r).unwrap();
    lodestar_filter::lists::apply(&s, &f.api.engine, &f.api.ips).unwrap();
}

fn ips(ranges: &[(u32, u32)]) -> Vec<u8> {
    IpSet::new(ranges.iter().map(|&(a, b)| IpRange::V4(a, b))).encode()
}

fn blocked(f: &Fixture, h: [u8; 20]) -> bool {
    f.api.engine.current().blocked_hash(&[h]).is_some()
}

fn held(f: &Fixture, h: [u8; 20]) -> bool {
    f.api.store.lock().unwrap().held_among(&[h]).unwrap()[0]
}

fn code(body: &Value) -> &Value {
    &body["error"]["code"]
}

#[tokio::test]
async fn nothing_is_subscribed_by_default_and_listing_connects_nothing() {
    let f = fixture();
    let c = logged_in(&f).await;
    let l = listing(&f, &c).await;
    let catalog = l["catalog"].as_array().unwrap();
    assert_eq!(
        catalog.len(),
        lodestar_filter::strictness::CATALOG.len(),
        "{l}"
    );
    assert!(catalog.iter().all(|e| e["subscription"].is_null()));
    assert_eq!(l["connected"], json!([]));
    assert_eq!(l["strictness"]["level"], "none");
    let bt = catalog
        .iter()
        .find(|e| e["key"] == "bt-blocklists")
        .unwrap();
    assert_eq!(bt["kind"], "ip");
    assert_eq!(bt["figures"]["measured"], true);
    assert!(bt["figures"]["ipv4_public_share"].as_f64().unwrap() > 0.25);
    // Previewing a connect stores nothing either.
    let change =
        json!({ "op": "subscribe", "url": bt["url"], "kind": "ip", "format": bt["format"] });
    assert_eq!(preview(&f, &c, change).await.0, StatusCode::OK);
    assert_eq!(listing(&f, &c).await["connected"], json!([]));
    assert!(
        f.api
            .store
            .lock()
            .unwrap()
            .subscriptions()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn connecting_needs_no_password_and_shows_its_strictness() {
    let f = fixture();
    let c = logged_in(&f).await;
    let bt = lodestar_filter::strictness::catalog("bt-blocklists").unwrap();
    let change = json!({ "op": "subscribe", "url": bt.url, "kind": "ip", "format": "p2p" });
    let (status, a) = preview(&f, &c, change.clone()).await;
    assert_eq!(status, StatusCode::OK, "{a}");
    assert_eq!(
        (&a["weakening"], &a["confirm"]),
        (&json!(false), &json!(false))
    );
    assert!(a["warning"].is_null());
    assert_eq!(a["strictness"]["level"], "severe", "{a}");
    assert_eq!(a["strictness"]["estimate"], true);
    let (status, body) = apply(&f, &c, change.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["id"].as_i64().unwrap();
    assert_eq!(body["assessment"]["strictness"]["level"], "severe");
    // Connecting again is the same list.
    assert_eq!(apply(&f, &c, change, None).await.1["id"], id);
    let l = listing(&f, &c).await;
    let entry = l["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "bt-blocklists")
        .unwrap();
    assert_eq!(entry["subscription"], id);
    assert_eq!(l["connected"][0]["subscription"]["enabled"], true);
    assert_eq!(l["connected"][0]["held"], false);
    assert_eq!(l["strictness"]["level"], "severe");
}

#[tokio::test]
async fn a_subscribe_is_validated_in_the_preview_and_in_apply() {
    let f = fixture();
    let c = logged_in(&f).await;
    let base = json!({ "op": "subscribe", "url": "https://lists.example/a", "kind": "content" });
    let long = format!("https://lists.example/{}", "a".repeat(2100));
    let cases = [
        (json!({ "kind": "peers" }), "unknown_list_kind", "kind"),
        (
            json!({ "url": "ftp://lists.example/a" }),
            "bad_list_url",
            "url",
        ),
        (
            json!({ "url": "https://lists.example/a b" }),
            "bad_list_url",
            "url",
        ),
        (
            json!({ "url": "https://lists.example/\na" }),
            "bad_list_url",
            "url",
        ),
        (json!({ "url": long }), "bad_list_url", "url"),
        (
            json!({ "url": "https://user:pw@lists.example/a" }),
            "bad_list_url",
            "url",
        ),
        (json!({ "url": "https://" }), "bad_list_url", "url"),
        (
            json!({ "url": "http://169.254.169.254/latest" }),
            "list_url_refused",
            "url",
        ),
        (
            json!({ "url": "http://10.0.0.1/a" }),
            "list_url_refused",
            "url",
        ),
        (
            json!({ "url": "http://127.0.0.1/a" }),
            "list_url_refused",
            "url",
        ),
        (
            json!({ "url": "http://[::1]/a" }),
            "list_url_refused",
            "url",
        ),
        (
            json!({ "url": "http://0x7f.1/a" }),
            "list_url_refused",
            "url",
        ),
        (json!({ "name": "é".repeat(201) }), "bad_list_name", "name"),
        (
            json!({ "name": "safe\u{202e}txt.exe" }),
            "bad_list_name",
            "name",
        ),
        (
            json!({ "name": "zero\u{200b}width" }),
            "bad_list_name",
            "name",
        ),
        (json!({ "name": " " }), "bad_list_name", "name"),
        (json!({ "action": "hide" }), "bad_list_action", "action"),
        (json!({ "format": "zip" }), "unknown_list_format", "format"),
        (json!({ "format": "p2p" }), "list_format_mismatch", "format"),
        (
            json!({ "kind": "ip", "floor": true }),
            "floor_needs_content_list",
            "floor",
        ),
    ];
    for (patch, want, field) in cases {
        let mut change = base.clone();
        for (k, v) in patch.as_object().unwrap() {
            change[k] = v.clone();
        }
        for (status, body) in [
            preview(&f, &c, change.clone()).await,
            apply(&f, &c, change.clone(), None).await,
        ] {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{change}: {body}");
            assert_eq!(code(&body), want, "{change}");
            assert_eq!(body["error"]["field"], field, "{change}");
        }
    }
    assert!(
        f.api
            .store
            .lock()
            .unwrap()
            .subscriptions()
            .unwrap()
            .is_empty()
    );
    // Marking a list as part of the floor locks its entries: password.
    let floor = json!({ "op": "subscribe", "url": "https://lists.example/cs", "kind": "content",
        "floor": true });
    let (status, body) = apply_from(&f, &c, 1, floor.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
    let (status, _) = apply_from(&f, &c, 1, floor.clone(), Some("wrong password")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        f.api
            .store
            .lock()
            .unwrap()
            .subscriptions()
            .unwrap()
            .is_empty()
    );
    let (status, body) = apply_from(&f, &c, 1, floor, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        listing(&f, &c).await["connected"][0]["subscription"]["floor"],
        true
    );
    // A name counts characters, not bytes; without one, the host names it.
    let named = json!({ "op": "subscribe", "url": "https://lists.example/n", "kind": "content",
        "name": "é".repeat(200) });
    assert_eq!(apply(&f, &c, named, None).await.0, StatusCode::OK);
    let (_, body) = apply(&f, &c, base, None).await;
    let l = listing(&f, &c).await;
    let entry = l["connected"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["subscription"]["id"] == body["id"])
        .unwrap();
    assert_eq!(entry["subscription"]["name"], "lists.example");
    // The URL is stored as parsed: one that fetches as it was checked.
    let spaced =
        json!({ "op": "subscribe", "url": "\u{a0}HTTPS://Lists.Example/nb", "kind": "content" });
    let (status, body) = apply(&f, &c, spaced, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = f
        .api
        .store
        .lock()
        .unwrap()
        .subscription(body["id"].as_i64().unwrap())
        .unwrap();
    assert_eq!(stored.unwrap().url, "https://lists.example/nb");
}

#[tokio::test]
async fn pausing_and_removing_need_the_password_and_release_what_they_counted() {
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    let [h0, h1, h2, other] = [[0x10u8; 20], [0x11; 20], [0x12; 20], [0x13; 20]];
    let id = add_list(&f, "https://lists.example/content", false, false);
    version(&f, id, &[h0, h1, h2, other].concat(), false);
    let rule = format!("L-{id}");
    hold(&f, h0, &rule, Held::Quarantine, now);
    hold(&f, h1, &rule, Held::Tombstone, now);
    hold(&f, other, "CS-3a", Held::Tombstone, now);
    assert!(blocked(&f, h0) && f.api.held.contains(&h0));

    // An unknown list is a 404 before any password is asked for.
    for op in [
        json!({ "op": "unsubscribe", "id": 999 }),
        json!({ "op": "pause_subscription", "id": 999, "enabled": false }),
        json!({ "op": "pause_subscription", "id": 999, "enabled": true }),
        json!({ "op": "resolve_held", "id": 999, "accept": true }),
    ] {
        let (status, body) = preview(&f, &c, op.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{op}: {body}");
        let (status, body) = apply(&f, &c, op.clone(), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{op}: {body}");
        assert_eq!(code(&body), "subscription_not_found");
    }

    let pause = json!({ "op": "pause_subscription", "id": id, "enabled": false });
    let (_, a) = preview(&f, &c, pause.clone()).await;
    assert_eq!(
        (&a["weakening"], &a["confirm"]),
        (&json!(true), &json!(true))
    );
    assert_eq!(a["preview"]["quarantine"], 1);
    assert_eq!(a["preview"]["tombstones"], 1);
    let w = a["warning"].as_str().unwrap();
    assert!(
        w.starts_with("Pausing") && w.contains("fetched and indexed again"),
        "{w}"
    );
    assert_eq!(a["strictness"]["level"], "none", "nothing left");
    let (status, body) = apply_from(&f, &c, 2, pause.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
    assert_eq!(code(&body), "confirm_required");
    assert_eq!(body["assessment"]["warning"], a["warning"]);
    let (status, _) = apply_from(&f, &c, 2, pause.clone(), Some("wrong password")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        blocked(&f, h0) && held(&f, h0) && held(&f, h1),
        "nothing changed"
    );
    let (status, body) = apply_from(&f, &c, 2, pause, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["released"], 2, "what the preview counted");
    assert!(!blocked(&f, h0) && !held(&f, h0) && !held(&f, h1));
    assert!(!f.api.held.contains(&h0) && !f.api.held.contains(&h1));
    assert!(held(&f, other), "another rule's hold stays");
    assert_eq!(
        listing(&f, &c).await["connected"][0]["subscription"]["enabled"],
        false
    );

    // Resuming strengthens: no password.
    let resume = json!({ "op": "pause_subscription", "id": id, "enabled": true });
    let (_, a) = preview(&f, &c, resume.clone()).await;
    assert_eq!(a["confirm"], false, "{a}");
    assert_eq!(a["strictness"]["hashes"], 4);
    assert_eq!(apply(&f, &c, resume, None).await.0, StatusCode::OK);
    assert!(blocked(&f, h2));

    // Removing it: the list stops applying before anything is released,
    // so a released hash isn't held again by it.
    hold(&f, h2, &rule, Held::Quarantine, now);
    let remove = json!({ "op": "unsubscribe", "id": id });
    let (_, a) = preview(&f, &c, remove.clone()).await;
    assert_eq!(a["preview"]["quarantine"], 1, "{a}");
    assert!(a["warning"].as_str().unwrap().starts_with("Removing"));
    let (status, body) = apply_from(&f, &c, 3, remove.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
    let (status, _) = apply_from(&f, &c, 3, remove.clone(), Some("wrong password")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        blocked(&f, h2) && held(&f, h2),
        "a wrong password changes nothing"
    );
    let (status, body) = apply_from(&f, &c, 3, remove.clone(), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["released"], 1);
    assert!(!blocked(&f, h2), "the removed list's layer is gone");
    assert!(!held(&f, h2) && !f.api.held.contains(&h2));
    assert_eq!(listing(&f, &c).await["connected"], json!([]));
    let (status, body) = apply_from(&f, &c, 3, remove, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn accepting_a_version_that_drops_entries_needs_the_password() {
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    let h: Vec<[u8; 20]> = (0..5u8).map(|i| [0x20 + i; 20]).collect();
    let id = add_list(&f, "https://lists.example/grows", false, false);
    version(&f, id, &[h[0], h[1]].concat(), false);
    // Ten times bigger, but without h0: held, and accepting it unblocks h0.
    version(&f, id, &[h[1], h[2], h[3]].concat(), true);
    let rule = format!("L-{id}");
    hold(&f, h[0], &rule, Held::Quarantine, now);
    hold(&f, h[1], &rule, Held::Tombstone, now);
    assert_eq!(listing(&f, &c).await["connected"][0]["held"], true);

    let keep = json!({ "op": "resolve_held", "id": id, "accept": false });
    assert_eq!(preview(&f, &c, keep).await.1["confirm"], false);
    let accept = json!({ "op": "resolve_held", "id": id, "accept": true });
    let (_, a) = preview(&f, &c, accept.clone()).await;
    assert_eq!(a["weakening"], true, "{a}");
    assert_eq!(
        (&a["preview"]["quarantine"], &a["preview"]["tombstones"]),
        (&json!(1), &json!(0))
    );
    assert!(a["warning"].as_str().unwrap().contains("held back"));
    assert_eq!(a["strictness"]["hashes"], 3);
    assert_eq!(a["unblocked"], 1, "{a}");
    let w = a["warning"].as_str().unwrap();
    assert!(
        w.contains("1 entry it no longer has stops being blocked"),
        "{w}"
    );
    assert!(
        w.contains("1 items in quarantine") && !w.contains("30 days"),
        "{w}"
    );
    let (status, body) = apply_from(&f, &c, 4, accept.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
    // The password confirms the version the warning described, so it's
    // named, before the password is read.
    let (status, body) = apply_from(&f, &c, 4, accept.clone(), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(code(&body), "held_needs_version");
    let mut named = accept.clone();
    named["held"] = a["held_version"].clone();
    let (status, body) = apply_from(&f, &c, 4, named, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["released"], 1, "only the dropped entry's hold");
    assert!(!held(&f, h[0]) && !blocked(&f, h[0]));
    assert!(held(&f, h[1]), "still listed, still held");
    assert!(blocked(&f, h[3]));
    let (status, body) = apply(&f, &c, accept.clone(), None).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, &json!("nothing_held"))
    );

    // A held version that only adds needs nothing.
    version(&f, id, &[h[1], h[2], h[3], h[4]].concat(), true);
    let (status, body) = apply(&f, &c, accept, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(blocked(&f, h[4]));
}

/// The password confirms the held version the warning described: one a
/// refresh held after the preview is refused, and nothing is released.
#[tokio::test]
async fn a_version_held_after_the_preview_is_refused() {
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    let h: Vec<[u8; 20]> = (0..6u8).map(|i| [0x50 + i; 20]).collect();
    let id = add_list(&f, "https://lists.example/swap", false, false);
    version(&f, id, &[h[0], h[1], h[2]].concat(), false);
    version(&f, id, &[h[0], h[1], h[3]].concat(), true);
    for x in &h[..3] {
        hold(&f, *x, &format!("L-{id}"), Held::Quarantine, now);
    }
    let accept = json!({ "op": "resolve_held", "id": id, "accept": true });
    let (_, a) = preview(&f, &c, accept.clone()).await;
    assert_eq!(a["unblocked"], 1, "{a}");
    // While the dialog is open, the refresh holds a version that drops
    // more, with other holds.
    version(&f, id, &h[3], true);
    let mut named = accept.clone();
    named["held"] = a["held_version"].clone();
    let (status, body) = apply_from(&f, &c, 7, named, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "changed_since_preview");
    assert!(h[..3].iter().all(|x| held(&f, *x) && blocked(&f, *x)));
    // Previewed again, it names the new version.
    let (_, b) = preview(&f, &c, accept.clone()).await;
    assert_eq!(b["unblocked"], 3);
    assert_ne!(b["held_version"], a["held_version"]);

    // The same held bytes against another applied version unblock
    // something else, so the fingerprint changes too: a refresh applies B
    // (which clears the held one), then the publisher's version is held
    // again against B.
    let x = [h[0], h[1], h[3]].concat();
    version(&f, id, &[h[0], h[1], h[2]].concat(), false);
    version(&f, id, &x, true);
    let (_, a) = preview(&f, &c, accept.clone()).await;
    assert_eq!(a["unblocked"], 1, "{a}");
    version(&f, id, &[h[0], h[1], h[2], h[4], h[5]].concat(), false);
    version(&f, id, &x, true);
    let mut named = accept.clone();
    named["held"] = a["held_version"].clone();
    let (status, body) = apply_from(&f, &c, 8, named, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "changed_since_preview");
    assert!(blocked(&f, h[4]) && blocked(&f, h[5]), "nothing applied");
    assert_eq!(preview(&f, &c, accept).await.1["unblocked"], 3);
}

#[tokio::test]
async fn an_ip_list_that_narrows_needs_the_password() {
    let f = fixture();
    let c = logged_in(&f).await;
    let base = u32::from(Ipv4Addr::new(198, 51, 100, 0));
    let id = add_list(&f, "https://lists.example/peers", true, false);
    version(&f, id, &ips(&[(base, base + 255)]), false);
    let inside = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 200));
    assert!(f.api.ips.contains(inside));
    let accept = json!({ "op": "resolve_held", "id": id, "accept": true });
    version(&f, id, &ips(&[(base, base + 127)]), true);
    let (status, body) = apply(&f, &c, accept.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
    assert!(
        body["assessment"]["warning"]
            .as_str()
            .unwrap()
            .contains("contact")
    );
    assert!(f.api.ips.contains(inside), "nothing changed");
    // A held version that doesn't decode would block nothing: weakening.
    version(&f, id, b"not an ip set", true);
    let (status, _) = apply(&f, &c, accept.clone(), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    // A wider version keeps everything blocked.
    version(&f, id, &ips(&[(base - 256, base + 511)]), true);
    let (status, body) = apply(&f, &c, accept.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(f.api.ips.contains(inside));
    // Narrowing with the password unblocks what it dropped, and only that.
    version(&f, id, &ips(&[(base, base + 127)]), true);
    let (_, a) = preview(&f, &c, accept.clone()).await;
    assert_eq!(a["unblocked"], 1, "{a}");
    let mut named = accept;
    named["held"] = a["held_version"].clone();
    let (status, body) = apply_from(&f, &c, 6, named, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!f.api.ips.contains(inside));
    assert!(
        f.api
            .ips
            .contains(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 100)))
    );
    let (_, a) = preview(&f, &c, json!({ "op": "unsubscribe", "id": id })).await;
    assert_eq!(a["confirm"], true);
    assert!(a["warning"].as_str().unwrap().contains("contact"));
}

#[tokio::test]
async fn a_floor_marked_list_warns_that_its_entries_leave_the_floor() {
    let f = fixture();
    let c = logged_in(&f).await;
    let (a, b, d) = ([0x31u8; 20], [0x32u8; 20], [0x33u8; 20]);
    let id = add_list(&f, "https://lists.example/vetted", false, true);
    version(&f, id, &[a, b, d].concat(), false);
    // The publisher drops `d`: it waits in the pending removals, blocked.
    version(&f, id, &[a, b].concat(), false);
    // `a` is your own CS-1 entry as well, so it stays in the floor.
    let hex = |h: &[u8; 20]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let report = json!({ "op": "report_abuse", "hashes": [hex(&a)] });
    assert_eq!(apply(&f, &c, report, None).await.0, StatusCode::OK);
    for op in [
        json!({ "op": "unsubscribe", "id": id }),
        json!({ "op": "pause_subscription", "id": id, "enabled": false }),
    ] {
        let (_, p) = preview(&f, &c, op.clone()).await;
        assert_eq!(p["preview"]["removed_from_floor"], 2, "{op}: {p}");
        assert!(p["warning"].as_str().unwrap().contains("locked floor"));
    }
    let remove = json!({ "op": "unsubscribe", "id": id });
    let (status, body) = apply_from(&f, &c, 5, remove, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(blocked(&f, a), "your own entry stays");
    assert!(!blocked(&f, b) && !blocked(&f, d));
    assert_eq!(overview(&f, &c).await["pending_floor_removals"], json!([]));
}

/// Several changes to one list at once: each applies whole, in some
/// order, and what's released is released once.
#[tokio::test]
async fn concurrent_changes_to_one_list_apply_once() {
    let f = fixture();
    let c = logged_in(&f).await;
    let now = lodestar_api_now();
    let h = [0x40u8; 20];
    let id = add_list(&f, "https://lists.example/race", false, false);
    version(&f, id, &h, false);
    hold(&f, h, &format!("L-{id}"), Held::Quarantine, now);
    let mut calls = tokio::task::JoinSet::new();
    for i in 0..8u8 {
        let change = if i % 2 == 0 {
            json!({ "op": "unsubscribe", "id": id })
        } else {
            json!({ "op": "pause_subscription", "id": id, "enabled": i % 4 == 1 })
        };
        let (app, c) = (f.app.clone(), c.clone());
        calls.spawn(async move {
            let r = send(&app, &c, 10 + i, change, Some(PASSWORD)).await;
            (usize::from(i), r)
        });
    }
    let mut results = calls.join_all().await;
    results.sort_by_key(|(i, _)| *i);
    let results: Vec<_> = results.into_iter().map(|(_, r)| r).collect();
    let mut removed = 0;
    let mut released = 0;
    for (i, (status, body)) in results.iter().enumerate() {
        assert!(
            *status == StatusCode::OK || *status == StatusCode::NOT_FOUND,
            "{i}: {status} {body}"
        );
        if *status == StatusCode::OK {
            removed += usize::from(i % 2 == 0);
            released += body["released"].as_u64().unwrap_or(0);
        }
    }
    assert_eq!(removed, 1, "{results:?}");
    assert_eq!(released, 1, "released once: {results:?}");
    assert!(!blocked(&f, h) && !held(&f, h) && !f.api.held.contains(&h));
    assert!(
        f.api
            .store
            .lock()
            .unwrap()
            .subscriptions()
            .unwrap()
            .is_empty()
    );
}

/// What filtering blocks for one property case: per hash, whether the
/// engine blocks it, the store holds it or intake refuses it, and per
/// address whether it's denied.
fn state(f: &Fixture, hashes: &[[u8; 20]], addrs: &[IpAddr]) -> Vec<bool> {
    let store = f.api.store.lock().unwrap();
    let mut out: Vec<bool> = store.held_among(hashes).unwrap();
    out.extend(hashes.iter().map(|h| blocked(f, *h)));
    out.extend(hashes.iter().map(|h| f.api.held.contains(h)));
    out.extend(addrs.iter().map(|a| f.api.ips.contains(*a)));
    out
}

#[derive(Debug, Clone)]
enum Op {
    Pause(usize, bool),
    Unsubscribe(usize),
    Resolve(usize, bool),
    Subscribe(usize, bool),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0..4usize, any::<bool>()).prop_map(|(i, on)| Op::Pause(i, on)),
        (0..4usize).prop_map(Op::Unsubscribe),
        (0..4usize, any::<bool>()).prop_map(|(i, a)| Op::Resolve(i, a)),
        (0..3usize, any::<bool>()).prop_map(|(i, floor)| Op::Subscribe(i, floor)),
    ]
}

/// No sequence of subscription changes sent without the password ever
/// unblocks anything: each either strengthens, changes nothing, or is
/// refused (428, 404, 409, 400).
#[test]
fn no_sequence_loosens_filtering_without_the_password() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let f = fixture();
    let c = rt.block_on(logged_in(&f));
    let case = std::cell::Cell::new(0u8);
    let config = ProptestConfig {
        cases: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    proptest!(config, |(ops in proptest::collection::vec(op(), 1..10))| {
        let n = case.get().wrapping_add(1);
        case.set(n);
        let hs: Vec<[u8; 20]> = (0..6u8).map(|i| { let mut h = [i; 20]; h[0] = n; h }).collect();
        let net = u32::from(Ipv4Addr::new(10, n, 0, 0));
        let addrs = [10u8, 200].map(|x| IpAddr::V4(Ipv4Addr::from(net + u32::from(x))));
        let url = |what: &str| format!("https://lists.example/{n}/{what}");
        // A content list whose held version drops h0, an IP list whose
        // held version narrows, a floor list whose held version drops h3,
        // and a paused list.
        let ids = [
            add_list(&f, &url("content"), false, false),
            add_list(&f, &url("peers"), true, false),
            add_list(&f, &url("floor"), false, true),
            add_list(&f, &url("paused"), false, false),
        ];
        version(&f, ids[0], &[hs[0], hs[1]].concat(), false);
        version(&f, ids[0], &[hs[1], hs[2]].concat(), true);
        version(&f, ids[1], &ips(&[(net, net + 255)]), false);
        version(&f, ids[1], &ips(&[(net, net + 15)]), true);
        version(&f, ids[2], &hs[3], false);
        version(&f, ids[2], &hs[4], true);
        version(&f, ids[3], &hs[5], false);
        f.api.store.lock().unwrap().set_subscription_enabled(ids[3], false).unwrap();
        {
            let s = f.api.store.lock().unwrap();
            lodestar_filter::lists::apply(&s, &f.api.engine, &f.api.ips).unwrap();
        }
        let now = lodestar_api_now();
        hold(&f, hs[0], &format!("L-{}", ids[0]), Held::Quarantine, now);
        hold(&f, hs[1], &format!("L-{}", ids[0]), Held::Tombstone, now);
        for op in ops {
            let change = match op {
                Op::Pause(i, on) => json!({ "op": "pause_subscription", "id": ids[i], "enabled": on }),
                Op::Unsubscribe(i) => json!({ "op": "unsubscribe", "id": ids[i] }),
                Op::Resolve(i, accept) => json!({ "op": "resolve_held", "id": ids[i], "accept": accept }),
                Op::Subscribe(i, floor) => json!({ "op": "subscribe", "url": url(&format!("new{i}")),
                    "kind": "content", "floor": floor }),
            };
            let before = state(&f, &hs, &addrs);
            let (status, body) = rt.block_on(apply(&f, &c, change.clone(), None));
            let after = state(&f, &hs, &addrs);
            prop_assert!(
                status == StatusCode::OK
                    || status == StatusCode::PRECONDITION_REQUIRED
                    || status == StatusCode::NOT_FOUND
                    || status == StatusCode::CONFLICT,
                "{change}: {status} {body}"
            );
            let loosened: Vec<usize> = before
                .iter()
                .zip(&after)
                .enumerate()
                .filter(|(_, (b, a))| **b && !**a)
                .map(|(i, _)| i)
                .collect();
            prop_assert!(loosened.is_empty(), "{change} ({status}) loosened {loosened:?}: {body}");
        }
    });
}
