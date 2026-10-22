//! The API end to end through the router: auth, CSRF checks, search,
//! parse and torrent detail.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

fn torrent(name: &str, files: &[&str]) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let files = files
        .iter()
        .map(|p| {
            d(vec![
                ("length", OwnedValue::Int(1 << 20)),
                ("path", OwnedValue::List(p.split('/').map(b).collect())),
            ])
        })
        .collect();
    to_vec(&d(vec![
        ("files", OwnedValue::List(files)),
        ("name", b(name)),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
    ]))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    hashes: Vec<String>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws = [
        torrent(
            "The.Dark.Knight.2008.1080p",
            &["The.Dark.Knight.mkv", "Subs/en.srt"],
        ),
        torrent("Batman Begins 720p", &["batman.mkv"]),
    ];
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .map(|(info, raw)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 10).unwrap();
    w.commit().unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(index.into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
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
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, cookie, body)
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

async fn logged_in(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let (status, cookie, _) = call(
        &f.app,
        post(
            "/api/v1/setup",
            None,
            json!({ "token": token, "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

#[tokio::test]
async fn setup_login_and_protection() {
    let f = fixture();
    let (_, _, body) = call(&f.app, get("/api/v1/auth", None)).await;
    assert_eq!(body, json!({ "setup_required": true, "logged_in": false }));
    let (status, _, body) = call(&f.app, get("/api/v1/search?q=knight", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["setup_required"], json!(true));
    let (status, _, _) = call(
        &f.app,
        post(
            "/api/v1/setup",
            None,
            json!({ "token": "nope", "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let cookie = logged_in(&f).await;
    let (_, _, body) = call(&f.app, get("/api/v1/auth", Some(&cookie))).await;
    assert_eq!(body, json!({ "setup_required": false, "logged_in": true }));
    let (status, _, _) = call(&f.app, get("/api/v1/search?q=knight", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, again, _) = call(
        &f.app,
        post(
            "/api/v1/login",
            None,
            json!({ "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(again.unwrap().starts_with("lodestar_session="));
    let (status, _, _) = call(
        &f.app,
        post(
            "/api/v1/login",
            None,
            json!({ "password": "wrong password!" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (_, cleared, _) = call(&f.app, post("/api/v1/logout", Some(&cookie), json!({}))).await;
    assert_eq!(cleared.as_deref(), Some("lodestar_session="));
}

#[tokio::test]
async fn cross_site_writes_are_refused() {
    let f = fixture();
    let form = Request::post("/api/v1/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("password=x"))
        .unwrap();
    assert_eq!(
        call(&f.app, form).await.0,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    let foreign = Request::post("/api/v1/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "lodestar.lan:8090")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::from(r#"{"password":"x"}"#))
        .unwrap();
    assert_eq!(call(&f.app, foreign).await.0, StatusCode::FORBIDDEN);
    let same = Request::post("/api/v1/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "lodestar.lan:8090")
        .header(header::ORIGIN, "http://lodestar.lan:8090")
        .body(Body::from(r#"{"password":"x"}"#))
        .unwrap();
    assert_eq!(
        call(&f.app, same).await.0,
        StatusCode::CONFLICT,
        "not set up yet, but allowed through"
    );
}

#[tokio::test]
async fn search_parse_and_detail() {
    let f = fixture();
    let c = logged_in(&f).await;
    let c = Some(c.as_str());

    let (status, _, body) = call(&f.app, get("/api/v1/search?q=dark%20knight", c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["hits"][0]["name"], json!("The.Dark.Knight.2008.1080p"));
    assert_eq!(body["hits"][0]["hash"], json!(f.hashes[0]));
    assert_eq!(body["hits"][0]["files"], json!(2));
    assert_eq!(body["stemmed"], json!(false));

    // A file clause lists the matching files of each hit.
    let (_, _, body) = call(&f.app, get("/api/v1/search?q=file:(ext:srt)", c)).await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["hits"][0]["matched_files"], json!([1]));
    let (_, _, body) = call(&f.app, get("/api/v1/search?q=dark", c)).await;
    assert!(body["hits"][0].get("matched_files").is_none());

    let (_, _, body) = call(&f.app, get("/api/v1/search?q=mkv&sort=%2Bsize&limit=1", c)).await;
    assert_eq!(
        (
            body["total"].clone(),
            body["hits"].as_array().unwrap().len()
        ),
        (json!(2), 1)
    );
    assert_eq!(body["hits"][0]["name"], json!("Batman Begins 720p"));

    let (status, _, body) = call(&f.app, get("/api/v1/search?q=sise:1", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        (body["error"]["start"].clone(), body["error"]["end"].clone()),
        (json!(0), json!(5))
    );
    let (status, _, _) = call(&f.app, get("/api/v1/search?q=x&sort=seeders", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = call(&f.app, get("/api/v1/search?q=x&offset=9990&limit=50", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, _, parsed) = call(&f.app, get("/api/v1/parse?q=batman%20-cam", c)).await;
    assert_eq!(parsed["canonical"], json!("batman -cam"));
    let (status, _, body) = call(
        &f.app,
        post("/api/v1/search", c, json!({ "ast": parsed["ast"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hits"][0]["name"], json!("Batman Begins 720p"));
    let bad_ast = json!({ "ast": { "op": "word", "field": "any", "word": "two words" } });
    let (status, _, _) = call(&f.app, post("/api/v1/search", c, bad_ast)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, body) =
        call(&f.app, get(&format!("/api/v1/torrents/{}", f.hashes[0]), c)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["file_list_total"], json!(2));
    assert_eq!(body["file_list"][1]["path"], json!("Subs/en.srt"));
    assert!(
        body["magnet"]
            .as_str()
            .unwrap()
            .starts_with(&format!("magnet:?xt=urn:btih:{}", f.hashes[0]))
    );
    let (_, _, body) = call(
        &f.app,
        get(
            &format!("/api/v1/torrents/{}/files?offset=1&limit=5", f.hashes[0]),
            c,
        ),
    )
    .await;
    assert_eq!(body["files"].as_array().unwrap().len(), 1);
    let (status, _, _) = call(
        &f.app,
        get(&format!("/api/v1/torrents/{}", "0".repeat(40)), c),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(&f.app, get("/api/v1/torrents/xyz", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn errors_share_one_envelope() {
    let f = fixture();
    // Without a session, a protected path's wrong method is still a 401.
    for req in [
        post("/api/v1/parse", None, json!({})),
        Request::delete("/api/v1/search")
            .body(Body::empty())
            .unwrap(),
        Request::delete("/api/v1/password")
            .body(Body::empty())
            .unwrap(),
    ] {
        let (status, _, body) = call(&f.app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], json!("login_required"));
    }
    // 401 before login, with the hint the login screen needs.
    let (status, _, body) = call(&f.app, get("/api/v1/search?q=x", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], json!("login_required"));
    assert!(body["error"]["message"].is_string());
    assert_eq!(body["setup_required"], json!(true));

    let cookie = logged_in(&f).await;
    let c = Some(cookie.as_str());
    // 400: a parse error keeps its span.
    let (status, _, body) = call(&f.app, get("/api/v1/parse?q=sise:1", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let e = &body["error"];
    assert_eq!(
        (&e["code"], &e["field"], &e["start"], &e["end"]),
        (&json!("query_syntax"), &json!("q"), &json!(0), &json!(5))
    );
    assert!(e["message"].as_str().is_some_and(|m| !m.is_empty()));
    let (_, _, body) = call(&f.app, get("/api/v1/search?q=x&sort=seeders", c)).await;
    assert_eq!(
        body,
        json!({ "error": {
            "code": "unknown_sort",
            "message": body["error"]["message"],
            "field": "sort",
        } })
    );

    // 404: an unknown torrent, and an unknown endpoint.
    let (status, _, body) = call(
        &f.app,
        get(&format!("/api/v1/torrents/{}", "0".repeat(40)), c),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("torrent_not_found"));
    let (status, _, body) = call(&f.app, get("/api/v1/nope", c)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("no_route"));

    // 405: a known path with the wrong method (with a session, for a
    // protected one).
    for req in [
        post("/api/v1/parse", c, json!({})),
        Request::delete(format!("/api/v1/torrents/{}", f.hashes[0]))
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
        get("/api/v1/login", None),
    ] {
        let (status, _, body) = call(&f.app, req).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(body["error"]["code"], json!("method_not_allowed"));
    }

    // A huge offset is refused, not overflowed.
    let (status, _, body) = call(
        &f.app,
        get("/api/v1/search?q=x&offset=18446744073709551615", c),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        (&body["error"]["code"], &body["error"]["field"]),
        (&json!("window_too_deep"), &json!("offset"))
    );
    let (status, _, body) = call(&f.app, get("/api/v1/search?q=x&limit=lots", c)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"],
        json!({
            "code": "bad_parameter",
            "message": "A query parameter has the wrong type or value.",
            "field": "limit",
        })
    );

    // Rejected bodies and wrong passwords use it too, with their field.
    let form = Request::post("/api/v1/login")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("x"))
        .unwrap();
    let (status, _, body) = call(&f.app, form).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["error"]["code"], json!("unsupported_media_type"));
    let (status, _, body) = call(&f.app, post("/api/v1/login", None, json!({}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["error"],
        json!({
            "code": "bad_body",
            "message": "The request body isn't valid JSON for this endpoint.",
            "field": "password",
        })
    );
    let (_, _, body) = call(
        &f.app,
        post("/api/v1/search", c, json!({ "q": "x", "limit": "ten" })),
    )
    .await;
    assert_eq!(body["error"]["field"], json!("limit"));
    let broken = Request::post("/api/v1/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{\"password\": "))
        .unwrap();
    let (status, _, body) = call(&f.app, broken).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("bad_json"));
    let (status, _, body) = call(
        &f.app,
        post(
            "/api/v1/password",
            c,
            json!({ "old": "not the password", "new": "whatever whatever" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        (&body["error"]["code"], &body["error"]["field"]),
        (&json!("bad_password"), &json!("old"))
    );
}

#[tokio::test]
async fn protected_routes_fail_closed_outside_the_router() {
    let f = fixture();
    let cookie = logged_in(&f).await;
    let reader = Store::open_reader(f._dir.path().join("t.db"), StoreConfig::default()).unwrap();
    // Mounted without `router`, the guard has nothing to check against.
    let stray = lodestar_api::auth::protected(
        Router::new().route("/x", axum::routing::get(|| async { "secret" })),
    )
    .with_state(ApiState {
        auth: f.auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(reader)),
        filters: None,
    });
    let (status, _, body) = call(&stray, get("/x", Some(&cookie))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], json!("internal"));
}
