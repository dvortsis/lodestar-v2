//! `/torrents/{hash}/siblings` ("Related", §10.5) and `POST /compare`
//! (§10.6) through the router, with goldens in `tests/golden/`
//! (`LODESTAR_BLESS=1` rewrites them).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// One fixture torrent: a name, its files (`""` alone is single-file),
/// whether it's corrected to Adult, and its extra flags.
struct T<'a> {
    name: &'a str,
    files: Vec<(String, i64)>,
    adult: bool,
    flags: i64,
}

fn t<'a>(name: &'a str, files: &[(&str, i64)]) -> T<'a> {
    T {
        name,
        files: files.iter().map(|(p, l)| (p.to_string(), *l)).collect(),
        adult: false,
        flags: 0,
    }
}

fn metainfo(name: &str, files: &[(String, i64)]) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let mut entries = vec![
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ];
    if let [(single, len)] = files
        && single.is_empty()
    {
        entries.push(("length", OwnedValue::Int(*len)));
    } else {
        let list = files
            .iter()
            .map(|(p, len)| {
                d(vec![
                    ("length", OwnedValue::Int(*len)),
                    ("path", OwnedValue::List(p.split('/').map(b).collect())),
                ])
            })
            .collect();
        entries.push(("files", OwnedValue::List(list)));
    }
    to_vec(&d(entries))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    hashes: Vec<String>,
    _dir: tempfile::TempDir,
}

fn fixture(torrents: &[T<'_>], search: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = torrents
        .iter()
        .map(|t| metainfo(t.name, &t.files))
        .collect();
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
    // Classified, as the pipeline would have; `adult` as an Is adult
    // correction would.
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
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 1000).unwrap();
    w.commit().unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: search.then(|| index.into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
        }),
    );
    let hashes = infos
        .iter()
        .map(|i| {
            i.primary_hash()
                .0
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        })
        .collect();
    Fixture {
        app,
        auth,
        hashes,
        _dir: dir,
    }
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn json_of(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let (status, body) = call(app, req).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::get(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::empty()).unwrap()
}

fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let resp = f
        .app
        .clone()
        .oneshot(post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": PASSWORD }),
        ))
        .await
        .unwrap();
    resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// Checks the status and body against `tests/golden/{name}.txt`.
async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    let (status, body) = call(&f.app, req).await;
    let got = format!("{}\n{body}\n", status.as_u16());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap();
    } else {
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display()));
        assert_eq!(got, want.replace("\r\n", "\n"), "{name}: the response changed");
    }
    serde_json::from_str(&body).unwrap_or(Value::Null)
}

const GB: i64 = 1 << 30;

/// Indexes into [`works`].
const KNIGHT_1080: usize = 0;
const KNIGHT_720: usize = 1;
const KNIGHT_LOWER: usize = 2;
const RISES: usize = 3;
const KNIGHT_1994: usize = 4;
const SHOW_S1E2: usize = 5;
const SHOW_S1E2_WEB: usize = 6;
const SHOW_S1E3: usize = 7;
const SHOW_S2E2: usize = 8;
const KNIGHT_HIDDEN: usize = 9;
const KNIGHT_ADULT: usize = 10;
const BEGINS: usize = 11;

fn works() -> Vec<T<'static>> {
    let srt = ("Subs/English.srt", 90_000);
    let mut hidden = t(
        "The.Dark.Knight.2008.2160p.UHD.BluRay.x265-HID",
        &[("The.Dark.Knight.2008.2160p.mkv", 40 * GB)],
    );
    hidden.flags = flags::HIDDEN;
    let mut adult = t(
        "The.Dark.Knight.2008.480p.XviD-ADL",
        &[("The.Dark.Knight.2008.480p.avi", GB)],
    );
    adult.adult = true;
    vec![
        t(
            "The.Dark.Knight.2008.1080p.BluRay.x264-GRP",
            &[("The.Dark.Knight.2008.1080p.mkv", 8 * GB), srt],
        ),
        t(
            "The Dark Knight (2008) 720p WEB-DL",
            &[("The Dark Knight (2008).mkv", 4 * GB), srt],
        ),
        t(
            "the.dark.knight.2008.2160p.web.h265-low",
            &[("the.dark.knight.2008.2160p.mkv", 20 * GB)],
        ),
        t(
            "The.Dark.Knight.Rises.2012.1080p.BluRay.x264-GRP",
            &[("The.Dark.Knight.Rises.2012.1080p.mkv", 9 * GB)],
        ),
        t(
            "The.Dark.Knight.1994.720p.WEB-DL",
            &[("The.Dark.Knight.1994.720p.mkv", 2 * GB)],
        ),
        t(
            "Show.Name.S01E02.1080p.WEB.x264-AAA",
            &[("Show.Name.S01E02.1080p.mkv", GB)],
        ),
        t(
            "Show Name S01E02 720p HDTV x264-BBB",
            &[("Show Name S01E02 720p.mkv", GB / 2)],
        ),
        t(
            "Show.Name.S01E03.1080p.WEB.x264-AAA",
            &[("Show.Name.S01E03.1080p.mkv", GB)],
        ),
        t(
            "Show.Name.S02E02.1080p.WEB.x264-AAA",
            &[("Show.Name.S02E02.1080p.mkv", GB)],
        ),
        hidden,
        adult,
        t("Batman.Begins.2005.720p", &[("Batman.Begins.mkv", 4 * GB)]),
    ]
}

fn names(group: &Value) -> Vec<String> {
    group["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn other_versions_match_the_title_identity() {
    let all = works();
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());
    let siblings = |i: usize| format!("/api/v1/torrents/{}/siblings", f.hashes[i]);

    let body = golden(&f, "siblings_movie", get(&siblings(KNIGHT_1080), c)).await;
    let group = &body["other_versions"];
    assert_eq!(group["status"], "available");
    assert_eq!(group["reason"], Value::Null);
    let mut got = names(group);
    got.sort();
    let mut want = vec![all[KNIGHT_720].name, all[KNIGHT_LOWER].name];
    want.sort();
    // Title variants of the same work; not Rises (another title), not
    // 1994 (another year), not itself, not the hidden or Adult copies.
    assert_eq!(got, want);
    assert_eq!(group["truncated"], false);
    assert_eq!(body["identity"]["years"], json!(["2008"]));
    assert_eq!(body["identity"]["title_words"], json!(["the", "dark", "knight"]));
    // Each hit is a search row, classification included.
    assert_eq!(group["hits"][0]["content_type"], "movie");

    // The variants find each other.
    for i in [KNIGHT_720, KNIGHT_LOWER] {
        let (status, body) = json_of(&f.app, get(&siblings(i), c)).await;
        assert_eq!(status, StatusCode::OK);
        let got = names(&body["other_versions"]);
        assert!(got.contains(&all[KNIGHT_1080].name.to_string()), "{i}: {got:?}");
        assert!(!got.contains(&all[i].name.to_string()), "{i}: self listed");
    }

    // Seasons and episodes are kept apart.
    let body = golden(&f, "siblings_episode", get(&siblings(SHOW_S1E2), c)).await;
    assert_eq!(names(&body["other_versions"]), vec![all[SHOW_S1E2_WEB].name]);
    let (_, body) = json_of(&f.app, get(&siblings(SHOW_S2E2), c)).await;
    assert_eq!(names(&body["other_versions"]), Vec::<String>::new());
    assert_eq!(body["other_versions"]["status"], "available");
    let (_, body) = json_of(&f.app, get(&siblings(SHOW_S1E3), c)).await;
    assert_eq!(names(&body["other_versions"]), Vec::<String>::new());
    // Another year is its own work.
    let (_, body) = json_of(&f.app, get(&siblings(KNIGHT_1994), c)).await;
    assert_eq!(names(&body["other_versions"]), Vec::<String>::new());
    let (_, body) = json_of(&f.app, get(&siblings(RISES), c)).await;
    assert_eq!(names(&body["other_versions"]), Vec::<String>::new());

    // The M7 groups are typed and unavailable, never empty-as-none.
    for (group, reason) in [
        ("identical_payload", "needs_payload_signature"),
        ("same_file", "needs_piece_roots"),
    ] {
        assert_eq!(body[group]["status"], "unavailable");
        assert_eq!(body[group]["reason"], reason);
        assert_eq!(body[group]["hits"], json!([]));
    }
}

#[tokio::test]
async fn siblings_respect_hidden_and_adult() {
    let all = works();
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());
    let siblings = |i: usize| format!("/api/v1/torrents/{}/siblings", f.hashes[i]);

    // A torrent a filter rule hid opens in the drawer (`hidden=true`), so
    // its Related answers, but lists no hidden or Adult torrents.
    let (status, body) = json_of(&f.app, get(&siblings(KNIGHT_HIDDEN), c)).await;
    assert_eq!(status, StatusCode::OK);
    let got = names(&body["other_versions"]);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(!got.contains(&all[KNIGHT_ADULT].name.to_string()));

    // Adult while hidden is unknown, and the body names nothing.
    let (status, body) = call(&f.app, get(&siblings(KNIGHT_ADULT), c)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("torrent_not_found"), "{body}");
    assert!(!body.contains("Knight"), "{body}");

    golden(
        &f,
        "siblings_unknown",
        get(
            &format!("/api/v1/torrents/{}/siblings", "ab".repeat(20)),
            c,
        ),
    )
    .await;
    golden(
        &f,
        "siblings_bad_hash",
        get("/api/v1/torrents/xyz/siblings", c),
    )
    .await;
    let (status, _) = call(&f.app, get(&siblings(KNIGHT_1080), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn siblings_without_a_title_or_search() {
    let all = vec![
        t("Batman.Begins.2005.720p", &[("Batman.Begins.mkv", 4 * GB)]),
        t("x", &[("", 1000)]),
    ];
    let f = fixture(&all, false);
    let c = session(&f).await;
    let c = Some(c.as_str());
    let (_, body) = json_of(
        &f.app,
        get(&format!("/api/v1/torrents/{}/siblings", f.hashes[0]), c),
    )
    .await;
    assert_eq!(body["other_versions"]["status"], "unavailable");
    assert_eq!(body["other_versions"]["reason"], "search_disabled");
    let f = fixture(&all, true);
    let c = session(&f).await;
    let (_, body) = json_of(
        &f.app,
        get(
            &format!("/api/v1/torrents/{}/siblings", f.hashes[1]),
            Some(&c),
        ),
    )
    .await;
    if body["identity"].is_null() {
        assert_eq!(body["other_versions"]["reason"], "no_title");
        assert_eq!(body["other_versions"]["status"], "unavailable");
    }
}

fn compare(f: &Fixture, idx: &[usize], c: Option<&str>) -> Request<Body> {
    let hashes: Vec<&str> = idx.iter().map(|&i| f.hashes[i].as_str()).collect();
    post("/api/v1/compare", c, json!({ "hashes": hashes }))
}

#[tokio::test]
async fn compare_two_and_five() {
    let all = works();
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());

    let body = golden(&f, "compare_two", compare(&f, &[KNIGHT_1080, KNIGHT_720], c)).await;
    let cols = body["torrents"].as_array().unwrap();
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0]["name"], all[KNIGHT_1080].name);
    assert_eq!(cols[1]["name"], all[KNIGHT_720].name);
    let differs = &body["differs"];
    assert_eq!(differs["title"], false, "{differs}");
    assert_eq!(differs["year"], false);
    assert_eq!(differs["res"], true);
    // Every attribute in `differs` is in every column, null when absent.
    for col in cols {
        let attrs = col["attributes"].as_object().unwrap();
        let keys: Vec<&String> = attrs.keys().collect();
        let want: Vec<&String> = differs.as_object().unwrap().keys().collect();
        assert_eq!(keys, want);
    }
    for (k, d) in differs.as_object().unwrap() {
        assert_eq!(
            d.as_bool().unwrap(),
            cols[0]["attributes"][k] != cols[1]["attributes"][k],
            "{k}"
        );
    }
    // The shared scale is the largest size.
    assert_eq!(body["scale_bytes"], json!(8 * GB + 90_000));
    // Files match on path and size: the subtitles are common.
    let diff = &body["file_diff"];
    assert_eq!(
        diff["common"],
        json!({ "count": 1, "files": [{ "path": "Subs/English.srt", "size": 90_000 }], "truncated": false })
    );
    assert_eq!(diff["only"][0]["files"][0]["path"], "The.Dark.Knight.2008.1080p.mkv");
    assert_eq!(diff["only"][1]["files"][0]["path"], "The Dark Knight (2008).mkv");

    let five = [KNIGHT_1080, KNIGHT_720, KNIGHT_LOWER, RISES, BEGINS];
    let body = golden(&f, "compare_five", compare(&f, &five, c)).await;
    assert_eq!(body["torrents"].as_array().unwrap().len(), 5);
    assert_eq!(body["differs"]["title"], true);
    assert_eq!(body["file_diff"]["only"].as_array().unwrap().len(), 5);
    assert_eq!(body["file_diff"]["common"]["count"], 0);
    // With three or more, a file two share is in neither list.
    for only in body["file_diff"]["only"].as_array().unwrap() {
        assert!(!only.to_string().contains("English.srt"), "{only}");
    }
    assert_eq!(body["scale_bytes"], json!(20 * GB));
}

#[tokio::test]
async fn compare_refuses_bad_sets() {
    let all = works();
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());
    golden(&f, "compare_one", compare(&f, &[KNIGHT_1080], c)).await;
    let six = [0, 1, 2, 3, 4, 5];
    golden(&f, "compare_six", compare(&f, &six, c)).await;
    golden(
        &f,
        "compare_duplicate",
        compare(&f, &[KNIGHT_1080, KNIGHT_1080], c),
    )
    .await;
    golden(
        &f,
        "compare_bad_hash",
        post("/api/v1/compare", c, json!({ "hashes": ["xyz", f.hashes[0]] })),
    )
    .await;
    // Hidden, or Adult while hidden: as unknown, naming nothing.
    for i in [KNIGHT_HIDDEN, KNIGHT_ADULT] {
        let (status, body) = call(&f.app, compare(&f, &[KNIGHT_1080, i], c)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body.contains("torrent_not_found"), "{body}");
        assert!(!body.contains(all[i].name), "{body}");
        let (_, unknown) = call(
            &f.app,
            post(
                "/api/v1/compare",
                c,
                json!({ "hashes": [f.hashes[KNIGHT_1080], "ab".repeat(20)] }),
            ),
        )
        .await;
        assert_eq!(body, unknown, "the same as an unknown hash");
    }
    golden(
        &f,
        "compare_hidden",
        compare(&f, &[KNIGHT_1080, KNIGHT_HIDDEN], c),
    )
    .await;
    let (status, _) = call(&f.app, compare(&f, &[KNIGHT_1080, KNIGHT_720], None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_file_diff_is_bounded() {
    let big: Vec<(String, i64)> = (0..2100).map(|i| (format!("d/f{i:05}.bin"), 1000)).collect();
    let small: Vec<(String, i64)> = (0..50)
        .map(|i| (format!("d/f{i:05}.bin"), 1000))
        .chain([("extra.txt".to_string(), 5)])
        .collect();
    // Same path, other size: not a match.
    let mut resized = small.clone();
    resized[0].1 = 999;
    let huge: Vec<(String, i64)> = (0..60_000)
        .map(|i| (format!("h/{i:06}.dat"), 1 + i))
        .collect();
    let all = vec![
        T {
            name: "Big Pack",
            files: big,
            adult: false,
            flags: 0,
        },
        T {
            name: "Small Pack",
            files: small,
            adult: false,
            flags: 0,
        },
        T {
            name: "Resized Pack",
            files: resized,
            adult: false,
            flags: 0,
        },
        T {
            name: "Huge Pack",
            files: huge,
            adult: false,
            flags: 0,
        },
    ];
    let f = fixture(&all, true);
    let c = session(&f).await;
    let c = Some(c.as_str());

    let (status, body) = json_of(&f.app, compare(&f, &[0, 1], c)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let diff = &body["file_diff"];
    assert_eq!(diff["common"]["count"], 50);
    assert_eq!(diff["common"]["files"].as_array().unwrap().len(), 50);
    assert_eq!(diff["common"]["truncated"], false);
    assert_eq!(diff["only"][0]["count"], 2050);
    assert_eq!(diff["only"][0]["files"].as_array().unwrap().len(), 2000);
    assert_eq!(diff["only"][0]["truncated"], true);
    // In the torrent's order.
    assert_eq!(diff["only"][0]["files"][0]["path"], "d/f00050.bin");
    assert_eq!(diff["only"][1]["files"], json!([{ "path": "extra.txt", "size": 5 }]));

    let (_, body) = json_of(&f.app, compare(&f, &[1, 2], c)).await;
    let diff = &body["file_diff"];
    assert_eq!(diff["common"]["count"], 50);
    assert_eq!(diff["only"][0]["files"], json!([{ "path": "d/f00000.bin", "size": 1000 }]));
    assert_eq!(diff["only"][1]["files"], json!([{ "path": "d/f00000.bin", "size": 999 }]));

    // A 60,000-file tree against itself-sized others: every list stays
    // within its cap, and the response stays small.
    let (status, text) = call(&f.app, compare(&f, &[3, 0, 1], c)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.len() < 400_000, "{} bytes", text.len());
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["file_diff"]["only"][0]["count"], 60_000);
    assert_eq!(body["file_diff"]["only"][0]["truncated"], true);
    for list in body["file_diff"]["only"].as_array().unwrap() {
        assert!(list["files"].as_array().unwrap().len() <= 2000);
    }
}
