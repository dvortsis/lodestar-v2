//! F-filter-standin-undo: once a stored row that failed closed reads
//! again, what its stand-in held is judged again by the rules as they are,
//! and only what they wouldn't hold is released
//! (docs/decisions/2026-11-02-F-filter-standin-undo.md).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_api::filters::{Change, FiltersApi};
use lodestar_filter::lists::SharedIps;
use lodestar_filter::load::STAND_INS_KEY;
use lodestar_filter::{Action, Core, CsList, Engine, Handle, HeldSet, Lists, Origin, Sealer};
use lodestar_store::{Filtered, Held, Store, StoreConfig, WriteBatch};

struct Fixture {
    api: FiltersApi,
    dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap();
    let engine = Handle::new(Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        lodestar_filter::Switches::default(),
    )));
    let api = api_on(store, Arc::new(engine), Arc::new(HeldSet::default()));
    Fixture { api, dir }
}

fn api_on(store: Store, engine: Arc<Handle>, held: Arc<HeldSet>) -> FiltersApi {
    FiltersApi {
        store: Arc::new(Mutex::new(store)),
        engine,
        held,
        sealer: Some(Arc::new(Sealer::new(&[7; 32]))),
        ips: Arc::new(SharedIps::default()),
        index: None,
        reenrich: Arc::new(AtomicBool::new(false)),
        jobs: Default::default(),
        quarantine_secs: 30 * 86_400,
        shutdown: Default::default(),
    }
}

/// Loads the engine from the store, as a start does.
fn load(api: &FiltersApi) {
    let (set, class) = {
        let s = api.store.lock().unwrap();
        (s.rule_set().unwrap(), s.class_rules().unwrap())
    };
    api.engine.update_engine(|e| {
        e.load_rules(&set);
        e.load_class_rules(&class);
    });
}

/// The next start on the same file: a new store handle, an engine loaded
/// from it, the held set rebuilt.
fn restart(f: &Fixture) -> FiltersApi {
    let store = Store::open(f.dir.path().join("t.db"), StoreConfig::default()).unwrap();
    let held = HeldSet::default();
    for (h, _) in store.filter_state().unwrap().held {
        held.insert(h);
    }
    let engine = Handle::new(Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        lodestar_filter::Switches::default(),
    )));
    let api = api_on(store, Arc::new(engine), Arc::new(held));
    load(&api);
    api
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

const Q: Held = Held::Quarantine;
const T: Held = Held::Tombstone;

/// Holds `[h; 20]` under `rule` at `at`, as an adult hold when `adult`,
/// with `name` sealed for a quarantine.
fn hold(api: &FiltersApi, (h, rule, held, at, adult, name): (u8, &str, Held, i64, bool, &str)) {
    hold_hash(api, [h; 20], rule, held, at, adult, name);
}

fn hold_hash(
    api: &FiltersApi,
    hash: [u8; 20],
    rule: &str,
    held: Held,
    at: i64,
    adult: bool,
    name: &str,
) {
    let sealed = api.sealer.as_ref().unwrap().seal(name).unwrap();
    api.store
        .lock()
        .unwrap()
        .write(&WriteBatch {
            filtered: &[Filtered {
                hashes: &[hash],
                rule,
                action: Action::Quarantine.code(),
                held: Some(held),
                sealed: (held == Held::Quarantine).then_some(&sealed[..]),
                evidence: &[],
                at,
                expires_at: at + 86_400,
                adult_hold: adult,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    api.held.insert(hash);
}

fn held(api: &FiltersApi, h: u8) -> bool {
    api.store.lock().unwrap().held_among(&[[h; 20]]).unwrap()[0] && api.held.contains(&[h; 20])
}

fn record(api: &FiltersApi) -> Option<String> {
    api.store
        .lock()
        .unwrap()
        .setting_text(STAND_INS_KEY)
        .unwrap()
}

/// Makes the row read again in the store, as an upgrade back would.
fn fix_rule(api: &FiltersApi, id: i64, query: &str, action: Action, unless: Option<&str>) {
    let mut s = api.store.lock().unwrap();
    let mut row = s
        .rule_set()
        .unwrap()
        .rules
        .into_iter()
        .find(|r| r.id == id)
        .unwrap();
    row.query = query.into();
    row.action = action.code();
    row.unless_query = unless.map(Into::into);
    s.update_rule(&row).unwrap();
}

/// A rule's query unreadable, then readable again after a restart: the
/// stand-in's holds that the real rule, the floor or another rule still
/// make stay; the rest are released, and holds from before stay.
#[test]
fn a_rules_stand_in_holds_are_judged_again_when_it_reads() {
    let f = fixture();
    let (rule, other) = {
        let mut s = f.api.store.lock().unwrap();
        (
            s.add_rule("cam size:>>", Action::Quarantine.code(), None, None)
                .unwrap(),
            s.add_rule("othername", Action::Drop.code(), None, None)
                .unwrap(),
        )
    };
    load(&f.api);
    assert_eq!(f.api.undo_stand_ins(now()).unwrap(), None);
    let rec = record(&f.api).unwrap();
    assert!(rec.starts_with(&format!("U-{rule}@")), "{rec}");
    let since: i64 = rec.split('@').nth(1).unwrap().parse().unwrap();
    let u = format!("U-{rule}");
    let at = since + 1;
    hold(&f.api, (1, &u, Q, at, false, "cam rip 2025"));
    hold(&f.api, (2, &u, Q, at, false, "nice movie"));
    hold(&f.api, (3, &u, Q, since - 100, false, "older one"));
    hold(&f.api, (4, &u, Q, at, false, "floor listed"));
    hold(&f.api, (5, &u, Q, at, false, "othername x"));
    hold(&f.api, (6, &u, T, at, true, ""));
    // The floor holds 4 by hash.
    f.api
        .engine
        .update(|c| c.cs.add_hash([4; 20], Origin::Imported));

    // The next start reads the rule again.
    fix_rule(&f.api, rule, "cam", Action::Quarantine, None);
    let api = restart(&f);
    api.engine
        .update(|c| c.cs.add_hash([4; 20], Origin::Imported));
    let done = api.undo_stand_ins(now()).unwrap().unwrap();
    assert_eq!((done.released, done.kept), (2, 3), "{done:?}");
    assert!(held(&api, 1), "the real rule holds it");
    assert!(!held(&api, 2), "only the stand-in held it");
    assert!(held(&api, 3), "before the stand-in: the real rule's");
    assert!(held(&api, 4), "the floor holds it");
    assert!(held(&api, 5), "U-{other} drops it");
    assert!(!held(&api, 6), "its adult tombstone is released by hash");
    assert_eq!(record(&api), None, "done, so it's no longer recorded");
    let notice = lodestar_api::filters::stand_in_notice(&api.store.lock().unwrap(), now());
    assert_eq!(notice.map(|n| (n.released, n.kept)), Some((2, 3)));
    // Nothing more to do on the next run.
    assert_eq!(api.undo_stand_ins(now()).unwrap(), None);
}

/// An unreadable `unless` makes its rule a stand-in that applies without
/// it; once it reads, what the `unless` lets through is released.
#[test]
fn an_unless_stand_in_holds_are_judged_again_when_it_reads() {
    let f = fixture();
    let rule = f
        .api
        .store
        .lock()
        .unwrap()
        .add_rule("spam", Action::Drop.code(), None, Some("size:>>"))
        .unwrap();
    load(&f.api);
    f.api.undo_stand_ins(now()).unwrap();
    let u = format!("U-{rule}");
    let at = now() + 1;
    hold(&f.api, (1, &u, Q, at, false, "spam goodgroup"));
    hold(&f.api, (2, &u, Q, at, false, "spam badgroup"));
    fix_rule(&f.api, rule, "spam", Action::Drop, Some("goodgroup"));
    load(&f.api);
    let done = f.api.undo_stand_ins(now()).unwrap().unwrap();
    assert_eq!((done.released, done.kept), (1, 1));
    assert!(!held(&f.api, 1), "its unless lets it through");
    assert!(held(&f.api, 2));
}

/// An unreadable Is adult classification rule marks everything adult;
/// once it reads, its adult holds (quarantine and tombstones, made before
/// or after the 30-day purge) are judged again, and every stored torrent
/// is queued to be classified again.
#[test]
fn an_is_adult_stand_ins_adult_holds_are_judged_again_when_it_reads() {
    let f = fixture();
    let class = f
        .api
        .store
        .lock()
        .unwrap()
        .add_class_rule("size:>>", true)
        .unwrap();
    load(&f.api);
    f.api.undo_stand_ins(now()).unwrap();
    assert!(record(&f.api).unwrap().starts_with(&format!("C-{class}@")));
    let at = now() + 1_000;
    hold(&f.api, (1, "CS-5a", Q, at, true, "some name"));
    hold(&f.api, (2, "CS-5a", T, at, true, ""));
    hold(&f.api, (3, "CS-5a", T, at - 86_400, true, ""));
    hold(&f.api, (4, "CS-5b", T, at, true, ""));
    hold(&f.api, (5, "CS-5a", Q, at, false, "not adult-held"));
    // The 30-day purge turns the adult quarantine 6 into a tombstone.
    hold(&f.api, (6, "CS-5a", Q, at - 500, true, "x"));
    f.api
        .store
        .lock()
        .unwrap()
        .purge_filter(at - 500 + 86_400, 0)
        .unwrap();
    assert!(held(&f.api, 1) && held(&f.api, 5), "not expired");
    assert!(held(&f.api, 6), "a tombstone now");
    {
        let s = f.api.store.lock().unwrap();
        s.connection()
            .execute("UPDATE class_rules SET query = 'studiox'", [])
            .unwrap();
    }
    f.api
        .engine
        .update(|c| c.cs.add_hash([4; 20], Origin::Imported));
    load(&f.api);
    assert!(f.api.engine.current().unreadable.is_empty());
    let done = f.api.undo_stand_ins(now()).unwrap().unwrap();
    assert!(!held(&f.api, 1));
    assert!(!held(&f.api, 2), "its tombstone is released");
    assert!(held(&f.api, 3), "made before the rule was unreadable");
    assert!(held(&f.api, 4), "the floor still refuses it");
    assert!(held(&f.api, 5), "not an adult hold");
    assert!(!held(&f.api, 6), "a purged adult hold is released too");
    assert_eq!((done.released, done.kept), (3, 1));
    let requeue = f
        .api
        .store
        .lock()
        .unwrap()
        .setting_text("classify.requeue_all")
        .unwrap();
    assert_eq!(
        requeue.as_deref(),
        Some("1"),
        "classified again at search's start"
    );
    assert_eq!(record(&f.api), None);
}

/// Removing an unreadable Is adult rule through the API also re-judges
/// its adult holds: before, they became tombstones for good.
#[test]
fn removing_an_unreadable_is_adult_rule_releases_its_adult_holds() {
    let f = fixture();
    let class = f
        .api
        .store
        .lock()
        .unwrap()
        .add_class_rule("size:>>", true)
        .unwrap();
    load(&f.api);
    f.api.undo_stand_ins(now()).unwrap();
    hold(&f.api, (1, "CS-5a", T, now() + 1, true, ""));
    f.api.remove_class_rule(class, now()).unwrap();
    assert!(!held(&f.api, 1));
    assert_eq!(record(&f.api), None);
}

/// A row that still fails closed keeps its holds and its first time, and
/// a stop part-way leaves the entry recorded for the next start.
#[test]
fn a_row_still_unreadable_keeps_its_holds_and_a_stop_redoes_it() {
    let f = fixture();
    let rule = f
        .api
        .store
        .lock()
        .unwrap()
        .add_rule("size:>>", Action::Hide.code(), None, None)
        .unwrap();
    load(&f.api);
    f.api.undo_stand_ins(now()).unwrap();
    let first = record(&f.api).unwrap();
    hold(&f.api, (1, &format!("U-{rule}"), Q, now() + 1, false, "a"));
    let api = restart(&f);
    assert_eq!(api.undo_stand_ins(now() + 10).unwrap(), None);
    assert_eq!(record(&api).unwrap(), first, "its first time is kept");
    assert!(held(&api, 1));
    fix_rule(&api, rule, "cam", Action::Hide, None);
    load(&api);
    api.shutdown.cancel();
    assert!(api.undo_stand_ins(now()).is_err(), "the shutdown stops it");
    assert_eq!(record(&api).unwrap(), first, "still to do");
    assert!(held(&api, 1));
    let api = restart(&f);
    let done = api.undo_stand_ins(now()).unwrap().unwrap();
    assert_eq!(done.released, 1);
    assert!(!held(&api, 1));
}

/// Changes applied while a re-judge runs: each re-judge chunk sees one
/// engine, the hold a new rule makes stays, and nothing deadlocks.
#[test]
fn a_rejudge_runs_beside_apply() {
    let f = fixture();
    let rule = f
        .api
        .store
        .lock()
        .unwrap()
        .add_rule("size:>>", Action::Quarantine.code(), None, None)
        .unwrap();
    load(&f.api);
    f.api.undo_stand_ins(now()).unwrap();
    let u = format!("U-{rule}");
    let at = now() + 1;
    for i in 0..2_000u32 {
        let mut h = [0u8; 20];
        h[..4].copy_from_slice(&i.to_be_bytes());
        h[19] = 9;
        let name = if i % 2 == 0 { "keepword x" } else { "free y" };
        hold_hash(&f.api, h, &u, Q, at, false, name);
    }
    fix_rule(&f.api, rule, "keepword", Action::Quarantine, None);
    load(&f.api);
    let api = Arc::new(f.api);
    let (tx, rx) = std::sync::mpsc::channel();
    let changer = {
        let api = api.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            for i in 0..20 {
                let change = Change::AddRule {
                    query: format!("flagword{i}"),
                    action: "flag".into(),
                    shadow_days: None,
                    unless: None,
                };
                let a = api.assess(&change, now()).unwrap();
                api.apply(&change, a, now()).unwrap();
            }
            tx.send(()).unwrap();
        })
    };
    let judge = {
        let api = api.clone();
        std::thread::spawn(move || {
            let _ = api.undo_stand_ins(now()).unwrap();
            tx.send(()).unwrap();
        })
    };
    for _ in 0..2 {
        rx.recv_timeout(Duration::from_secs(60))
            .expect("apply and the re-judge both finish");
    }
    changer.join().unwrap();
    judge.join().unwrap();
    let mut kept = 0;
    let s = api.store.lock().unwrap();
    for i in 0..2_000u32 {
        let mut h = [0u8; 20];
        h[..4].copy_from_slice(&i.to_be_bytes());
        h[19] = 9;
        let is_held = s.held_among(&[h]).unwrap()[0];
        assert_eq!(is_held, i % 2 == 0, "hash {i}");
        kept += usize::from(is_held);
    }
    assert_eq!(kept, 1_000);
    assert_eq!(s.setting_text(STAND_INS_KEY).unwrap(), None);
}
