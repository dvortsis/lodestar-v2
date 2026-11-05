//! RP-3's API gates (plan 2026-11-04 §4.3): `GET /filters` lists pack and
//! rule versions, changelogs and pending versions; accepting a pending
//! version is weakening (428 without the password) for every weakening
//! direction, no record and a shadow disagreement, and strengthening (no
//! password) for a widening or heavier version whose shadow agrees; a
//! stale preview gives `changed_since_preview`; re-confirming a paused
//! override is gated; the review queue marks what acceptance left for
//! review; and `GET /filters/fixtures` reports the pack corpora.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::filters::{Change, FiltersApi};
use lodestar_api::{ApiState, Auth, router};
use lodestar_filter::lists::SharedIps;
use lodestar_filter::packs::Ctx as PackCtx;
use lodestar_filter::packs::manifest::{Code, Direction, Manifest, RuleEntry, Version};
use lodestar_filter::packs::upgrade::{self, Applied, FRESH_KEY};
use lodestar_filter::{Action, Core, CsList, Engine, Handle, HeldSet, Lists, PackRule, Switches};
use lodestar_store::{SHADOW, Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    api: Arc<FiltersApi>,
    _dir: tempfile::TempDir,
}

/// The built-in manifest with every rule at its first version: MW-2 and
/// FK-2 cut back to the version 1 this build keeps (RP-1b ships their
/// version 2). These are RP-3's acceptance tests, which start from an
/// install that applied every rule at version 1; the shipped manifest's
/// own corpus report is checked with [`Manifest::builtin`].
fn baseline() -> Arc<Manifest> {
    let b = Manifest::builtin();
    let entries = PackRule::ALL
        .into_iter()
        .map(|r| {
            let e = b.entry(r);
            let first = e.versions()[0].clone();
            assert!(first.code.is_some(), "{} keeps its version 1", r.id());
            RuleEntry::new(r, e.baseline, vec![first])
        })
        .collect();
    Arc::new(Manifest::new(entries).unwrap())
}

fn real_mw2(c: &PackCtx<'_>) -> bool {
    c.matches(PackRule::Mw2)
}

/// The built-in manifest with MW-2's versions replaced by `versions`
/// (number, direction, action), each matching as MW-2 does today.
fn mw2(versions: &[(u32, Direction, Action)]) -> Arc<Manifest> {
    let b = baseline();
    let mut entries: Vec<RuleEntry> = PackRule::ALL
        .into_iter()
        .filter(|r| *r != PackRule::Mw2)
        .map(|r| b.entry(r).clone())
        .collect();
    let n = versions.len();
    entries.push(RuleEntry::new(
        PackRule::Mw2,
        true,
        versions
            .iter()
            .enumerate()
            .map(|(i, (version, direction, action))| Version {
                version: *version,
                direction: *direction,
                action: *action,
                code: Some(if i + 1 == n {
                    Code::Current
                } else {
                    Code::Fn(real_mw2)
                }),
            })
            .collect(),
    ));
    Arc::new(Manifest::new(entries).unwrap())
}

/// MW-2 at version 2 in `direction` with `action` (version 1 hides).
fn bumped(direction: Direction, action: Action) -> Arc<Manifest> {
    mw2(&[(1, Direction::Widens, Action::Hide), (2, direction, action)])
}

/// The built-in manifest with MW-5 as a rule added after RP-0, as MW-6 is:
/// an install that never applied it has it pending at version 1.
fn new_rule() -> Arc<Manifest> {
    let b = baseline();
    let entries = PackRule::ALL
        .into_iter()
        .map(|r| {
            let e = b.entry(r).clone();
            if r == PackRule::Mw5 {
                RuleEntry::new(r, false, e.versions().to_vec())
            } else {
                e
            }
        })
        .collect();
    Arc::new(Manifest::new(entries).unwrap())
}

/// A store an earlier build set up with every rule at version 1 (but the
/// rules in `missing`) and the malware pack on, served with `manifest`.
fn fixture_with(manifest: Arc<Manifest>, missing: &[PackRule]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    store.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    let mut record = upgrade::current_record(&baseline());
    for r in missing {
        record.0.remove(r.id());
    }
    let v1 = record.encode();
    store
        .write_pack_versions([Some(&v1), Some(&v1)], Some(("malware", true)))
        .unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let mut engine = Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    ));
    engine.manifest = manifest;
    let api = Arc::new(FiltersApi {
        store: Arc::new(Mutex::new(store)),
        engine: Arc::new(Handle::new(engine)),
        held: Arc::new(HeldSet::default()),
        sealer: None,
        ips: Arc::new(SharedIps::default()),
        index: None,
        reenrich: Arc::new(AtomicBool::new(false)),
        jobs: Default::default(),
        quarantine_secs: 30 * 86_400,
        shutdown: Default::default(),
        adult_sources: 0,
    });
    // The engine as the store has it: MW-2's version 1 applied, version 2
    // in shadow.
    let set = api.store.lock().unwrap().rule_set().unwrap();
    api.engine.update_engine(|e| e.load_rules(&set));
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: Some(api.clone()),
            hot: None,
            events: None,
            settings: None,
            ops: None,
            user_data: None,
            adult: Default::default(),
            insights: None,
        }),
    );
    Fixture {
        app,
        auth,
        api,
        _dir: dir,
    }
}

fn fixture(manifest: Arc<Manifest>) -> Fixture {
    fixture_with(manifest, &[])
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Option<String>, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    (
        status,
        cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn get(f: &Fixture, c: &str, uri: &str) -> Value {
    let (status, _, body) = call(
        &f.app,
        Request::get(uri)
            .header(header::COOKIE, c)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn logged_in(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let (status, cookie, _) = call(
        &f.app,
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD, "notice_version": lodestar_api::legal::NOTICE_VERSION }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn preview(f: &Fixture, c: &str, change: &Value) -> Value {
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/preview",
            Some(c),
            json!({ "change": change }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn apply(
    f: &Fixture,
    c: &str,
    change: &Value,
    password: Option<&str>,
) -> (StatusCode, Value) {
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/apply",
            Some(c),
            json!({ "change": change, "password": password }),
        ),
    )
    .await;
    (status, body)
}

fn accept(rules: &[&str], versions: Option<&[&str]>) -> Value {
    json!({ "op": "accept_pack_versions", "rules": rules, "versions": versions })
}

fn rule_state<'a>(overview: &'a Value, rule: &str) -> &'a Value {
    overview["packs"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p["rules"].as_array().unwrap())
        .find(|r| r["id"] == rule)
        .unwrap()
}

fn applied(f: &Fixture) -> Applied {
    let store = f.api.store.lock().unwrap();
    let rows: Vec<_> = upgrade::APPLIED_KEYS
        .iter()
        .map(|k| store.setting_text(k).unwrap().unwrap())
        .collect();
    assert_eq!(rows[0], rows[1]);
    Applied::decode(&rows[0]).unwrap()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A filter hit of `rule` on hash `[n; 20]`, `ago` seconds ago; a shadow
/// one when `shadow`.
fn hit(f: &Fixture, rule: &str, n: u8, action: Action, shadow: bool, ago: i64) {
    let code = action.code() | if shadow { SHADOW } else { 0 };
    f.api
        .store
        .lock()
        .unwrap()
        .connection()
        .execute(
            "INSERT INTO filter_hits (hash, rule, action, at, evidence) VALUES (?1, ?2, ?3, ?4, x'')",
            lodestar_store::rusqlite::params![vec![n; 20], rule, code, now() - ago],
        )
        .unwrap();
}

fn shadow_hits(f: &Fixture, rule: &str) -> i64 {
    f.api
        .store
        .lock()
        .unwrap()
        .connection()
        .query_row(
            "SELECT count(*) FROM filter_hits WHERE rule = ?1 AND (action & 128) != 0",
            [rule],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn get_filters_lists_versions_changelogs_and_pending_versions() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    let o = get(&f, &c, "/api/v1/filters").await;
    let malware = &o["packs"][0];
    assert_eq!(malware["id"], json!("malware"));
    assert_eq!(
        malware["version"],
        json!("MW-4@1 MW-1@1 MW-2@2 MW-3@1 MW-5@1 MW-6@1")
    );
    assert!(malware["changelog"].as_str().unwrap().contains("## MW-2@1"));
    let mw2 = rule_state(&o, "MW-2");
    assert_eq!(mw2["version"], json!(2));
    assert_eq!(mw2["applied"], json!(1));
    assert_eq!(mw2["pending"], json!(2));
    assert_eq!(mw2["action"], json!("hide"));
    assert_eq!(mw2["base_action"], json!("hide"));
    assert_eq!(mw2["base_changed"], json!(false));
    assert!(mw2.get("override").is_none() && mw2.get("pack_override").is_none());
    let mw1 = rule_state(&o, "MW-1");
    assert_eq!(mw1["pending"], json!(null));
    assert_eq!(mw1["applied"], json!(1));
    assert_eq!(
        o["pending_pack_versions"],
        json!([{
            "applied": 1, "direction": "widens", "pack": "malware", "rule": "MW-2",
            "version": 2,
            // The test's MW-2@2 widens; its text is the shipped MW-2@2's.
            "changelog": [{
                "direction": "widens",
                "text": lodestar_filter::packs::manifest::changelog_entry(PackRule::Mw2, 2),
                "version": 2,
            }],
        }])
    );
    assert_eq!(o["pack_conditions"], json!([]));
    assert_eq!(o["packs_seeded_over_rules"], json!(null));
}

#[tokio::test]
async fn a_widening_acceptance_whose_shadow_agrees_needs_no_password() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    // 3 was held by version 1 before the shadow ran, so it says nothing
    // once the start records where the shadow begins.
    hit(&f, "MW-2", 3, Action::Hide, false, 20 * 86_400);
    f.api.resume_requeues(now()).unwrap();
    // Only the new version holds 1; both hold 2 the same way.
    hit(&f, "MW-2", 1, Action::Hide, true, 100);
    hit(&f, "MW-2", 2, Action::Hide, true, 90);
    hit(&f, "MW-2", 2, Action::Hide, false, 90);
    let change = accept(&["MW-2"], None);
    let p = preview(&f, &c, &change).await;
    assert_eq!(p["weakening"], json!(false), "{p}");
    assert_eq!(p["confirm"], json!(false));
    assert_eq!(p["warning"], json!(null));
    assert_eq!(p["pack_versions"], json!(["MW-2@2"]));
    assert_eq!(
        p["pack_acceptance"],
        json!([{
            "applied": 1, "direction": "widens", "disagreements": 0, "heavier": 0,
            "in_doubt": false, "lighter": 0, "newly_held": 1, "no_longer_held": 0,
            "review_quarantine": 0, "review_tombstones": 0, "rule": "MW-2",
            "strengthens": true, "version": 2,
        }])
    );
    assert_eq!(p["preview"]["quarantine"], json!(0));
    assert_eq!(p["preview"]["caught_30d"], json!(2));
    let (status, body) = apply(&f, &c, &change, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(applied(&f).get(PackRule::Mw2), Some(2));
    assert!(
        !f.api
            .engine
            .current()
            .pack_versions
            .state(PackRule::Mw2)
            .pending()
    );
    assert_eq!(shadow_hits(&f, "MW-2"), 2, "the shadow hits are kept");
    let since = f
        .api
        .store
        .lock()
        .unwrap()
        .setting_text("filter.packs.shadow_since")
        .unwrap();
    assert_eq!(since, None, "nothing is pending, so nothing is recorded");
    let o = get(&f, &c, "/api/v1/filters").await;
    assert_eq!(o["pending_pack_versions"], json!([]));
    assert_eq!(rule_state(&o, "MW-2")["applied"], json!(2));
    // Nothing left to accept.
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/filters/preview",
            Some(&c),
            json!({ "change": change }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("not_pending"));
}

/// A rule added after RP-0 (MW-6's case): pending on every upgraded store
/// whose pack is on, and accepting it widens.
#[tokio::test]
async fn a_new_rule_in_a_pack_that_is_on_is_accepted_without_a_password() {
    let f = fixture_with(new_rule(), &[PackRule::Mw5]);
    let c = logged_in(&f).await;
    let o = get(&f, &c, "/api/v1/filters").await;
    let pending = &o["pending_pack_versions"];
    assert_eq!(pending[0]["rule"], json!("MW-5"));
    assert_eq!(pending[0]["applied"], json!(null));
    assert_eq!(pending[0]["direction"], json!("widens"));
    let entry = &pending[0]["changelog"][0];
    assert_eq!(entry["version"], json!(1));
    assert!(
        entry["text"].as_str().is_some_and(|t| !t.is_empty()),
        "{entry}"
    );
    hit(&f, "MW-5", 7, Action::Drop, true, 50);
    // By pack name: its pending rules.
    let change = accept(&["malware"], None);
    let p = preview(&f, &c, &change).await;
    assert_eq!(p["weakening"], json!(false), "{p}");
    assert_eq!(p["pack_versions"], json!(["MW-5@1"]));
    let (status, body) = apply(&f, &c, &change, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(applied(&f).get(PackRule::Mw5), Some(1));
    assert!(
        !f.api
            .engine
            .current()
            .pack_versions
            .state(PackRule::Mw5)
            .pending()
    );
}

/// Confirms a weakening acceptance: 428 without the password, 400 without
/// the versions, then accepted.
async fn needs_password(f: &Fixture, c: &str, why: &str) {
    let change = accept(&["MW-2"], None);
    let p = preview(f, c, &change).await;
    assert_eq!(p["weakening"], json!(true), "{why}: {p}");
    assert_eq!(p["confirm"], json!(true), "{why}");
    assert!(p["warning"].as_str().unwrap().contains("MW-2@"), "{why}");
    let (status, body) = apply(f, c, &change, None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{why}: {body}");
    assert_eq!(body["error"]["code"], json!("confirm_required"));
    let before = applied(f);
    let (status, body) = apply(f, c, &change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
    assert_eq!(body["error"]["code"], json!("pack_versions_needed"));
    assert_eq!(applied(f), before, "{why}: nothing accepted");
    let versions: Vec<String> = serde_json::from_value(p["pack_versions"].clone()).unwrap();
    let versions: Vec<&str> = versions.iter().map(String::as_str).collect();
    let (status, body) = apply(f, c, &accept(&["MW-2"], Some(&versions)), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{why}: {body}");
    assert_ne!(applied(f), before, "{why}");
}

#[tokio::test]
async fn every_weakening_acceptance_needs_the_password() {
    for (direction, action) in [
        (Direction::Narrows, Action::Hide),
        (Direction::ActionLighter, Action::Demote),
        (Direction::Mixed, Action::Drop),
    ] {
        let f = fixture(bumped(direction, action));
        let c = logged_in(&f).await;
        needs_password(&f, &c, direction.as_str()).await;
        assert_eq!(applied(&f).get(PackRule::Mw2), Some(2));
    }
    // No record: the version applied (2) is gone from the build's list.
    let f = fixture(mw2(&[
        (1, Direction::Widens, Action::Hide),
        (3, Direction::Widens, Action::Hide),
    ]));
    let c = logged_in(&f).await;
    {
        let mut store = f.api.store.lock().unwrap();
        let mut rec = upgrade::current_record(&baseline());
        rec.0.insert("MW-2".into(), 2);
        let t = rec.encode();
        store
            .write_pack_versions([Some(&t), Some(&t)], None)
            .unwrap();
    }
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["pack_acceptance"][0]["direction"], json!(null), "{p}");
    needs_password(&f, &c, "no record").await;
    assert_eq!(applied(&f).get(PackRule::Mw2), Some(3));
}

#[tokio::test]
async fn a_shadow_disagreement_makes_any_acceptance_weakening() {
    // Widens, but version 1 held a torrent the new version doesn't, while
    // both ran.
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    hit(&f, "MW-2", 1, Action::Hide, true, 100);
    hit(&f, "MW-2", 2, Action::Hide, false, 50);
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["pack_acceptance"][0]["disagreements"], json!(1), "{p}");
    assert_eq!(p["pack_acceptance"][0]["no_longer_held"], json!(1));
    needs_password(&f, &c, "widens, applied-only").await;

    // Action-heavier, but both versions hold one torrent the same way.
    let f = fixture(bumped(Direction::ActionHeavier, Action::Drop));
    let c = logged_in(&f).await;
    hit(&f, "MW-2", 1, Action::Hide, true, 100);
    hit(&f, "MW-2", 1, Action::Hide, false, 100);
    needs_password(&f, &c, "heavier, not heavier").await;

    // Action-heavier that agrees: no password.
    let f = fixture(bumped(Direction::ActionHeavier, Action::Drop));
    let c = logged_in(&f).await;
    hit(&f, "MW-2", 1, Action::Drop, true, 100);
    hit(&f, "MW-2", 1, Action::Hide, false, 100);
    let change = accept(&["MW-2"], None);
    assert_eq!(preview(&f, &c, &change).await["weakening"], json!(false));
    let (status, body) = apply(&f, &c, &change, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_version_state_in_doubt_makes_acceptance_weakening() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    // The rows differ: MW-1 recorded at 1 by one, missing from the other.
    {
        let mut store = f.api.store.lock().unwrap();
        let a = upgrade::current_record(&baseline());
        let mut b = a.clone();
        b.0.insert("XX-1".into(), 1);
        let (a, b) = (a.encode(), b.encode());
        store
            .write_pack_versions([Some(&a), Some(&b)], None)
            .unwrap();
    }
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["pack_acceptance"][0]["in_doubt"], json!(true), "{p}");
    let o = get(&f, &c, "/api/v1/filters").await;
    assert_eq!(o["pack_conditions"], json!(["packs.differ"]));
    let change = accept(&["MW-2"], Some(&["MW-2@2"]));
    let (status, _) = apply(&f, &c, &change, None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let (status, body) = apply(&f, &c, &change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_stale_preview_gives_changed_since_preview() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let _c = logged_in(&f).await;
    hit(&f, "MW-2", 1, Action::Hide, true, 100);
    let change: Change = serde_json::from_value(accept(&["MW-2"], None)).unwrap();
    let previewed = f.api.assess(&change, now()).unwrap();
    assert!(!previewed.confirm);
    // A torrent only version 1 holds arrives before the apply.
    hit(&f, "MW-2", 2, Action::Hide, false, 10);
    let err = f.api.apply(&change, previewed, now()).unwrap_err();
    assert_eq!(err.code(), "changed_since_preview");
    assert_eq!(applied(&f).get(PackRule::Mw2), Some(1), "nothing accepted");
    // Versions named that aren't the ones pending now.
    let change: Change = serde_json::from_value(accept(&["MW-2"], Some(&["MW-2@1"]))).unwrap();
    let a = f.api.assess(&change, now()).unwrap();
    assert_eq!(
        f.api.apply(&change, a, now()).unwrap_err().code(),
        "changed_since_preview"
    );
    assert_eq!(applied(&f).get(PackRule::Mw2), Some(1));
}

#[tokio::test]
async fn bad_acceptances_are_refused() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    for (change, status, code) in [
        (accept(&[], None), StatusCode::BAD_REQUEST, "no_rules"),
        (
            accept(&["MW-9"], None),
            StatusCode::BAD_REQUEST,
            "unknown_pack",
        ),
        (accept(&["MW-1"], None), StatusCode::CONFLICT, "not_pending"),
        (accept(&["spam"], None), StatusCode::CONFLICT, "not_pending"),
    ] {
        let (s, body) = apply(&f, &c, &change, None).await;
        assert_eq!(s, status, "{change}: {body}");
        assert_eq!(body["error"]["code"], json!(code), "{change}");
    }
    assert_eq!(applied(&f).get(PackRule::Mw2), Some(1));
}

#[tokio::test]
async fn re_confirming_a_paused_override_needs_the_password() {
    let f = fixture(bumped(Direction::ActionHeavier, Action::Drop));
    let c = logged_in(&f).await;
    let confirm =
        |base: Option<u32>| json!({ "op": "confirm_pack_override", "rule": "MW-2", "base": base });
    // Hide on a hide rule: no password. Confirming an unchanged one: 409.
    let set = json!({ "op": "set_pack_override", "rule": "MW-2", "action": "hide" });
    let (status, body) = apply(&f, &c, &set, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = apply(&f, &c, &confirm(None), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("pack_override_current"));
    // Version 2 drops: accepted (heavier, no shadow against it), and the
    // hide override now pauses.
    let (status, body) = apply(&f, &c, &accept(&["MW-2"], None), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let o = get(&f, &c, "/api/v1/filters").await;
    let mw2 = rule_state(&o, "MW-2");
    assert_eq!(mw2["base_changed"], json!(true));
    assert_eq!(mw2["pack_override"]["paused"], json!(true));
    assert_eq!(mw2["action"], json!("drop"), "paused: the rule as built");
    let p = preview(&f, &c, &confirm(None)).await;
    assert_eq!(p["weakening"], json!(true), "{p}");
    assert_eq!(p["pack_base"], json!(2));
    let (status, _) = apply(&f, &c, &confirm(None), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let (status, body) = apply(&f, &c, &confirm(None), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("pack_override_needs_base"));
    let (status, body) = apply(&f, &c, &confirm(Some(1)), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("changed_since_preview"));
    let (status, body) = apply(&f, &c, &confirm(Some(2)), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let o = get(&f, &c, "/api/v1/filters").await;
    let mw2 = rule_state(&o, "MW-2");
    assert_eq!(mw2["base_changed"], json!(false));
    assert_eq!(mw2["pack_override"]["base"], json!(2));
    assert_eq!(mw2["pack_override"]["action"], json!("hide"));
    assert_eq!(mw2["action"], json!("hide"));
    // In Undo's snapshot: Undo puts it back paused.
    let (status, _) = apply(&f, &c, &json!({"op": "undo"}), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
    let o = get(&f, &c, "/api/v1/filters").await;
    assert_eq!(
        rule_state(&o, "MW-2")["pack_override"]["paused"],
        json!(true)
    );
}

#[tokio::test]
async fn re_confirming_a_strengthening_override_needs_no_password() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    let set = json!({ "op": "set_pack_override", "rule": "MW-2", "action": "drop" });
    assert_eq!(apply(&f, &c, &set, None).await.0, StatusCode::OK);
    assert_eq!(
        apply(&f, &c, &accept(&["MW-2"], None), None).await.0,
        StatusCode::OK
    );
    let confirm = json!({ "op": "confirm_pack_override", "rule": "MW-2" });
    let p = preview(&f, &c, &confirm).await;
    assert_eq!(p["weakening"], json!(false), "{p}");
    let (status, body) = apply(&f, &c, &confirm, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let o = get(&f, &c, "/api/v1/filters").await;
    assert_eq!(rule_state(&o, "MW-2")["pack_override"]["base"], json!(2));
    // No override: nothing to confirm.
    let none = json!({ "op": "confirm_pack_override", "rule": "MW-1" });
    let (status, body) = apply(&f, &c, &none, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn the_review_queue_marks_holds_acceptance_left_for_review() {
    let f = fixture(bumped(Direction::Narrows, Action::Hide));
    let c = logged_in(&f).await;
    {
        let store = f.api.store.lock().unwrap();
        let conn = store.connection();
        for (n, rule) in [(1u8, "MW-2"), (2, "MW-2"), (3, "MW-1")] {
            conn.execute(
                "INSERT INTO quarantine (hash, rule, sealed, at, expires_at)
                 VALUES (?1, ?2, NULL, 1, 9999999999)",
                lodestar_store::rusqlite::params![vec![n; 20], rule],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'MW-2', 1)",
            [vec![4u8; 20]],
        )
        .unwrap();
    }
    // The new version still holds 2.
    hit(&f, "MW-2", 2, Action::Hide, true, 100);
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    let impact = &p["pack_acceptance"][0];
    assert_eq!(impact["review_quarantine"], json!(1), "{p}");
    assert_eq!(impact["review_tombstones"], json!(1));
    assert_eq!(p["preview"]["quarantine"], json!(0), "nothing is released");
    let (status, body) = apply(
        &f,
        &c,
        &accept(&["MW-2"], Some(&["MW-2@2"])),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let q = get(&f, &c, "/api/v1/filters/queue").await;
    let mut marked: Vec<(String, String, bool)> = q["quarantine"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["hash"].as_str().unwrap()[..2].to_owned(),
                e["rule"].as_str().unwrap().to_owned(),
                e["pack_review"].as_bool().unwrap(),
            )
        })
        .collect();
    marked.sort();
    assert_eq!(
        marked,
        [
            ("01".into(), "MW-2".into(), true),
            ("02".into(), "MW-2".into(), false),
            ("03".into(), "MW-1".into(), false),
        ]
    );
    let t = get(&f, &c, "/api/v1/filters/tombstones").await;
    assert_eq!(t["tombstones"][0]["pack_review"], json!(true));
    assert_eq!(t["tombstones"][0]["rule"], json!("MW-2"));
    // Released, it leaves the review row: the row never keeps the hash of
    // a torrent nothing holds.
    let release = json!({ "op": "release", "hash": "01".repeat(20) });
    let (status, body) = apply(&f, &c, &release, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = f
        .api
        .store
        .lock()
        .unwrap()
        .setting_text("filter.packs.review")
        .unwrap();
    assert_eq!(row, Some(format!("MW-2 {}", "04".repeat(20))));
}

#[tokio::test]
async fn fixtures_report_the_pack_corpora_per_rule() {
    let f = fixture(Manifest::builtin());
    let c = logged_in(&f).await;
    let fx = get(&f, &c, "/api/v1/filters/fixtures").await;
    let packs = &fx["packs"];
    assert_eq!(packs["error"], json!(null));
    assert_eq!(packs["passes"], json!(true), "{}", packs["failures"]);
    assert_eq!(packs["failures"], json!([]));
    assert_eq!(packs["precision_failing"], json!(0));
    assert!(packs["precision_items"].as_u64().unwrap() >= 20);
    let rules = packs["rules"].as_array().unwrap();
    // The stateful rules (SP-3, RP-4) have sequence fixtures, not a
    // per-item corpus, so the report leaves them out.
    let stateless = PackRule::ALL
        .into_iter()
        .filter(|r| lodestar_filter::corpus::stateless(*r))
        .count();
    assert_eq!(rules.len(), stateless);
    assert!(rules.iter().all(|r| r["rule"] != "SP-3"));
    for r in rules {
        assert!(r["must_match"].as_u64().unwrap() >= 3, "{r}");
        assert!(r["must_not_match"].as_u64().unwrap() >= 3, "{r}");
        assert_eq!(r["failing"], json!(0), "{r}");
        assert_eq!(r["direction_disagreements"], json!(0), "{r}");
        let id = PackRule::from_id(r["rule"].as_str().unwrap()).unwrap();
        let shipped = Manifest::builtin().entry(id).current().version;
        assert_eq!(r["version"], json!(shipped), "{r}");
    }
    // RP-1's known gaps showed per rule, so they never read as a clean
    // pass; RP-1b built the last two guards, so none is left.
    for r in rules {
        assert_eq!(r["gaps"], json!(0), "{r}");
    }
    // The core part is as it was.
    assert!(fx["core"].as_array().is_some_and(|c| !c.is_empty()));
}

/// A review row that doesn't read fails closed: it marks nothing, never
/// blocks a page or a change, releases nothing, is reported, and the next
/// acceptance that leaves holds for review replaces it.
#[tokio::test]
async fn an_unreadable_review_row_fails_closed() {
    let f = fixture(bumped(Direction::Narrows, Action::Hide));
    let c = logged_in(&f).await;
    {
        let store = f.api.store.lock().unwrap();
        let conn = store.connection();
        conn.execute(
            "INSERT INTO quarantine (hash, rule, sealed, at, expires_at)
             VALUES (?1, 'MW-2', NULL, 1, 9999999999)",
            [vec![1u8; 20]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('filter.packs.review', x'00ff')",
            [],
        )
        .unwrap();
    }
    let q = get(&f, &c, "/api/v1/filters/queue").await;
    assert_eq!(q["quarantine"][0]["pack_review"], json!(false));
    let t = get(&f, &c, "/api/v1/filters/tombstones").await;
    assert_eq!(t["tombstones"], json!([]));
    let o = get(&f, &c, "/api/v1/filters").await;
    assert_eq!(o["pack_conditions"], json!(["packs.review-unreadable"]));
    // A change still applies, and the hold stays held.
    let (status, body) = apply(
        &f,
        &c,
        &json!({"op": "pack_switch", "id": "spam", "on": true}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = || -> i64 {
        f.api
            .store
            .lock()
            .unwrap()
            .connection()
            .query_row("SELECT count(*) FROM quarantine", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(held(), 1);
    // The next acceptance that leaves holds for review replaces it.
    let (status, body) = apply(
        &f,
        &c,
        &accept(&["MW-2"], Some(&["MW-2@2"])),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(held(), 1, "nothing released");
    let q = get(&f, &c, "/api/v1/filters/queue").await;
    assert_eq!(q["quarantine"][0]["pack_review"], json!(true));
    let o = get(&f, &c, "/api/v1/filters").await;
    assert!(
        !o["pack_conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "packs.review-unreadable")
    );
}

/// Where the shadow started is kept across restarts: applied-only holds
/// made while it ran still disagree after one, and holds from before it
/// never count.
#[tokio::test]
async fn shadow_evidence_survives_a_restart() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    // No start recorded yet: every hit in the window counts, so a hold
    // only version 1 made reads as a disagreement.
    hit(&f, "MW-2", 9, Action::Hide, false, 25 * 86_400);
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["weakening"], json!(true), "{p}");
    // The start records where the shadow begins: 9 is from before it.
    f.api.resume_requeues(now()).unwrap();
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["weakening"], json!(false), "{p}");
    // While it runs, version 1 holds 2 and the new version doesn't.
    hit(&f, "MW-2", 2, Action::Hide, false, 10);
    // A restart, with nothing the new version has held since.
    f.api.resume_requeues(now()).unwrap();
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["weakening"], json!(true), "{p}");
    assert_eq!(p["pack_acceptance"][0]["disagreements"], json!(1));
    needs_password(&f, &c, "applied-only before a restart").await;
}

/// An applied hit counts as the applied version's own action, whatever
/// action an override gave it: a heavier override doesn't read as a
/// disagreement.
#[tokio::test]
async fn an_override_does_not_skew_the_comparison() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    let set = json!({ "op": "set_pack_override", "rule": "MW-2", "action": "drop" });
    assert_eq!(apply(&f, &c, &set, None).await.0, StatusCode::OK);
    f.api.resume_requeues(now()).unwrap();
    // Version 1 held it and the override dropped it; version 2 hides it.
    hit(&f, "MW-2", 1, Action::Drop, false, 50);
    hit(&f, "MW-2", 1, Action::Hide, true, 50);
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["weakening"], json!(false), "{p}");
    assert_eq!(p["pack_acceptance"][0]["disagreements"], json!(0));
}

/// A rule that's switched off runs no shadow and has nothing waiting:
/// switching it on records its current version.
#[tokio::test]
async fn a_rule_that_is_off_has_nothing_pending() {
    let f = fixture(bumped(Direction::Narrows, Action::Hide));
    let c = logged_in(&f).await;
    let off = json!({ "op": "pack_switch", "id": "MW-2", "on": false });
    let (status, body) = apply(&f, &c, &off, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let o = get(&f, &c, "/api/v1/filters").await;
    assert_eq!(o["pending_pack_versions"], json!([]));
    assert_eq!(rule_state(&o, "MW-2")["pending"], json!(null));
    let (status, body) = apply(&f, &c, &accept(&["MW-2"], None), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("not_pending"));
    let on = json!({ "op": "pack_switch", "id": "MW-2", "on": true });
    assert_eq!(apply(&f, &c, &on, None).await.0, StatusCode::OK);
    assert_eq!(applied(&f).get(PackRule::Mw2), Some(2));
}

fn real_mw1(c: &PackCtx<'_>) -> bool {
    c.matches(PackRule::Mw1)
}

/// MW-2 at version 2 widening, and MW-1 at version 2 narrowing.
fn strong_and_weak() -> Arc<Manifest> {
    let b = bumped(Direction::Widens, Action::Hide);
    let entries = PackRule::ALL
        .into_iter()
        .map(|r| {
            if r != PackRule::Mw1 {
                return b.entry(r).clone();
            }
            let a = r.action();
            RuleEntry::new(
                r,
                true,
                vec![
                    Version {
                        version: 1,
                        direction: Direction::Widens,
                        action: a,
                        code: Some(Code::Fn(real_mw1)),
                    },
                    Version {
                        version: 2,
                        direction: Direction::Narrows,
                        action: a,
                        code: Some(Code::Current),
                    },
                ],
            )
        })
        .collect();
    Arc::new(Manifest::new(entries).unwrap())
}

/// One acceptance of a strengthening and a weakening version is weakening
/// as a whole, and accepts both once confirmed.
#[tokio::test]
async fn a_batch_with_one_weakening_version_needs_the_password() {
    let f = fixture(strong_and_weak());
    let c = logged_in(&f).await;
    let change = accept(&["malware"], None);
    let p = preview(&f, &c, &change).await;
    assert_eq!(p["weakening"], json!(true), "{p}");
    assert_eq!(p["pack_versions"], json!(["MW-1@2", "MW-2@2"]));
    let by: Vec<(String, bool)> = p["pack_acceptance"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["rule"].as_str().unwrap().to_owned(),
                i["strengthens"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(by, [("MW-1".into(), false), ("MW-2".into(), true)]);
    assert!(p["warning"].as_str().unwrap().contains("MW-1@2"));
    assert!(!p["warning"].as_str().unwrap().contains("MW-2@2"));
    let (status, _) = apply(&f, &c, &change, None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let named = accept(&["malware"], Some(&["MW-1@2", "MW-2@2"]));
    let (status, body) = apply(&f, &c, &named, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rec = applied(&f);
    assert_eq!(
        (rec.get(PackRule::Mw1), rec.get(PackRule::Mw2)),
        (Some(2), Some(2))
    );
    // Naming more than every pack and rule is refused.
    let limit = lodestar_filter::Pack::ALL.len() + PackRule::ALL.len();
    let many: Vec<&str> = std::iter::repeat_n("MW-1", limit + 1).collect();
    let (status, body) = apply(&f, &c, &accept(&many, None), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("too_many_rules"));
}

fn trigger(f: &Fixture, key: &str) {
    let conn = f.api.store.lock().unwrap();
    for when in ["INSERT", "UPDATE"] {
        conn.connection()
            .execute_batch(&format!(
                "CREATE TRIGGER no_{when} BEFORE {when} ON settings WHEN NEW.key = '{key}'
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;"
            ))
            .unwrap();
    }
}

fn untrigger(f: &Fixture) {
    let conn = f.api.store.lock().unwrap();
    conn.connection()
        .execute_batch("DROP TRIGGER no_INSERT; DROP TRIGGER no_UPDATE;")
        .unwrap();
}

fn review_row(f: &Fixture) -> Option<String> {
    f.api
        .store
        .lock()
        .unwrap()
        .setting_text("filter.packs.review")
        .unwrap()
}

/// The review marks and the acceptance land together or not at all: a
/// review write that fails leaves nothing accepted, and an acceptance
/// that fails puts the review row back.
#[tokio::test]
async fn a_failed_write_leaves_nothing_half_committed() {
    let f = fixture(bumped(Direction::Narrows, Action::Hide));
    let c = logged_in(&f).await;
    f.api
        .store
        .lock()
        .unwrap()
        .connection()
        .execute(
            "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'MW-2', 1)",
            [vec![4u8; 20]],
        )
        .unwrap();
    let change = accept(&["MW-2"], Some(&["MW-2@2"]));
    // The review row can't be written: nothing is accepted.
    trigger(&f, "filter.packs.review");
    let (status, body) = apply(&f, &c, &change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    untrigger(&f);
    assert_eq!(applied(&f).get(PackRule::Mw2), Some(1));
    assert_eq!(review_row(&f), None);
    // The acceptance can't be written: the review row is put back.
    trigger(&f, "filter.packs.applied");
    let (status, body) = apply(&f, &c, &change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    untrigger(&f);
    assert_eq!(applied(&f).get(PackRule::Mw2), Some(1));
    assert_eq!(review_row(&f), None, "put back");
    // Both written: accepted and marked.
    let (status, body) = apply(&f, &c, &change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(applied(&f).get(PackRule::Mw2), Some(2));
    assert_eq!(review_row(&f), Some(format!("MW-2 {}", "04".repeat(20))));
}

/// A line of the review row that doesn't read is reported, not dropped
/// silently; the lines that read still mark their holds.
#[tokio::test]
async fn a_review_line_that_does_not_read_is_reported() {
    let f = fixture(baseline());
    let c = logged_in(&f).await;
    {
        let mut store = f.api.store.lock().unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO quarantine (hash, rule, sealed, at, expires_at)
                 VALUES (?1, 'MW-2', NULL, 1, 9999999999)",
                [vec![1u8; 20]],
            )
            .unwrap();
        let text = format!("MW-2 {}\nnot a line", "01".repeat(20));
        store
            .set_settings_text(&[("filter.packs.review", &text)])
            .unwrap();
    }
    let o = get(&f, &c, "/api/v1/filters").await;
    assert_eq!(o["pack_conditions"], json!(["packs.review-unreadable"]));
    let q = get(&f, &c, "/api/v1/filters/queue").await;
    assert_eq!(q["quarantine"][0]["pack_review"], json!(true));
}

fn since_row(f: &Fixture) -> Option<String> {
    f.api
        .store
        .lock()
        .unwrap()
        .setting_text("filter.packs.shadow_since")
        .unwrap()
}

fn clear_hits(f: &Fixture) {
    f.api
        .store
        .lock()
        .unwrap()
        .connection()
        .execute("DELETE FROM filter_hits", [])
        .unwrap();
}

/// `filter_hits.id` is reused once the table empties (the 30-day purge on
/// an idle store): a recorded anchor then can't be trusted, so acceptance
/// is weakening until confirmed, whether the new ids stay under the
/// anchor or pass it.
#[tokio::test]
async fn reused_hit_ids_put_the_shadow_evidence_in_doubt() {
    for refill in [2u8, 12] {
        let f = fixture(bumped(Direction::Widens, Action::Hide));
        let c = logged_in(&f).await;
        for n in 0..5 {
            hit(&f, "MW-1", n, Action::Hide, false, 1_000);
        }
        f.api.resume_requeues(now()).unwrap();
        let since = since_row(&f).unwrap();
        assert_eq!(since.split(':').next(), Some("MW-2@2 5"), "{since}");
        assert_eq!(
            preview(&f, &c, &accept(&["MW-2"], None)).await["weakening"],
            json!(false)
        );
        // Purged, then new hits reuse ids from 1.
        clear_hits(&f);
        for n in 0..refill {
            hit(&f, "MW-1", 100 + n, Action::Hide, false, 10);
        }
        let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
        assert_eq!(p["weakening"], json!(true), "refill {refill}: {p}");
        // The next start marks it, and it stays in doubt.
        f.api.resume_requeues(now()).unwrap();
        assert_eq!(
            since_row(&f).as_deref(),
            Some("MW-2@2 ?"),
            "refill {refill}"
        );
        let o = get(&f, &c, "/api/v1/filters").await;
        assert_eq!(o["pack_conditions"], json!(["packs.shadow-in-doubt"]));
        needs_password(&f, &c, "reused ids").await;
        assert_eq!(since_row(&f), None, "accepted: nothing pending");
    }
}

/// An anchor that aged out with every hit before it is no doubt: the
/// table just moved on.
#[tokio::test]
async fn an_anchor_that_aged_out_is_still_trusted() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    for n in 0..5 {
        hit(&f, "MW-1", n, Action::Hide, false, 1_000);
    }
    f.api.resume_requeues(now()).unwrap();
    hit(&f, "MW-2", 9, Action::Hide, true, 10);
    // The purge takes every hit up to the anchor.
    f.api
        .store
        .lock()
        .unwrap()
        .connection()
        .execute("DELETE FROM filter_hits WHERE id <= 5", [])
        .unwrap();
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["weakening"], json!(false), "{p}");
}

/// A `shadow_since` row or line that doesn't read fails closed: the
/// pending versions it may have held are in doubt, reported, and stay so.
#[tokio::test]
async fn an_unreadable_shadow_since_fails_closed() {
    for bad in ["x'00ff'", "'MW-2@2 nonsense'", "'garbage'"] {
        let f = fixture(bumped(Direction::Widens, Action::Hide));
        let c = logged_in(&f).await;
        f.api
            .store
            .lock()
            .unwrap()
            .connection()
            .execute(
                &format!(
                    "INSERT INTO settings (key, value) VALUES ('filter.packs.shadow_since', {bad})"
                ),
                [],
            )
            .unwrap();
        let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
        assert_eq!(p["weakening"], json!(true), "{bad}: {p}");
        let o = get(&f, &c, "/api/v1/filters").await;
        assert_eq!(
            o["pack_conditions"],
            json!(["packs.shadow-in-doubt"]),
            "{bad}"
        );
        f.api.resume_requeues(now()).unwrap();
        assert_eq!(since_row(&f).as_deref(), Some("MW-2@2 ?"), "{bad}");
        let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
        assert_eq!(p["weakening"], json!(true), "{bad}: still in doubt");
    }
}

/// A build that changes the pending version starts its evidence afresh:
/// the older version's results never count as the newer one's.
#[tokio::test]
async fn a_new_pending_version_starts_its_own_evidence() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    f.api.resume_requeues(now()).unwrap();
    assert!(since_row(&f).unwrap().starts_with("MW-2@2 "));
    // While MW-2@2 waited, version 1 held a torrent @2 didn't.
    hit(&f, "MW-2", 2, Action::Hide, false, 10);
    assert_eq!(
        preview(&f, &c, &accept(&["MW-2"], None)).await["weakening"],
        json!(true)
    );
    // A build with MW-2@3 (widening) replaces it.
    let three = mw2(&[
        (1, Direction::Widens, Action::Hide),
        (2, Direction::Widens, Action::Hide),
        (3, Direction::Widens, Action::Hide),
    ]);
    f.api.engine.update_engine(|e| e.manifest = three);
    f.api.resume_requeues(now()).unwrap();
    let since = since_row(&f).unwrap();
    assert!(
        since.starts_with("MW-2@3 ") && !since.contains("MW-2@2"),
        "{since}"
    );
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["pack_versions"], json!(["MW-2@3"]));
    assert_eq!(p["weakening"], json!(false), "{p}");
}

/// MW-6 as shipped (RP-UC): a rule added after RP-0, so an install seeded
/// before it has it pending in shadow wherever the malware pack is on.
/// Its first version widens, so accepting it while its shadow agrees
/// needs no password; once version 1 holds something MW-6 doesn't, it
/// does.
#[tokio::test]
async fn the_real_mw6_is_accepted_without_a_password_while_its_shadow_agrees() {
    let f = fixture_with(baseline(), &[PackRule::Mw6]);
    let c = logged_in(&f).await;
    assert!(
        lodestar_filter::packs::manifest::ADDED_AFTER_RP0.contains(&PackRule::Mw6),
        "MW-6 isn't a baseline rule"
    );
    let o = get(&f, &c, "/api/v1/filters").await;
    let pending = o["pending_pack_versions"].as_array().unwrap();
    let mw6 = pending.iter().find(|p| p["rule"] == "MW-6").unwrap();
    assert_eq!(mw6["applied"], json!(null));
    assert_eq!(mw6["direction"], json!("widens"));
    assert_eq!(mw6["pack"], json!("malware"));
    assert!(
        mw6["changelog"][0]["text"]
            .as_str()
            .is_some_and(|t| !t.is_empty()),
        "{mw6}"
    );
    f.api.resume_requeues(now()).unwrap();
    hit(&f, "MW-6", 3, Action::Hide, true, 30);
    let change = accept(&["MW-6"], None);
    let p = preview(&f, &c, &change).await;
    assert_eq!(p["weakening"], json!(false), "{p}");
    assert_eq!(p["pack_versions"], json!(["MW-6@1"]));
    assert_eq!(p["pack_acceptance"][0]["newly_held"], json!(1));
    let (status, body) = apply(&f, &c, &change, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(applied(&f).get(PackRule::Mw6), Some(1));
    assert!(
        !f.api
            .engine
            .current()
            .pack_versions
            .state(PackRule::Mw6)
            .pending()
    );

    // A new rule has no applied side, so an ordinary MW-6 hit can't come
    // from an applied version. Read as "only the applied version holds
    // it", it's a disagreement, so the acceptance fails closed.
    let f = fixture_with(baseline(), &[PackRule::Mw6]);
    let c = logged_in(&f).await;
    f.api.resume_requeues(now()).unwrap();
    hit(&f, "MW-6", 4, Action::Hide, false, 30);
    let p = preview(&f, &c, &accept(&["MW-6"], None)).await;
    assert_eq!(p["weakening"], json!(true), "{p}");
}

/// Two more ways an anchor stops being trustworthy, each in doubt: new
/// hits that refill exactly to the anchor's id (its row is another hit
/// now), and a purge that takes the anchor's row but leaves a lower id.
#[tokio::test]
async fn an_anchor_refilled_exactly_or_purged_alone_is_in_doubt() {
    for case in ["refill to the anchor", "anchor row purged"] {
        let f = fixture(bumped(Direction::Widens, Action::Hide));
        let c = logged_in(&f).await;
        for n in 0..5 {
            hit(&f, "MW-1", n, Action::Hide, false, 1_000);
        }
        f.api.resume_requeues(now()).unwrap();
        if case == "refill to the anchor" {
            clear_hits(&f);
            for n in 0..5 {
                hit(&f, "MW-1", 50 + n, Action::Hide, false, 10);
            }
        } else {
            f.api
                .store
                .lock()
                .unwrap()
                .connection()
                .execute("DELETE FROM filter_hits WHERE id = 5", [])
                .unwrap();
        }
        let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
        assert_eq!(p["weakening"], json!(true), "{case}: {p}");
        f.api.resume_requeues(now()).unwrap();
        assert_eq!(since_row(&f).as_deref(), Some("MW-2@2 ?"), "{case}");
    }
}

/// A version listed twice in `shadow_since` fails closed: neither line is
/// trusted.
#[tokio::test]
async fn a_version_listed_twice_is_in_doubt() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    f.api.resume_requeues(now()).unwrap();
    let line = since_row(&f).unwrap();
    f.api
        .store
        .lock()
        .unwrap()
        .set_settings_text(&[("filter.packs.shadow_since", &format!("{line}\n{line}"))])
        .unwrap();
    let p = preview(&f, &c, &accept(&["MW-2"], None)).await;
    assert_eq!(p["weakening"], json!(true), "{p}");
    let o = get(&f, &c, "/api/v1/filters").await;
    assert_eq!(o["pack_conditions"], json!(["packs.shadow-in-doubt"]));
    f.api.resume_requeues(now()).unwrap();
    assert_eq!(since_row(&f).as_deref(), Some("MW-2@2 ?"));
}
