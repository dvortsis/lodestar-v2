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

fn real_mw2(c: &PackCtx<'_>) -> bool {
    c.matches(PackRule::Mw2)
}

/// The built-in manifest with MW-2's versions replaced by `versions`
/// (number, direction, action), each matching as MW-2 does today.
fn mw2(versions: &[(u32, Direction, Action)]) -> Arc<Manifest> {
    let b = Manifest::builtin();
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
    let b = Manifest::builtin();
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
    let mut record = upgrade::current_record(&Manifest::builtin());
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
            json!({ "token": token, "password": PASSWORD }),
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
        json!("MW-4@1 MW-1@1 MW-2@2 MW-3@1 MW-5@1")
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
            "changelog": [{"direction": "widens", "text": null, "version": 2}],
        }])
    );
    assert_eq!(o["pack_conditions"], json!([]));
    assert_eq!(o["packs_seeded_over_rules"], json!(null));
}

#[tokio::test]
async fn a_widening_acceptance_whose_shadow_agrees_needs_no_password() {
    let f = fixture(bumped(Direction::Widens, Action::Hide));
    let c = logged_in(&f).await;
    // Only the new version holds 1; both hold 2 the same way; 3 was held
    // by version 1 before the shadow ran, so it says nothing.
    hit(&f, "MW-2", 1, Action::Hide, true, 100);
    hit(&f, "MW-2", 2, Action::Hide, true, 90);
    hit(&f, "MW-2", 2, Action::Hide, false, 90);
    hit(&f, "MW-2", 3, Action::Hide, false, 20 * 86_400);
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
    assert_eq!(
        shadow_hits(&f, "MW-2"),
        0,
        "the accepted version's shadow is forgotten"
    );
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
        let mut rec = upgrade::current_record(&Manifest::builtin());
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
        let a = upgrade::current_record(&Manifest::builtin());
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
    assert_eq!(rules.len(), PackRule::ALL.len());
    for r in rules {
        assert!(r["must_match"].as_u64().unwrap() >= 3, "{r}");
        assert!(r["must_not_match"].as_u64().unwrap() >= 3, "{r}");
        assert_eq!(r["failing"], json!(0), "{r}");
        assert_eq!(r["version"], json!(1));
    }
    // RP-1's known gaps show per rule, so they never read as a clean pass.
    let gaps = |id: &str| {
        rules.iter().find(|r| r["rule"] == id).unwrap()["gaps"]
            .as_u64()
            .unwrap()
    };
    assert!(gaps("MW-2") >= 1 && gaps("FK-2") >= 1);
    assert_eq!(gaps("MW-1"), 0);
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
