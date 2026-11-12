//! GS-1: `/torrents/{hash}/siblings`' identical-payload clones and the
//! same file elsewhere, through the router: found in the index, confirmed
//! by the stored files, withheld as other versions are (B13), partial
//! while the index backfills, and bounded.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_search::{Generations, SearchIndex};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
/// `compare::MAX_SIBLINGS` and `compare::SAME_FILE_ROOTS` (the module is
/// private).
const MAX_SIBLINGS: usize = 20;
const SAME_FILE_ROOTS: usize = 64;

/// How a fixture torrent's info dict is laid out.
#[derive(Clone, Copy, PartialEq)]
enum Layout {
    /// v1, a `files` list (or `length` for one file named "").
    V1,
    /// Pure v2: a `file tree` of single-segment names, each with its
    /// root.
    V2,
    /// One file, v1 and v2 (a hybrid).
    Hybrid,
}

/// A fixture torrent: its name, files as (path, length, v2 root byte),
/// layout, whether it's corrected to Adult, and its extra flags.
struct T {
    name: String,
    files: Vec<(String, i64, u8)>,
    layout: Layout,
    adult: bool,
    flags: i64,
}

fn v1(name: &str, files: &[(&str, i64)]) -> T {
    T {
        name: name.to_owned(),
        files: files.iter().map(|(p, l)| (p.to_string(), *l, 0)).collect(),
        layout: Layout::V1,
        adult: false,
        flags: 0,
    }
}

fn v2(name: &str, files: &[(&str, i64, u8)]) -> T {
    T {
        name: name.to_owned(),
        files: files
            .iter()
            .map(|(p, l, r)| (p.to_string(), *l, *r))
            .collect(),
        layout: Layout::V2,
        adult: false,
        flags: 0,
    }
}

fn hybrid(name: &str, len: i64, root: u8) -> T {
    T {
        name: name.to_owned(),
        files: vec![(name.to_owned(), len, root)],
        layout: Layout::Hybrid,
        adult: false,
        flags: 0,
    }
}

fn d(e: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        e.into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn b(s: &[u8]) -> OwnedValue {
    OwnedValue::Bytes(s.to_vec())
}

fn metainfo(t: &T) -> Vec<u8> {
    let name = t.name.as_bytes();
    let leaf = |len: i64, root: u8| {
        d(vec![(
            "",
            d(vec![
                ("length", OwnedValue::Int(len)),
                ("pieces root", b(&[root; 32])),
            ]),
        )])
    };
    let tree = |files: &[(String, i64, u8)]| {
        OwnedValue::Dict(
            files
                .iter()
                .map(|(p, l, r)| (p.as_bytes().to_vec(), leaf(*l, *r)))
                .collect(),
        )
    };
    match t.layout {
        Layout::V1 => {
            let mut entries = vec![
                ("name", b(name)),
                ("piece length", OwnedValue::Int(1 << 16)),
                ("pieces", b(&[0; 20])),
            ];
            if let [(single, len, _)] = t.files.as_slice()
                && single.is_empty()
            {
                entries.push(("length", OwnedValue::Int(*len)));
            } else {
                let list = t
                    .files
                    .iter()
                    .map(|(p, len, _)| {
                        d(vec![
                            ("length", OwnedValue::Int(*len)),
                            (
                                "path",
                                OwnedValue::List(p.split('/').map(|s| b(s.as_bytes())).collect()),
                            ),
                        ])
                    })
                    .collect();
                entries.push(("files", OwnedValue::List(list)));
            }
            to_vec(&d(entries))
        }
        Layout::V2 => to_vec(&d(vec![
            ("file tree", tree(&t.files)),
            ("meta version", OwnedValue::Int(2)),
            ("name", b(name)),
            ("piece length", OwnedValue::Int(16_384)),
        ])),
        Layout::Hybrid => {
            let (_, len, _) = &t.files[0];
            to_vec(&d(vec![
                ("file tree", tree(&t.files)),
                ("length", OwnedValue::Int(*len)),
                ("meta version", OwnedValue::Int(2)),
                ("name", b(name)),
                ("piece length", OwnedValue::Int(16_384)),
                ("pieces", b(&[0; 20])),
            ]))
        }
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    hashes: Vec<String>,
    _dir: tempfile::TempDir,
}

/// The torrents stored, classified and indexed. `adult`: "Show adult
/// content" on. `generations`: a build shipping a new generation of a
/// field, so the index is backfilling it.
fn fixture(torrents: &[T], adult: bool, generations: Option<Generations>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = torrents.iter().map(metainfo).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(torrents)
        .enumerate()
        .map(|(i, ((info, raw), t))| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000 + i as i64,
            metadata_at: 1_700_000_001 + i as i64,
            extra_flags: t.flags,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    let ids: Vec<i64> = store
        .insert(&batch)
        .unwrap()
        .into_iter()
        .map(|i| i.id())
        .collect();
    let mut enriched = Vec::new();
    for (id, t) in ids.iter().zip(torrents) {
        let row = store.torrent(*id).unwrap().unwrap();
        let tree = store.files(*id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let correction = Correction {
            adult: t.adult.then_some(true),
            not_junk: false,
        };
        let (e, _) = lodestar_filter::reclassify::enrich_corrected(&row.name, &files, correction);
        enriched.push((*id, e));
    }
    store.set_enrichments(&enriched).unwrap();
    let index_dir = lodestar_search::index_dir(dir.path());
    {
        let index = SearchIndex::open(&index_dir).unwrap();
        let mut w = index.writer(15 << 20).unwrap();
        while w.catch_up(&mut store, 1000).unwrap() > 0 {}
        w.commit().unwrap();
        w.finish().unwrap();
    }
    let mut index = SearchIndex::open(&index_dir).unwrap();
    if let Some(g) = generations {
        index = index.with_generations(g).unwrap();
    }
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(index.into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            user_data: None,
            adult: AdultVisibility::follow(move || adult),
            insights: None,
            hot: None,
            events: None,
        }),
    );
    let hashes = infos.iter().map(|i| i.primary_hash().to_string()).collect();
    Fixture {
        app,
        auth,
        hashes,
        _dir: dir,
    }
}

fn plain(torrents: &[T]) -> Fixture {
    fixture(torrents, false, None)
}

async fn json_of(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "token": token, "password": PASSWORD, "notice_version": lodestar_api::legal::NOTICE_VERSION })
                .to_string(),
        ))
        .unwrap();
    let resp = f.app.clone().oneshot(req).await.unwrap();
    resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn siblings(f: &Fixture, c: &str, i: usize) -> Value {
    let (status, body) = json_of(
        &f.app,
        get(&format!("/api/v1/torrents/{}/siblings", f.hashes[i]), c),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn names(group: &Value) -> Vec<String> {
    let mut v: Vec<String> = group["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap().to_owned())
        .collect();
    v.sort();
    v
}

fn reason(group: &Value) -> (&str, &str) {
    (
        group["status"].as_str().unwrap(),
        group["reason"].as_str().unwrap_or(""),
    )
}

/// Payload A (two files, both of distinctive size: specific under CS-8a's
/// rule) under several names and orders, a different payload, and clones
/// a filter rule hid or that are Adult.
fn clones() -> Vec<T> {
    let mut hidden = v1(
        "Clone.Hidden",
        &[("x/a.mkv", 7_000_001), ("x/b.srt", 70_003)],
    );
    hidden.flags = flags::HIDDEN;
    let mut adult = v1(
        "Clone.Adult",
        &[("y/a.mkv", 7_000_001), ("y/b.srt", 70_003)],
    );
    adult.adult = true;
    vec![
        v1("Payload.A", &[("a.mkv", 7_000_001), ("b.srt", 70_003)]),
        v1(
            "Renamed.Copy",
            &[("subs.srt", 70_003), ("movie.mkv", 7_000_001)],
        ),
        v1("Other.Payload", &[("a.mkv", 7_000_001), ("b.srt", 70_004)]),
        hidden,
        adult,
        v1("Single.File", &[("", 7_000_001)]),
    ]
}

#[tokio::test]
async fn identical_payloads_are_found_and_withheld_as_other_versions_are() {
    let f = plain(&clones());
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(names(&body["identical_payload"]), ["Renamed.Copy"]);
    assert_eq!(body["identical_payload"]["status"], "available");
    assert_eq!(body["identical_payload_partial"], false);
    // A v1 torrent has no piece roots: said, never "none".
    assert_eq!(
        reason(&body["same_file"]),
        ("unavailable", "needs_v2_files")
    );
    assert_eq!(body["same_file"]["hits"], json!([]));
    // A torrent a filter rule hid opens in the drawer: its clones answer,
    // and still never list a hidden or (with Adult off) Adult one.
    let body = siblings(&f, &c, 3).await;
    assert_eq!(
        names(&body["identical_payload"]),
        ["Payload.A", "Renamed.Copy"]
    );
    // Adult shown: the Adult clone too, never the hidden one.
    let f = fixture(&clones(), true, None);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(
        names(&body["identical_payload"]),
        ["Clone.Adult", "Renamed.Copy"]
    );
    // With Adult off, an Adult torrent is unknown.
    let f = plain(&clones());
    let c = session(&f).await;
    let (status, body) = json_of(
        &f.app,
        get(&format!("/api/v1/torrents/{}/siblings", f.hashes[4]), &c),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.to_string().contains("Clone"), "{body}");
}

/// v2 and hybrid torrents sharing a file (its piece root) are found under
/// any name; another root isn't.
fn shared() -> Vec<T> {
    let mut adult = v2("Adult.Share", &[("p.bin", 500, 1)]);
    adult.adult = true;
    let mut hidden = v2("Hidden.Share", &[("q.bin", 500, 1)]);
    hidden.flags = flags::HIDDEN;
    vec![
        v2("Pack.V2", &[("a.bin", 500, 1), ("b.bin", 300, 2)]),
        v2("Other.Pack", &[("renamed.bin", 500, 1), ("c.bin", 50, 3)]),
        hybrid("Hybrid.One", 300, 2),
        v2("Unrelated", &[("z.bin", 500, 9)]),
        adult,
        hidden,
    ]
}

#[tokio::test]
async fn the_same_file_is_found_in_v2_and_hybrid_torrents() {
    let f = plain(&shared());
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(names(&body["same_file"]), ["Hybrid.One", "Other.Pack"]);
    assert_eq!(body["same_file"]["status"], "available");
    assert_eq!(body["same_file_partial"], false);
    assert_eq!(body["same_file_capped"], false);
    // A hybrid finds the v2 torrent with its file.
    let body = siblings(&f, &c, 2).await;
    assert_eq!(names(&body["same_file"]), ["Pack.V2"]);
    // Adult shown: the Adult one too; hidden never.
    let f = fixture(&shared(), true, None);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(
        names(&body["same_file"]),
        ["Adult.Share", "Hybrid.One", "Other.Pack"]
    );
}

/// While the index backfills a field, its group says it may be
/// incomplete, never that there's none.
#[tokio::test]
async fn a_backfilling_index_marks_the_groups_partial() {
    let g = Generations::current();
    let sig = g.get("payload_sig").unwrap() + 1;
    let root = g.get("file.pieces_root").unwrap() + 1;
    let gens = g.with("payload_sig", sig).with("file.pieces_root", root);
    // Payload A's torrents (specific, v1), then the v2 ones (index 6).
    let both = || clones().into_iter().chain(shared()).collect::<Vec<T>>();
    let f = fixture(&both(), false, Some(gens));
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(body["identical_payload_partial"], true);
    assert_eq!(body["identical_payload"]["status"], "available");
    let body = siblings(&f, &c, 6).await;
    assert_eq!(body["same_file_partial"], true);
    assert_eq!(body["same_file"]["status"], "available");
    // Complete again once nothing backfills.
    let f = plain(&both());
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(body["identical_payload_partial"], false);
    assert_eq!(names(&body["identical_payload"]), ["Renamed.Copy"]);
    let body = siblings(&f, &c, 6).await;
    assert_eq!(body["same_file_partial"], false);
}

/// More clones than are listed: the cap and `truncated`. A torrent with
/// more files than are looked up: `same_file_capped`.
#[tokio::test]
async fn the_groups_are_bounded() {
    let mut torrents: Vec<T> = (0..25)
        .map(|i| v1(&format!("Many.{i}"), &[("", 60_000_001)]))
        .collect();
    let files: Vec<(String, i64, u8)> = (0..SAME_FILE_ROOTS + 3)
        .map(|i| (format!("f{i}.bin"), 1000 + i as i64, i as u8))
        .collect();
    let refs: Vec<(&str, i64, u8)> = files.iter().map(|(p, l, r)| (p.as_str(), *l, *r)).collect();
    torrents.push(v2("Big.Pack", &refs));
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    let listed = body["identical_payload"]["hits"].as_array().unwrap().len();
    assert_eq!(listed, MAX_SIBLINGS);
    assert_eq!(body["identical_payload"]["truncated"], true);
    let body = siblings(&f, &c, 25).await;
    assert_eq!(body["same_file_capped"], true);
    assert_eq!(body["same_file"]["status"], "available");
}

/// CS-8a's specificity rule (the runner's refinement of §6): a payload
/// small or round enough for unrelated torrents to share by chance isn't
/// matched, whatever shares its sizes; a specific one still is.
#[tokio::test]
async fn a_payload_that_isnt_specific_is_never_matched() {
    let torrents = vec![
        v1("Small.One", &[("", 1 << 20)]),
        v1("Small.Two", &[("", 1 << 20)]),
        v1("Round.One", &[("a.bin", 4 << 20), ("b.bin", 8 << 20)]),
        v1("Round.Two", &[("c.bin", 4 << 20), ("d.bin", 8 << 20)]),
        v1("Big.One", &[("", 60_000_001)]),
        v1("Big.Two", &[("", 60_000_001)]),
    ];
    let f = plain(&torrents);
    let c = session(&f).await;
    for i in [0, 2] {
        let body = siblings(&f, &c, i).await;
        assert_eq!(
            reason(&body["identical_payload"]),
            ("unavailable", "payload_not_specific"),
            "{i}"
        );
        assert_eq!(body["identical_payload"]["hits"], json!([]));
    }
    // 50 MB or more is specific on its own.
    let body = siblings(&f, &c, 4).await;
    assert_eq!(names(&body["identical_payload"]), ["Big.Two"]);
}
