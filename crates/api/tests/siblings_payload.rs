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
                        let mut e = vec![
                            ("length", OwnedValue::Int(*len)),
                            (
                                "path",
                                OwnedValue::List(p.split('/').map(|s| b(s.as_bytes())).collect()),
                            ),
                        ];
                        // BEP 47 padding, by its conventional folder.
                        if p.starts_with(".pad/") {
                            e.push(("attr", b(b"p")));
                        }
                        d(e)
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
    build(torrents, adult, generations, true)
}

/// [`fixture`], served with search on or off (`indexed`).
fn build(torrents: &[T], adult: bool, generations: Option<Generations>, indexed: bool) -> Fixture {
    build_with(torrents, adult, generations, indexed, None)
}

/// Fields an older build's schemas didn't have: (torrent index, file
/// index).
type Older<'a> = (&'a [&'a str], &'a [&'a str]);

/// [`build`], the index then rewritten as an older build left it when
/// `older` is given: an older version's directory, its schemas without
/// those fields, every document with only the fields they keep, and the
/// same commit payload. It's served as an older index is during a
/// rebuild (`Serving`), so those fields are `Missing`; `generations` is
/// then unused.
fn build_with(
    torrents: &[T],
    adult: bool,
    generations: Option<Generations>,
    indexed: bool,
    older: Option<Older<'_>>,
) -> Fixture {
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
    let handle: lodestar_search::SearchHandle = match older {
        // Served as during a rebuild: the older index, while the rebuild
        // of this build's waits to start (until the fixture drops).
        Some(older) => {
            let old = lodestar_search::IndexVersion {
                schema: lodestar_search::IndexVersion::CURRENT.schema - 1,
                ..lodestar_search::IndexVersion::CURRENT
            };
            rewrite_older(
                &index_dir,
                &dir.path().join(old.dir_name()),
                &mut store,
                older,
            );
            std::fs::remove_dir_all(&index_dir).unwrap();
            let config = lodestar_search::ServingConfig {
                hold_before_start: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
                ..lodestar_search::ServingConfig::default()
            };
            let serving =
                lodestar_search::Serving::open_or_rebuild(dir.path(), &db, config).unwrap();
            lodestar_search::SearchHandle::serving(Arc::new(serving))
        }
        None => {
            let mut index = SearchIndex::open(&index_dir).unwrap();
            if let Some(g) = generations {
                index = index.with_generations(g).unwrap();
            }
            index.into()
        }
    };
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: indexed.then_some(handle),
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

/// Writes the index under `from` again at `dir`, without `older`'s
/// fields (see [`build_with`]), as search's own rebuild tests build a v5
/// index.
fn rewrite_older(
    from: &std::path::Path,
    dir: &std::path::Path,
    store: &mut Store,
    older: Older<'_>,
) {
    use lodestar_search::tokenizer::{NAME_TOKENIZER, NameTokenizer};
    use tantivy::TantivyDocument;
    use tantivy::schema::Schema;
    let without = |current: &Schema, drop: &[&str]| {
        let mut b = Schema::builder();
        for (_, entry) in current.fields() {
            if !drop.contains(&entry.name()) {
                b.add_field(entry.clone());
            }
        }
        b.build()
    };
    let payload = |d: &std::path::Path| {
        tantivy::Index::open_in_dir(d)
            .unwrap()
            .load_metas()
            .unwrap()
            .payload
            .unwrap()
    };
    let files_dir = dir.join("files");
    let (torrent_payload, file_payload) = (payload(from), payload(&from.join("files")));
    std::fs::create_dir_all(&files_dir).unwrap();
    let (schema, fields) = lodestar_search::schema::build();
    let (file_schema, file_fields) = lodestar_search::files::schema();
    let old = without(&schema, older.0);
    let old_files = without(&file_schema, older.1);
    let index = tantivy::Index::create_in_dir(dir, old.clone()).unwrap();
    let files = tantivy::Index::builder()
        .schema(old_files.clone())
        .settings(lodestar_search::files::settings())
        .create_in_dir(&files_dir)
        .unwrap();
    let map = |from: &Schema, to: &Schema, d: TantivyDocument| {
        let mut out = TantivyDocument::new();
        for (f, v) in d.field_values() {
            if let Ok(g) = to.get_field(from.get_field_name(f)) {
                out.add_field_value(g, v);
            }
        }
        out
    };
    let writer = |i: &tantivy::Index| {
        i.tokenizers()
            .register(NAME_TOKENIZER, NameTokenizer::default());
        i.tokenizers().register(
            lodestar_search::schema::TRIGRAM_TOKENIZER,
            lodestar_search::schema::trigram_analyzer(),
        );
        i.writer::<TantivyDocument>(15 << 20).unwrap()
    };
    let (mut w, mut fw) = (writer(&index), writer(&files));
    for id in store.ids_after(0, 100_000).unwrap() {
        let row = store.torrent(id).unwrap().unwrap();
        let tree = store.files(id).unwrap().unwrap();
        let doc = lodestar_search::TorrentDoc::new(&row, &tree).to_document(&fields);
        w.add_document(map(&schema, &old, doc)).unwrap();
        for d in lodestar_search::files::file_docs(id as u64, &tree) {
            fw.add_document(map(&file_schema, &old_files, d.to_document(&file_fields)))
                .unwrap();
        }
    }
    for (w, p) in [(&mut fw, &file_payload), (&mut w, &torrent_payload)] {
        let mut prepared = w.prepare_commit().unwrap();
        prepared.set_payload(p);
        prepared.commit().unwrap();
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
    let mut adult = v2("Adult.Share", &[("p.bin", A_BYTES, 1)]);
    adult.adult = true;
    let mut hidden = v2("Hidden.Share", &[("q.bin", A_BYTES, 1)]);
    hidden.flags = flags::HIDDEN;
    vec![
        v2("Pack.V2", &[("a.bin", A_BYTES, 1), ("b.bin", B_BYTES, 2)]),
        v2(
            "Other.Pack",
            &[("renamed.bin", A_BYTES, 1), ("c.bin", 50, 3)],
        ),
        hybrid("Hybrid.One", B_BYTES, 2),
        v2("Unrelated", &[("z.bin", A_BYTES, 9)]),
        adult,
        hidden,
        // Root 1, but another size: a claim that isn't the same file.
        v2("Wrong.Size", &[("w.bin", A_BYTES + 1, 1)]),
    ]
}

/// Sizes of the shared files: past the 1 MiB floor.
const A_BYTES: i64 = 3 << 20;
const B_BYTES: i64 = (2 << 20) + 7;

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
    // A pack with more files than are looked up, and 25 others sharing
    // its largest file: the visible list fills, so capped is said.
    let mb = 1i64 << 20;
    let files: Vec<(String, i64, u8)> = (0..SAME_FILE_ROOTS + 3)
        .map(|i| (format!("f{i}.bin"), mb + i as i64, i as u8))
        .collect();
    let refs: Vec<(&str, i64, u8)> = files.iter().map(|(p, l, r)| (p.as_str(), *l, *r)).collect();
    torrents.push(v2("Big.Pack", &refs));
    let top = SAME_FILE_ROOTS + 2;
    for i in 0..25 {
        torrents.push(v2(
            &format!("Sharer.{i}"),
            &[("x.bin", mb + top as i64, top as u8)],
        ));
    }
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    let listed = body["identical_payload"]["hits"].as_array().unwrap().len();
    assert_eq!(listed, MAX_SIBLINGS);
    assert_eq!(body["identical_payload"]["truncated"], true);
    let body = siblings(&f, &c, 25).await;
    assert_eq!(
        body["same_file"]["hits"].as_array().unwrap().len(),
        MAX_SIBLINGS
    );
    assert_eq!(body["same_file_capped"], true);
    // A sharer finds only the pack (its one file): a short list, never
    // capped, whatever the pack's size.
    let body = siblings(&f, &c, 26).await;
    assert_eq!(body["same_file_capped"], false);
}

/// B13: withheld torrents never fill the same-file window or show in its
/// flags. More hidden and Adult torrents than the window holds share the
/// file; the response is exactly what it is with none of them.
#[tokio::test]
async fn withheld_sharers_leave_no_trace() {
    // The visible match is older than every withheld one, so a window
    // taken before visibility would hold only withheld torrents.
    let mut torrents = vec![
        v2("Visible.Ref", &[("a.bin", A_BYTES, 1)]),
        v2("Visible.Old", &[("b.bin", A_BYTES, 1)]),
    ];
    for i in 0..1_010 {
        let mut t = v2(&format!("Withheld.{i}"), &[("a.bin", A_BYTES, 1)]);
        if i % 2 == 0 {
            t.flags = flags::HIDDEN;
        } else {
            t.adult = true;
        }
        torrents.push(t);
    }
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(names(&body["same_file"]), ["Visible.Old"]);
    assert_eq!(body["same_file_capped"], false);
    assert_eq!(body["same_file"]["truncated"], false);
    assert!(!body.to_string().contains("Withheld"), "{body}");
}

/// Only files of at least 1 MiB are looked up, and on (size, root): a
/// small shared file lists nothing, and a root claimed with another size
/// isn't the same file.
#[tokio::test]
async fn small_files_and_mismatched_sizes_are_not_the_same_file() {
    let torrents = vec![
        v2("Small.Note", &[("nfo.txt", 900, 5)]),
        v2("Also.Note", &[("nfo.txt", 900, 5)]),
    ];
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(
        reason(&body["same_file"]),
        ("unavailable", "payload_not_specific")
    );
    let f = plain(&shared());
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert!(!names(&body["same_file"]).contains(&"Wrong.Size".to_owned()));
}

/// An import that kept only part of a file list: its sizes aren't its
/// payload, so it gets a reason, and it's never listed as another's clone.
#[tokio::test]
async fn a_partial_file_list_is_never_a_clone() {
    let mut torrents = clones();
    let mut partial = v1("Partial.Import", &[("a.mkv", 7_000_001), ("b.srt", 70_003)]);
    partial.flags = flags::FILES_PARTIAL;
    torrents.push(partial);
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(names(&body["identical_payload"]), ["Renamed.Copy"]);
    let last = torrents.len() - 1;
    let body = siblings(&f, &c, last).await;
    assert_eq!(
        reason(&body["identical_payload"]),
        ("unavailable", "partial_file_list")
    );
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

/// Requirement 2: a torrent with only padding files has no signature and
/// no roots, so both groups say so, and it never lists the other
/// signature-less torrents.
#[tokio::test]
async fn a_torrent_with_only_padding_files_matches_nothing() {
    let torrents = vec![
        v1("Pad.Only.One", &[(".pad/16384", 16_384)]),
        v1("Pad.Only.Two", &[(".pad/16384", 16_384)]),
        v1("Payload.A", &[("a.mkv", 7_000_001), ("b.srt", 70_003)]),
    ];
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    for group in ["identical_payload", "same_file"] {
        assert_eq!(
            reason(&body[group]),
            ("unavailable", "no_content_files"),
            "{group}"
        );
        assert_eq!(body[group]["hits"], json!([]), "{group}");
    }
}

/// With search off, both groups say so (a v1 clone for identical
/// payload, a v2 sharer for the same file), and claim nothing partial or
/// capped.
#[tokio::test]
async fn without_search_the_groups_say_search_is_disabled() {
    let both = clones().into_iter().chain(shared()).collect::<Vec<T>>();
    let f = build(&both, false, None, false);
    let c = session(&f).await;
    for (i, group) in [(0, "identical_payload"), (6, "same_file")] {
        let body = siblings(&f, &c, i).await;
        assert_eq!(
            reason(&body[group]),
            ("unavailable", "search_disabled"),
            "{group}"
        );
        assert_eq!(body["identical_payload_partial"], false);
        assert_eq!(body["same_file_partial"], false);
        assert_eq!(body["same_file_capped"], false);
    }
}

// ---- similar payloads (GS-2) ----------------------------------------------

const GIB: i64 = 1 << 30;

/// Episode `e` of the reference season: about 1 GiB, a distinctive size.
fn episode(e: i64) -> i64 {
    GIB + 1_001 * e + 17
}

/// A season torrent of `episodes` (sizes) plus a small `.nfo`, which is
/// never a major file.
fn season(name: &str, episodes: &[i64]) -> T {
    let mut files: Vec<(String, i64)> = episodes
        .iter()
        .enumerate()
        .map(|(i, &len)| (format!("s/e{i:02}.mkv"), len))
        .collect();
    files.push(("s/info.nfo".to_owned(), 4_000));
    T {
        name: name.to_owned(),
        files: files.into_iter().map(|(p, l)| (p, l, 0)).collect(),
        layout: Layout::V1,
        adult: false,
        flags: 0,
    }
}

fn eps(range: std::ops::Range<i64>) -> Vec<i64> {
    range.map(episode).collect()
}

/// Whether two seasons' major sizes share a band term: what puts one in
/// the other's candidates.
fn share_a_band(a: &[i64], b: &[i64]) -> bool {
    let terms = |s: &[i64]| {
        let sizes: Vec<u64> = s.iter().map(|&x| x as u64).collect();
        lodestar_classify::group::band_terms(&sizes)
    };
    let (a, b) = (terms(a), terms(b));
    a.iter().any(|t| b.contains(t))
}

/// The reference (10 episodes), its relatives and decoys.
fn seasons() -> Vec<T> {
    let mut plus_one = eps(0..10);
    plus_one.push(episode(10));
    let mut swapped = eps(0..9);
    swapped.push(2 * GIB + 3);
    let mut hidden = season("Season.Hidden", &plus_one);
    hidden.flags = flags::HIDDEN;
    let mut adult = season("Season.Adult", &plus_one);
    adult.adult = true;
    let mut partial = season("Season.Partial", &plus_one);
    partial.flags = flags::FILES_PARTIAL;
    let mut far = eps(0..5);
    far.extend((0..5).map(|i| 3 * GIB + i));
    vec![
        season("Season.Ref", &eps(0..10)),
        // 10 of 11 major sizes shared: similar.
        season("Season.PlusOne", &plus_one),
        // 9 of 11: similar.
        season("Season.Swapped", &swapped),
        // The same sizes: an identical clone, listed there instead.
        season("Season.Clone", &eps(0..10)),
        // 5 of 15: not similar.
        season("Season.Far", &far),
        hidden,
        adult,
        partial,
        // Two large files: no signature.
        season("Film.Two", &[4 * GIB + 1, 3 * GIB + 5]),
        season("Film.Two.Copy", &[4 * GIB + 1, 3 * GIB + 5]),
    ]
}

#[tokio::test]
async fn similar_payloads_are_found_confirmed_and_withheld() {
    // The fixture relies on these sharing a band (fixed hashes: the
    // MinHash's choice, checked here so a change to it is caught).
    let mut swapped = eps(0..9);
    swapped.push(2 * GIB + 3);
    assert!(share_a_band(&eps(0..10), &swapped));
    let f = plain(&seasons());
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(body["similar_payload"]["status"], "available");
    assert_eq!(
        names(&body["similar_payload"]),
        ["Season.PlusOne", "Season.Swapped"]
    );
    assert_eq!(body["similar_payload_partial"], false);
    assert_eq!(body["similar_payload"]["truncated"], false);
    // The clone is an identical payload, not a similar one.
    assert_eq!(names(&body["identical_payload"]), ["Season.Clone"]);
    // Adult shown: the Adult one too; hidden and partial never.
    let f = fixture(&seasons(), true, None);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(
        names(&body["similar_payload"]),
        ["Season.Adult", "Season.PlusOne", "Season.Swapped"]
    );
    // A hidden torrent opened in the drawer answers, and lists no
    // hidden one.
    let body = siblings(&f, &c, 5).await;
    let got = names(&body["similar_payload"]);
    assert!(got.contains(&"Season.Ref".to_owned()), "{got:?}");
    assert!(
        !got.iter()
            .any(|n| n == "Season.Hidden" || n == "Season.Partial")
    );
}

/// Fewer than 3 major files: no signature, a reason, and never a match
/// with the other signature-less torrents.
#[tokio::test]
async fn too_few_major_files_say_so_and_match_nothing() {
    let f = plain(&seasons());
    let c = session(&f).await;
    let body = siblings(&f, &c, 8).await;
    assert_eq!(
        reason(&body["similar_payload"]),
        ("unavailable", "few_major_files")
    );
    assert_eq!(body["similar_payload"]["hits"], json!([]));
    // Its identical copy is still found there.
    assert_eq!(names(&body["identical_payload"]), ["Film.Two.Copy"]);
    // The other groups' torrent-level reasons apply first.
    let torrents = vec![
        v1("Pad.Only", &[(".pad/16384", 16_384)]),
        v1("Small.One", &[("", 1 << 20)]),
    ];
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(
        reason(&body["similar_payload"]),
        ("unavailable", "no_content_files")
    );
    let body = siblings(&f, &c, 1).await;
    assert_eq!(
        reason(&body["similar_payload"]),
        ("unavailable", "payload_not_specific")
    );
}

/// A candidate the bands find whose stored sizes aren't similar enough
/// (an estimate-only match: a band shared at Jaccard 8/12) is dropped.
#[tokio::test]
async fn a_band_match_that_fails_the_stored_sizes_is_dropped() {
    let reference = eps(0..10);
    // 8 shared, 2 of its own: J = 8 / 12. Pick the two so a band is
    // shared (deterministic: the first such pair).
    let near = (0..10_000i64)
        .map(|k| {
            let mut s = eps(0..8);
            s.extend([2 * GIB + 2 * k, 2 * GIB + 2 * k + 1]);
            s
        })
        .find(|s| share_a_band(&reference, s))
        .expect("a pair sharing a band");
    let torrents = vec![
        season("Season.Ref", &reference),
        season("Season.Near", &near),
    ];
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(body["similar_payload"]["status"], "available");
    assert_eq!(body["similar_payload"]["hits"], json!([]));
}

#[tokio::test]
async fn similar_payloads_are_capped_and_partial_while_backfilling() {
    let mut torrents = vec![season("Season.Ref", &eps(0..10))];
    for i in 0..25 {
        let mut s = eps(0..10);
        s.push(5 * GIB + i);
        torrents.push(season(&format!("Season.Plus.{i}"), &s));
    }
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    let listed = body["similar_payload"]["hits"].as_array().unwrap().len();
    assert_eq!(listed, MAX_SIBLINGS);
    assert_eq!(body["similar_payload"]["truncated"], true);
    // Newest first: the last 20 added.
    assert_eq!(body["similar_payload"]["hits"][0]["name"], "Season.Plus.24");
    // While the index backfills the field: partial, never "none".
    let g = Generations::current();
    let next = g.get("group_minhash").unwrap() + 1;
    let f = fixture(&torrents, false, Some(g.with("group_minhash", next)));
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(body["similar_payload_partial"], true);
    assert_eq!(body["similar_payload"]["status"], "available");
    // Search off: said.
    let f = build(&torrents, false, None, false);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(
        reason(&body["similar_payload"]),
        ("unavailable", "search_disabled")
    );
    assert_eq!(body["similar_payload_partial"], false);
}

/// An older index serving during a rebuild lacks the fields the groups
/// need (`Availability::Missing`): each group says so with its own
/// reason, never "none" and never partial (rv-772 O2 for GS-1's two).
#[tokio::test]
async fn an_index_without_the_fields_says_each_group_needs_them() {
    // Payload A's clones (0), the v2 sharers (6), the seasons (13).
    let torrents: Vec<T> = clones()
        .into_iter()
        .chain(shared())
        .chain(seasons())
        .collect();
    let older: Older<'_> = (&["payload_sig", "group_minhash"], &["pieces_root"]);
    let f = build_with(&torrents, false, None, true, Some(older));
    let c = session(&f).await;
    for (i, group, why) in [
        (0, "identical_payload", "needs_payload_signature"),
        (6, "same_file", "needs_piece_roots"),
        (13, "similar_payload", "needs_group_minhash"),
    ] {
        let body = siblings(&f, &c, i).await;
        assert_eq!(reason(&body[group]), ("unavailable", why), "{group}");
        assert_eq!(body[group]["hits"], json!([]), "{group}");
        for flag in [
            "identical_payload_partial",
            "same_file_partial",
            "similar_payload_partial",
            "same_file_capped",
        ] {
            assert_eq!(body[flag], false, "{group}: {flag}");
        }
    }
    // The same torrents on a complete index: every group answers.
    let f = plain(&torrents);
    let c = session(&f).await;
    assert_eq!(
        names(&siblings(&f, &c, 0).await["identical_payload"]),
        ["Renamed.Copy"]
    );
    assert_eq!(
        siblings(&f, &c, 6).await["same_file"]["status"],
        "available"
    );
    assert_eq!(
        siblings(&f, &c, 13).await["similar_payload"]["status"],
        "available"
    );
}

/// More visible candidates than the window reads: the group says it may
/// be missing some (`truncated`), even with nothing listed. 1,001
/// near-misses share a band with the reference (8 of 12 sizes: not
/// similar), so the window fills with candidates confirmation rejects.
#[tokio::test]
async fn a_full_candidate_window_says_truncated_however_short_the_list() {
    let reference = eps(0..10);
    let near = (0..10_000i64)
        .map(|k| {
            let mut s = eps(0..8);
            s.extend([2 * GIB + 2 * k, 2 * GIB + 2 * k + 1]);
            s
        })
        .find(|s| share_a_band(&reference, s))
        .expect("a pair sharing a band");
    let mut torrents = vec![season("Season.Ref", &reference)];
    for i in 0..1_001 {
        torrents.push(season(&format!("Near.{i}"), &near));
    }
    let f = plain(&torrents);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(body["similar_payload"]["status"], "available");
    assert_eq!(body["similar_payload"]["hits"], json!([]));
    assert_eq!(body["similar_payload"]["truncated"], true);
    // Fewer than the window: a short list is complete.
    let f = plain(&torrents[..20]);
    let c = session(&f).await;
    let body = siblings(&f, &c, 0).await;
    assert_eq!(body["similar_payload"]["truncated"], false);
}
