//! `POST /send`'s rate limit, alone in its own binary: the limit is
//! process-wide, so other sends running beside it would use it up.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::settings::{
    DownloadClientKind, Imported, Saved, SavedClient, SectionValues, SettingsApi, SettingsBatch,
    SettingsFailure, SettingsImport, SettingsIndex, SettingsPreview,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Debug)]
struct OneClient;

impl SettingsApi for OneClient {
    fn index(&self) -> SettingsIndex {
        unimplemented!()
    }
    fn section(&self, _: &str) -> Option<SectionValues> {
        unimplemented!()
    }
    fn save(&self, _: SettingsBatch) -> Result<Saved, SettingsFailure> {
        unimplemented!()
    }
    fn preview(&self, _: SettingsBatch) -> Result<SettingsPreview, SettingsFailure> {
        unimplemented!()
    }
    fn export(&self, _: bool) -> String {
        unimplemented!()
    }
    fn import(&self, _: SettingsImport, _: bool) -> Result<Imported, SettingsFailure> {
        unimplemented!()
    }
    fn download_client(&self, _: Option<&str>) -> Option<SavedClient> {
        // Never reached: every hash below is unknown, so nothing is sent.
        Some(SavedClient {
            id: "home".into(),
            kind: DownloadClientKind::Qbittorrent,
            url: "http://192.0.2.1/".into(),
            username: String::new(),
            password: None,
            category: String::new(),
            save_path: String::new(),
            paused: false,
        })
    }
}

#[tokio::test]
async fn sends_past_the_limit_are_refused_and_send_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let token = auth.setup_token().unwrap();
    let set = auth
        .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
        .unwrap();
    let cookie = set.split(';').next().unwrap().to_string();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth,
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: Some(Arc::new(OneClient)),
            ops: None,
            user_data: None,
            adult: Default::default(),
            insights: None,
            hot: None,
            events: None,
        }),
    );
    let send = || {
        let mut req = Request::post("/api/v1/send")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie)
            .body(Body::from(
                json!({ "hashes": ["f".repeat(40)] }).to_string(),
            ))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
        let app = app.clone();
        async move {
            let resp = app.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        }
    };
    for _ in 0..lodestar_api::settings::MAX_SENDS {
        let (status, v) = send().await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["results"][0]["code"], json!("not_found"));
    }
    let (status, v) = send().await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"]["code"], json!("send_rate_limited"));
}
