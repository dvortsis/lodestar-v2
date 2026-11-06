//! The rule-pack gate's guardrails (RP-G; plan 2026-11-04 §4.3,
//! architecture §20.11): through the API, every operation the rule packs
//! added (RP-0 to RP-6) is refused without the password wherever it
//! weakens filtering, and so is every older operation aimed at a pack
//! rule they added. Each refusal leaves the rule set as it was: without a
//! password (428 `confirm_required`), and with a wrong one. The same
//! change confirmed with the password, and with what its preview asked to
//! be sent back, applies.
//!
//! The list of operations is checked against [`Change`] with an
//! exhaustive match ([`new_since_rp0`]), so an operation added later
//! fails to compile here until it's classified, and the scenarios must
//! cover every one marked new.
//!
//! The corpora, the budget and the upgrade test are in `crates/filter`
//! (`tests/packs_gate.rs` and `tests/props.rs`).

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::filters::{Change, FiltersApi};
use lodestar_api::{ApiState, Auth, router};
use lodestar_filter::lists::SharedIps;
use lodestar_filter::packs::manifest::{ADDED_AFTER_RP0, Manifest};
use lodestar_filter::packs::overrides::PackOverride;
use lodestar_filter::packs::upgrade::{self, FRESH_KEY};
use lodestar_filter::packs::{Pack, PackRule};
use lodestar_filter::{Action, Core, CsList, Engine, Handle, HeldSet, Lists, Switches};
use lodestar_store::{RuleSet, Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// Whether an operation came with the rule packs (RP-0 to RP-6). Every
/// variant is named, so a new one doesn't compile until it's sorted.
fn new_since_rp0(c: &Change) -> bool {
    match c {
        Change::SetPackOverride { .. }
        | Change::RemovePackOverride { .. }
        | Change::ConfirmPackOverride { .. }
        | Change::AcceptPackVersions { .. } => true,
        Change::CoreSwitch { .. }
        | Change::PackSwitch { .. }
        | Change::AddRule { .. }
        | Change::UpdateRule { .. }
        | Change::RemoveRule { .. }
        | Change::AddOverride { .. }
        | Change::RemoveOverride { .. }
        | Change::AddException { .. }
        | Change::RemoveException { .. }
        | Change::AddAllow { .. }
        | Change::RemoveAllow { .. }
        | Change::RemoveCs1 { .. }
        | Change::ReportAbuse { .. }
        | Change::FalsePositive { .. }
        | Change::Release { .. }
        | Change::Confirm { .. }
        | Change::RevealName { .. }
        | Change::Subscribe { .. }
        | Change::Unsubscribe { .. }
        | Change::PauseSubscription { .. }
        | Change::ResolveHeld { .. }
        | Change::ResolveFloorRemoval { .. }
        | Change::Undo => false,
    }
}

/// The pack rules the rule packs added: SC-2, split out of SC-1 at RP-0,
/// and every rule added after it.
fn new_rules() -> Vec<PackRule> {
    std::iter::once(PackRule::Sc2)
        .chain(ADDED_AFTER_RP0.iter().copied())
        .collect()
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    api: Arc<FiltersApi>,
    _dir: tempfile::TempDir,
}

/// The shipped build with every pack on. `upgraded`: an install an
/// earlier build set up, so MW-2@2 and FK-2@2 (which narrow) and the
/// rules added after RP-0 are pending; otherwise one at every current
/// version.
fn fixture(upgraded: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    store.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    let m = Manifest::builtin();
    let record = if upgraded {
        upgrade::seed_record(&m)
    } else {
        upgrade::current_record(&m)
    }
    .encode();
    store
        .write_pack_versions([Some(&record), Some(&record)], None)
        .unwrap();
    for p in Pack::ALL {
        store.set_pack_switch(p.as_str(), true).unwrap();
    }
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let engine = Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    ));
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
    reload(&api);
    let pending = api.engine.current().pack_versions.pending().count();
    assert_eq!(pending > 0, upgraded, "{pending} pending");
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

/// The engine as the store has it, as start loads it.
fn reload(api: &FiltersApi) {
    let set = api.store.lock().unwrap().rule_set().unwrap();
    let notes = api.engine.update_engine(|e| e.load_rules(&set));
    assert!(notes.is_empty(), "{notes:?}");
}

fn rule_set(f: &Fixture) -> RuleSet {
    f.api.store.lock().unwrap().rule_set().unwrap()
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
    assert_eq!(status, StatusCode::OK, "{change}: {body}");
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

/// One weakening change, gated: previewed as weakening; refused without
/// the password and with a wrong one, the rule set untouched; then
/// applied with the password and what the preview asks to be confirmed
/// (`base` for an override, `versions` for an acceptance).
async fn gated(f: &Fixture, c: &str, change: Value) {
    let p = preview(f, c, &change).await;
    assert_eq!(p["weakening"], json!(true), "{change}: {p}");
    assert_eq!(p["confirm"], json!(true), "{change}: {p}");
    assert!(
        p["warning"].as_str().is_some_and(|w| !w.is_empty()),
        "{change}: a plain-language warning: {p}"
    );
    let before = rule_set(f);
    let version = f.api.engine.current().version;
    let (status, body) = apply(f, c, &change, None).await;
    assert_eq!(
        status,
        StatusCode::PRECONDITION_REQUIRED,
        "{change}: {body}"
    );
    assert_eq!(body["error"]["code"], json!("confirm_required"), "{change}");
    assert_eq!(rule_set(f), before, "{change}: nothing changed without it");
    let mut confirmed = change.clone();
    match change["op"].as_str().unwrap() {
        "set_pack_override" | "confirm_pack_override" => {
            confirmed["base"] = p["pack_base"].clone();
            assert!(p["pack_base"].is_u64(), "{change}: {p}");
        }
        "accept_pack_versions" => {
            confirmed["versions"] = p["pack_versions"].clone();
            assert!(p["pack_versions"].is_array(), "{change}: {p}");
        }
        _ => {}
    }
    let (status, body) = apply(f, c, &confirmed, Some("not the password")).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "{change}: a wrong password: {body}"
    );
    assert_eq!(body["error"]["code"], json!("bad_password"), "{change}");
    assert_eq!(
        rule_set(f),
        before,
        "{change}: nothing changed with a wrong one"
    );
    assert_eq!(f.api.engine.current().version, version, "{change}");
    let (status, body) = apply(f, c, &confirmed, Some(PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{confirmed}: {body}");
    assert_ne!(rule_set(f), before, "{change}: applied with it");
}

/// Writes an override row directly (no password spent), then reloads.
fn stored_override(f: &Fixture, rule: PackRule, base: u32, action: Action) {
    let row = PackOverride {
        base,
        action,
        unless: None,
    }
    .encode();
    f.api
        .store
        .lock()
        .unwrap()
        .set_pack_override(rule.id(), Some(&row))
        .unwrap();
    reload(&f.api);
}

/// The scenarios: each weakening form of each new operation, and the
/// older operations aimed at the new rules. Each runs on its own fixture
/// (the password checks per address are rate-limited).
#[tokio::test]
async fn every_new_op_that_weakens_needs_the_password() {
    let mut covered = BTreeSet::new();
    let mut run = async |upgraded: bool, setup: &dyn Fn(&Fixture), change: Value| {
        let parsed: Change = serde_json::from_value(change.clone()).unwrap();
        if new_since_rp0(&parsed) {
            covered.insert(change["op"].as_str().unwrap().to_owned());
        }
        let f = fixture(upgraded);
        setup(&f);
        let c = logged_in(&f).await;
        gated(&f, &c, change).await;
    };
    let none = |_: &Fixture| {};
    // An override lighter than the rule's own action, and one with an
    // `unless` (MW-1 hides).
    let set = json!({ "op": "set_pack_override", "rule": "MW-1", "action": "flag" });
    run(false, &none, set).await;
    let set =
        json!({ "op": "set_pack_override", "rule": "MW-1", "action": "hide", "unless": "trailer" });
    run(false, &none, set).await;
    // On a new rule too.
    let set = json!({ "op": "set_pack_override", "rule": "SP-4", "action": "demote" });
    run(false, &none, set).await;
    // Removing an override heavier than the rule's action.
    let heavier = |f: &Fixture| stored_override(f, PackRule::Mw1, 1, Action::Drop);
    let remove = json!({ "op": "remove_pack_override", "rule": "MW-1" });
    run(false, &heavier, remove).await;
    // Accepting a version that narrows: the shipped MW-2@2 and FK-2@2 on
    // an install that applied version 1.
    for rule in ["MW-2", "FK-2"] {
        let accept = json!({ "op": "accept_pack_versions", "rules": [rule] });
        run(true, &none, accept).await;
    }
    // A pack named whole, with a narrowing version among its pending.
    let accept = json!({ "op": "accept_pack_versions", "rules": ["malware"] });
    run(true, &none, accept).await;
    // Confirming again an override the acceptance paused: MW-2 flagged
    // against version 1, then version 2 accepted.
    let paused = |f: &Fixture| {
        stored_override(f, PackRule::Mw2, 1, Action::Flag);
        let mut s = f.api.store.lock().unwrap();
        upgrade::accept(&mut s, &Manifest::builtin(), &[PackRule::Mw2], 0).unwrap();
        drop(s);
        reload(&f.api);
        let e = f.api.engine.current();
        let o = e.pack_overrides.get(PackRule::Mw2).unwrap();
        assert!(o.changed && o.paused, "{o:?}");
    };
    let confirm = json!({ "op": "confirm_pack_override", "rule": "MW-2" });
    run(true, &paused, confirm).await;
    // The older operations, aimed at the new rules: switching one off,
    // and an exception to it.
    for rule in new_rules() {
        let off = json!({ "op": "pack_switch", "id": rule.id(), "on": false });
        run(false, &none, off).await;
        let exception = json!({ "op": "add_exception", "rule": rule.id(), "query": "example" });
        run(false, &none, exception).await;
    }
    let off = json!({ "op": "pack_switch", "id": "unauthorised-copy", "on": false });
    run(false, &none, off).await;
    let exception = json!({ "op": "add_exception", "rule": "SP-4", "payload": "00ff00ff00ff00f8" });
    run(false, &none, exception).await;

    // Every new operation is covered.
    let all = [
        json!({ "op": "set_pack_override", "rule": "MW-1", "action": "flag" }),
        json!({ "op": "remove_pack_override", "rule": "MW-1" }),
        json!({ "op": "confirm_pack_override", "rule": "MW-1" }),
        json!({ "op": "accept_pack_versions", "rules": ["MW-2"] }),
    ];
    let new: BTreeSet<String> = all
        .iter()
        .filter(|c| new_since_rp0(&serde_json::from_value((*c).clone()).unwrap()))
        .map(|c| c["op"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(new.len(), 4);
    assert_eq!(covered, new, "every new operation has a weakening scenario");
}

/// Strengthening forms of the same operations need no password, so the
/// gate above isn't passing because everything asks for one.
#[tokio::test]
async fn strengthening_forms_need_no_password() {
    for (upgraded, change) in [
        (
            false,
            json!({ "op": "set_pack_override", "rule": "MW-1", "action": "drop" }),
        ),
        (
            false,
            json!({ "op": "pack_switch", "id": "SP-4", "on": true }),
        ),
        // SP-4 widens (it was added): accepted without a password while
        // its shadow agrees.
        (
            true,
            json!({ "op": "accept_pack_versions", "rules": ["SP-4"] }),
        ),
    ] {
        let f = fixture(upgraded);
        let c = logged_in(&f).await;
        let p = preview(&f, &c, &change).await;
        assert_eq!(p["weakening"], json!(false), "{change}: {p}");
        let (status, body) = apply(&f, &c, &change, None).await;
        assert_eq!(status, StatusCode::OK, "{change}: {body}");
    }
}
