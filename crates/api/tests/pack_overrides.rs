//! RP-2's API gates (plan 2026-11-04 §4.3): every weakening pack-rule
//! override is refused without the password (428) and every strengthening
//! one isn't; a base version change pauses weakening overrides and keeps
//! strengthening ones; the `changed_since_preview` race (§20.7); Undo
//! restores the previous overrides; an unreadable row fails closed and is
//! listed; and nothing records who or when.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::filters::{Change, FiltersApi};
use lodestar_api::{ApiState, Auth, router};
use lodestar_filter::lists::SharedIps;
use lodestar_filter::load::STAND_INS_KEY;
use lodestar_filter::packs::Ctx as PackCtx;
use lodestar_filter::packs::manifest::{Code, Direction, Manifest, RuleEntry, Version};
use lodestar_filter::packs::upgrade::{self, FRESH_KEY};
use lodestar_filter::{Action, Core, CsList, Engine, Handle, HeldSet, Lists, PackRule, Switches};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    api: Arc<FiltersApi>,
    manifest: Arc<Manifest>,
    _dir: tempfile::TempDir,
}

fn real_mw2(c: &PackCtx<'_>) -> bool {
    c.matches(PackRule::Mw2)
}

/// The built-in manifest, MW-2 with a version 2 that drops what version 1
/// hid.
fn bumped() -> Arc<Manifest> {
    bumped_to(Direction::ActionHeavier, Action::Drop)
}

/// The built-in manifest, MW-2 with a version 2 matching what version 1
/// (hide) did, with `action`.
fn bumped_to(direction: Direction, action: Action) -> Arc<Manifest> {
    let b = Manifest::builtin();
    let mut entries: Vec<RuleEntry> = PackRule::ALL
        .into_iter()
        .filter(|r| *r != PackRule::Mw2)
        .map(|r| b.entry(r).clone())
        .collect();
    let v = |version, direction, action| Version {
        version,
        direction,
        action,
        code: Some(Code::Fn(real_mw2)),
    };
    entries.push(RuleEntry::new(
        PackRule::Mw2,
        true,
        vec![
            v(1, Direction::Widens, Action::Hide),
            v(2, direction, action),
        ],
    ));
    Arc::new(Manifest::new(entries).unwrap())
}

/// A store an earlier build set up with every rule at version 1 and the
/// malware pack on, served with `manifest`.
fn fixture(manifest: Arc<Manifest>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    store.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    let v1 = upgrade::current_record(&Manifest::builtin()).encode();
    store
        .write_pack_versions([Some(&v1), Some(&v1)], Some(("malware", true)))
        .unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let mut engine = Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    ));
    engine.manifest = manifest.clone();
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
        manifest,
        _dir: dir,
    }
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Option<String>, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
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

async fn overview_rule(f: &Fixture, c: &str, rule: &str) -> Value {
    let (status, _, body) = call(
        &f.app,
        Request::get("/api/v1/filters")
            .header(header::COOKIE, c)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["packs"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p["rules"].as_array().unwrap().clone())
        .find(|r| r["id"] == rule)
        .unwrap()
}

fn set(rule: &str, action: &str, unless: Option<&str>, base: Option<u32>) -> Value {
    json!({ "op": "set_pack_override", "rule": rule, "action": action, "unless": unless, "base": base })
}

fn stored(f: &Fixture, rule: &str) -> Option<Result<String, Vec<u8>>> {
    let set = f.api.store.lock().unwrap().rule_set().unwrap();
    set.pack_overrides
        .into_iter()
        .find(|(r, _)| r == rule)
        .map(|(_, v)| v)
}

/// Applies a change the password was confirmed for, as the route does
/// once it has checked it.
fn confirmed(f: &Fixture, change: &Value) {
    let change: Change = serde_json::from_value(change.clone()).unwrap();
    let a = f.api.assess(&change, 5).unwrap();
    assert!(a.confirm);
    f.api.apply(&change, a, 5).unwrap();
}

const ACTIONS: [(&str, Action); 4] = [
    ("flag", Action::Flag),
    ("demote", Action::Demote),
    ("hide", Action::Hide),
    ("drop", Action::Drop),
];

#[tokio::test]
async fn every_weakening_override_needs_the_password_and_no_strengthening_one_does() {
    let f = fixture(Manifest::builtin());
    let c = logged_in(&f).await;
    for rule in PackRule::ALL {
        let id = rule.id();
        for (name, action) in ACTIONS {
            for unless in [None, Some("group:trusted")] {
                let change = set(id, name, unless, Some(1));
                let p = preview(&f, &c, &change).await;
                let weak = unless.is_some() || action < rule.action();
                assert_eq!(p["weakening"], json!(weak), "{id} {name} {unless:?}");
                assert_eq!(p["pack_base"], json!(1));
                let (status, body) = apply(&f, &c, &change, None).await;
                if weak {
                    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{id} {name}");
                    assert_eq!(body["error"]["code"], json!("confirm_required"), "{body}");
                    assert!(body["assessment"]["warning"].is_string());
                    assert_eq!(stored(&f, id), None, "nothing stored");
                    // Confirmed: the route checked the password (the rate
                    // limit allows only a few here, so it's the
                    // weakening_needs_the_password tests' to exercise).
                    confirmed(&f, &change);
                } else {
                    assert_eq!(status, StatusCode::OK, "{id} {name}: {body}");
                    assert_eq!(body["assessment"]["confirm"], json!(false));
                }
                let mut want = format!("1\n1\n{name}");
                if let Some(u) = unless {
                    want.push('\n');
                    want.push_str(u);
                }
                assert_eq!(stored(&f, id), Some(Ok(want)), "no author, time or reason");
                // Removing: weakening only for an action heavier than the
                // rule's own.
                let remove = json!({ "op": "remove_pack_override", "rule": id });
                let (status, _) = apply(&f, &c, &remove, None).await;
                if action > rule.action() {
                    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{id} {name}");
                    assert!(stored(&f, id).is_some());
                    confirmed(&f, &remove);
                } else {
                    assert_eq!(status, StatusCode::OK, "{id} {name} {unless:?}");
                }
                assert_eq!(stored(&f, id), None);
            }
        }
    }
}

#[tokio::test]
async fn a_lighter_action_than_the_override_before_it_weakens_too() {
    let f = fixture(Manifest::builtin());
    let c = logged_in(&f).await;
    // MW-2 hides; drop is heavier, so no password.
    let (status, _) = apply(&f, &c, &set("MW-2", "drop", None, None), None).await;
    assert_eq!(status, StatusCode::OK);
    // Back to hide: the rule's own action, but lighter than what applies.
    let (status, _) = apply(&f, &c, &set("MW-2", "hide", None, None), None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    // The password needs the base it confirms.
    let (status, body) = apply(&f, &c, &set("MW-2", "hide", None, None), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("pack_override_needs_base"));
    let (status, _) = apply(&f, &c, &set("MW-2", "hide", None, Some(1)), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn bad_input_is_refused_before_anything_is_stored() {
    let f = fixture(Manifest::builtin());
    let c = logged_in(&f).await;
    for (change, code) in [
        (set("MW-9", "hide", None, None), "unknown_pack_rule"),
        (set("malware", "hide", None, None), "unknown_pack_rule"),
        (set("CS-3a", "hide", None, None), "unknown_pack_rule"),
        (set("MW-2", "quarantine", None, None), "unknown_action"),
        (set("MW-2", "explode", None, None), "unknown_action"),
        (set("MW-2", "drop", Some("ext:"), Some(1)), "query_syntax"),
        (
            set("MW-2", "drop", Some("-network:bt"), Some(1)),
            "provenance_in_rule",
        ),
    ] {
        let (status, body) = apply(&f, &c, &change, Some(PASSWORD)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{change}: {body}");
        assert_eq!(body["error"]["code"], json!(code), "{body}");
    }
    let (status, _) = apply(
        &f,
        &c,
        &json!({ "op": "remove_pack_override", "rule": "MW-2" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A blank `unless` is none (blank is the only text that parses as
    // matching everything): hide on a hide rule, no password.
    let (status, body) = apply(&f, &c, &set("MW-2", "hide", Some("  \t "), None), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stored(&f, "MW-2"), Some(Ok("1\n1\nhide".into())));
    f.api
        .store
        .lock()
        .unwrap()
        .set_pack_override("MW-2", None)
        .unwrap();
    assert!(
        f.api
            .store
            .lock()
            .unwrap()
            .rule_set()
            .unwrap()
            .pack_overrides
            .is_empty()
    );
}

#[tokio::test]
async fn a_base_change_pauses_weakening_overrides_and_keeps_strengthening_ones() {
    let f = fixture(bumped());
    let c = logged_in(&f).await;
    // Weakening against version 1: demote under hide.
    let (status, body) = apply(
        &f,
        &c,
        &set("MW-2", "demote", None, Some(1)),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let o = overview_rule(&f, &c, "MW-2").await;
    assert_eq!(
        o["pack_override"],
        json!({"action": "demote", "base": 1, "changed": false, "paused": false,
               "unless": null, "unreadable": false})
    );
    f.api.accept_pack_versions(&[PackRule::Mw2], 10).unwrap();
    let o = overview_rule(&f, &c, "MW-2").await;
    assert_eq!(o["pack_override"]["changed"], json!(true));
    assert_eq!(o["pack_override"]["paused"], json!(true));
    let e = f.api.engine.current();
    let l = e.pack_overrides.get(PackRule::Mw2).unwrap();
    assert!(l.paused, "the engine pauses it too");
    // Confirming it again against version 2 takes the password and binds
    // it there.
    let change = set("MW-2", "demote", None, Some(2));
    assert_eq!(preview(&f, &c, &change).await["pack_base"], json!(2));
    let (status, _) = apply(&f, &c, &change, None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let (status, _) = apply(&f, &c, &change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
    let o = overview_rule(&f, &c, "MW-2").await;
    assert_eq!(o["pack_override"]["base"], json!(2));
    assert_eq!(o["pack_override"]["changed"], json!(false));

    // Strengthening against version 1 (drop over hide): kept, flagged.
    let f = fixture(bumped());
    let c = logged_in(&f).await;
    let (status, _) = apply(&f, &c, &set("MW-2", "drop", None, None), None).await;
    assert_eq!(status, StatusCode::OK);
    f.api.accept_pack_versions(&[PackRule::Mw2], 10).unwrap();
    let o = overview_rule(&f, &c, "MW-2").await;
    assert_eq!(o["pack_override"]["changed"], json!(true));
    assert_eq!(o["pack_override"]["paused"], json!(false));
}

#[tokio::test]
async fn changed_since_preview_refuses_what_weakens_by_apply() {
    let f = fixture(bumped());
    let c = logged_in(&f).await;
    // Hide is version 1's own action: strengthening, no password asked.
    let change: Change = serde_json::from_value(set("MW-2", "hide", None, None)).unwrap();
    let previewed = f.api.assess(&change, 5).unwrap();
    assert!(!previewed.confirm);
    // Version 2 (drop) is accepted before the apply: hide now weakens.
    f.api.accept_pack_versions(&[PackRule::Mw2], 10).unwrap();
    let err = f.api.apply(&change, previewed, 11).unwrap_err();
    assert_eq!(err.code(), "changed_since_preview");
    assert!(stored(&f, "MW-2").is_none());

    // Confirmed with the password for version 1, applied after version 2
    // was accepted: the password confirmed a rule that no longer applies.
    let f = fixture(bumped());
    let c2 = logged_in(&f).await;
    let change = set("MW-2", "demote", None, Some(1));
    assert_eq!(preview(&f, &c2, &change).await["pack_base"], json!(1));
    f.api.accept_pack_versions(&[PackRule::Mw2], 10).unwrap();
    let (status, body) = apply(&f, &c2, &change, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("changed_since_preview"));
    assert!(stored(&f, "MW-2").is_none());
    let _ = c;
}

#[tokio::test]
async fn a_weakening_override_waits_while_the_version_is_in_doubt() {
    let f = fixture(Manifest::builtin());
    let c = logged_in(&f).await;
    f.api
        .store
        .lock()
        .unwrap()
        .update_settings_text(&[], &upgrade::APPLIED_KEYS)
        .unwrap();
    let (status, body) = apply(&f, &c, &set("MW-2", "flag", None, Some(0)), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("pack_rule_in_doubt"));
    // A strengthening one is bound to no version, and keeps applying.
    let (status, body) = apply(&f, &c, &set("MW-2", "drop", None, None), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["assessment"]["pack_base"], json!(0));
    let o = overview_rule(&f, &c, "MW-2").await;
    assert_eq!(o["pack_override"]["changed"], json!(true));
    assert_eq!(o["pack_override"]["paused"], json!(false));
}

#[tokio::test]
async fn changed_since_preview_covers_removal_too() {
    // Version 2 only demotes: a hide override removed after it's accepted
    // lightens the rule.
    let f = fixture(bumped_to(Direction::ActionLighter, Action::Demote));
    let c = logged_in(&f).await;
    let (status, _) = apply(&f, &c, &set("MW-2", "hide", None, None), None).await;
    assert_eq!(status, StatusCode::OK);
    let remove: Change =
        serde_json::from_value(json!({ "op": "remove_pack_override", "rule": "MW-2" })).unwrap();
    let previewed = f.api.assess(&remove, 5).unwrap();
    assert!(
        !previewed.confirm,
        "hide over hide: removing lightens nothing"
    );
    f.api.accept_pack_versions(&[PackRule::Mw2], 10).unwrap();
    let o = overview_rule(&f, &c, "MW-2").await;
    assert_eq!(o["pack_override"]["changed"], json!(true));
    assert_eq!(
        o["pack_override"]["paused"],
        json!(false),
        "heavier than v2"
    );
    let err = f.api.apply(&remove, previewed, 11).unwrap_err();
    assert_eq!(err.code(), "changed_since_preview");
    assert!(stored(&f, "MW-2").is_some());
}

#[tokio::test]
async fn undo_across_a_base_change_restores_the_override_paused() {
    let f = fixture(bumped());
    let c = logged_in(&f).await;
    let (status, _) = apply(
        &f,
        &c,
        &set("MW-2", "demote", None, Some(1)),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = apply(&f, &c, &set("SP-1", "hide", None, None), None).await;
    assert_eq!(status, StatusCode::OK);
    f.api.accept_pack_versions(&[PackRule::Mw2], 10).unwrap();
    // Undo goes back to the set before SP-1's override: MW-2's demote,
    // made against version 1, which no longer applies.
    let (status, _) = apply(&f, &c, &json!({ "op": "undo" }), Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored(&f, "MW-2"), Some(Ok("1\n1\ndemote".into())));
    assert_eq!(stored(&f, "SP-1"), None);
    let o = overview_rule(&f, &c, "MW-2").await;
    assert_eq!(o["pack_override"]["changed"], json!(true));
    assert_eq!(o["pack_override"]["paused"], json!(true));
    let e = f.api.engine.current();
    assert!(e.pack_overrides.get(PackRule::Mw2).unwrap().paused);
    assert_eq!(
        e.pack_versions.state(PackRule::Mw2).applied,
        Some(2),
        "Undo never restores versions"
    );
}

#[tokio::test]
async fn undo_restores_the_previous_overrides() {
    let f = fixture(Manifest::builtin());
    let c = logged_in(&f).await;
    let (status, _) = apply(&f, &c, &set("MW-2", "drop", None, None), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = apply(
        &f,
        &c,
        &set("SP-1", "flag", Some("group:x"), Some(1)),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let undo = json!({ "op": "undo" });
    let (status, _) = apply(&f, &c, &undo, None).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    let (status, _) = apply(&f, &c, &undo, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored(&f, "MW-2"), Some(Ok("1\n1\ndrop".into())));
    assert_eq!(stored(&f, "SP-1"), None);
    assert!(
        f.api
            .engine
            .current()
            .pack_overrides
            .get(PackRule::Sp1)
            .is_none()
    );
    // A second Undo redoes.
    let (status, _) = apply(&f, &c, &undo, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored(&f, "SP-1"), Some(Ok("1\n1\nflag\ngroup:x".into())));
    assert!(
        f.api
            .engine
            .current()
            .pack_overrides
            .get(PackRule::Sp1)
            .is_some()
    );
}

#[tokio::test]
async fn an_unreadable_row_fails_closed_is_listed_and_can_be_removed() {
    let f = fixture(Manifest::builtin());
    let c = logged_in(&f).await;
    f.api
        .store
        .lock()
        .unwrap()
        .set_pack_override("MW-2", Some("1\n1\ndemote\nsize:>>"))
        .unwrap();
    // A change reloads the engine and records the conditions.
    let (status, _) = apply(&f, &c, &set("SP-1", "hide", None, None), None).await;
    assert_eq!(status, StatusCode::OK);
    let e = f.api.engine.current();
    assert_eq!(e.unreadable.len(), 1);
    assert_eq!(e.unreadable[0].label(), "packs.override.MW-2");
    let o = overview_rule(&f, &c, "MW-2").await;
    assert_eq!(o["pack_override"]["unreadable"], json!(true));
    assert_eq!(o["pack_override"]["unless"], json!(null));
    let record = f
        .api
        .store
        .lock()
        .unwrap()
        .setting_text(STAND_INS_KEY)
        .unwrap();
    assert!(
        record
            .as_deref()
            .is_some_and(|r| r.contains("packs.override.MW-2@")),
        "{record:?}"
    );
    // Removing it restores the rule as built, which its stand-in already
    // applied: strengthening, no password.
    let (status, _) = apply(
        &f,
        &c,
        &json!({ "op": "remove_pack_override", "rule": "MW-2" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(f.api.engine.current().unreadable.is_empty());
    let record = f
        .api
        .store
        .lock()
        .unwrap()
        .setting_text(STAND_INS_KEY)
        .unwrap();
    assert!(
        !record.as_deref().unwrap_or("").contains("packs.override"),
        "{record:?}"
    );
    let _ = &f.manifest;
}
