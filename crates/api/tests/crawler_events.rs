//! The crawler's controls publish `settings` on the `/events` hub
//! (carry-forward from B2b): `POST /crawler/pause`, `/crawler/resume`
//! and `PATCH /crawler` each publish one `{section: "crawler", revision}`
//! when they change the section, and a no-op publishes nothing. The
//! responses themselves are `tests/ops.rs`'s goldens, unchanged.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, HttpBody};
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::{Extension, Router};
use lodestar_api::events::{EventHub, EventSources, EventsConfig};
use lodestar_api::ops::{
    BoxFuture, CrawlerRuntime, FetchStatus, NetworkCheck, OpsApi, OpsError, Source, SourcePatch,
    StatsResponse,
};
use lodestar_api::settings::{
    Imported, Saved, SectionValues, SettingSource, SettingValue, SettingsApi, SettingsBatch,
    SettingsFailure, SettingsImport, SettingsIndex, SettingsPreview,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The crawler section, with a revision that moves when a save changes
/// it, as the registry's does.
#[derive(Debug)]
struct FakeSettings {
    values: Mutex<(BTreeMap<String, Value>, u64)>,
}

impl SettingsApi for FakeSettings {
    fn index(&self) -> SettingsIndex {
        SettingsIndex {
            sections: Vec::new(),
        }
    }

    fn section(&self, section: &str) -> Option<SectionValues> {
        let (values, revision) = &*self.values.lock().unwrap();
        (section == "crawler").then(|| SectionValues {
            fields: values
                .iter()
                .map(|(k, v)| {
                    let value = SettingValue {
                        env: None,
                        read_only: false,
                        restart_required: false,
                        source: SettingSource::Db,
                        value: v.clone(),
                    };
                    (k.clone(), value)
                })
                .collect(),
            restart_required: Vec::new(),
            revision: *revision,
            section: "crawler".into(),
        })
    }

    fn save(&self, batch: SettingsBatch) -> Result<Saved, SettingsFailure> {
        let mut changed = Vec::new();
        for (section, patch) in batch.0 {
            assert_eq!(section, "crawler");
            let mut state = self.values.lock().unwrap();
            let mut any = false;
            for (k, v) in patch.0 {
                any |= state.0.insert(k, v.clone()) != Some(v);
            }
            if any {
                state.1 += 1;
                changed.push((section, state.1));
            }
        }
        Ok(Saved {
            sections: vec![self.section("crawler").unwrap()],
            changed,
        })
    }

    fn preview(&self, _: SettingsBatch) -> Result<SettingsPreview, SettingsFailure> {
        unreachable!()
    }

    fn export(&self, _: bool) -> String {
        unreachable!()
    }

    fn import(&self, _: SettingsImport, _: bool) -> Result<Imported, SettingsFailure> {
        unreachable!()
    }
}

#[derive(Debug)]
struct FakeOps;

impl OpsApi for FakeOps {
    fn crawler(&self) -> CrawlerRuntime {
        CrawlerRuntime {
            since: 1_700_000_000,
            dht: None,
            fetch: FetchStatus {
                in_flight: 0,
                lookups: 0,
                queued: 0,
            },
        }
    }

    fn sync_crawler(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn stats(&self) -> BoxFuture<'_, StatsResponse> {
        unreachable!()
    }

    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>> {
        unreachable!()
    }

    fn patch_source(&self, _: u8, _: SourcePatch) -> BoxFuture<'_, Result<Source, OpsError>> {
        unreachable!()
    }

    fn network(&self) -> NetworkCheck {
        unreachable!()
    }

    fn restart_network_check(&self) -> Result<NetworkCheck, OpsError> {
        unreachable!()
    }
}

struct Fixture {
    app: Router,
    hub: Arc<EventHub>,
    cookie: String,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let token = auth.setup_token().unwrap();
    let cookie = auth
        .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let settings = FakeSettings {
        values: Mutex::new((
            [
                ("intensity".to_string(), json!("balanced")),
                ("paused".to_string(), json!(false)),
            ]
            .into(),
            0,
        )),
    };
    let hub = EventHub::start(EventsConfig::default(), EventSources::default());
    let app = Router::new()
        .nest(
            "/api/v1",
            router(ApiState {
                auth,
                index: None,
                reader: Arc::new(Mutex::new(reader)),
                filters: None,
                settings: Some(Arc::new(settings)),
                ops: Some(Arc::new(FakeOps)),
                user_data: None,
                insights: None,
                adult: Default::default(),
                hot: None,
                events: None,
            }),
        )
        .layer(Extension(hub.clone()));
    Fixture {
        app,
        hub,
        cookie,
        _dir: dir,
    }
}

fn req(f: &Fixture, method: &str, uri: &str, body: &str) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, &f.cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    req
}

async fn send(f: &Fixture, req: Request<Body>) -> u16 {
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    status
}

/// Reads `body` until `needle` appears, or fails after 5 s.
async fn read_until(body: &mut Body, needle: &str) -> String {
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !text.contains(needle) {
        let frame = tokio::time::timeout_at(
            deadline,
            std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)),
        )
        .await
        .unwrap_or_else(|_| panic!("no {needle:?} in {text:?}"))
        .expect("the stream ended")
        .unwrap();
        if let Ok(data) = frame.into_data() {
            text.push_str(std::str::from_utf8(&data).unwrap());
        }
    }
    text
}

#[tokio::test]
async fn crawler_controls_publish_settings_once_per_change() {
    let f = fixture();
    let stream = f
        .app
        .clone()
        .oneshot(req(&f, "GET", "/api/v1/events?topics=settings", ""))
        .await
        .unwrap();
    assert_eq!(stream.status(), 200);
    let mut body = stream.into_body();
    read_until(&mut body, "retry:").await;
    let id = f.hub.epoch().to_string();
    let event = |n: u64, revision: u64| {
        format!(
            "id: {id}-{n}\nevent: settings\ndata: {{\"revision\":{revision},\"section\":\"crawler\"}}\n\n"
        )
    };

    assert_eq!(
        send(&f, req(&f, "POST", "/api/v1/crawler/pause", "")).await,
        200
    );
    // A no-op pause publishes nothing: the marker is the next event.
    assert_eq!(
        send(&f, req(&f, "POST", "/api/v1/crawler/pause", "")).await,
        200
    );
    f.hub.settings_saved("marker", 0);
    let text = read_until(&mut body, "marker").await;
    assert!(text.contains(&event(1, 1)), "{text}");
    assert!(
        text.contains(&format!(
            "id: {id}-2\nevent: settings\ndata: {{\"revision\":0,\"section\":\"marker\"}}"
        )),
        "{text}"
    );

    assert_eq!(
        send(&f, req(&f, "POST", "/api/v1/crawler/resume", "")).await,
        200
    );
    let text = read_until(&mut body, &event(3, 2)).await;
    assert!(
        !text.contains("intensity") && !text.contains("paused"),
        "{text}"
    );

    assert_eq!(
        send(
            &f,
            req(&f, "PATCH", "/api/v1/crawler", r#"{"intensity":"low"}"#)
        )
        .await,
        200
    );
    read_until(&mut body, &event(4, 3)).await;
    // The same intensity again changes nothing.
    assert_eq!(
        send(
            &f,
            req(&f, "PATCH", "/api/v1/crawler", r#"{"intensity":"low"}"#)
        )
        .await,
        200
    );
    f.hub.settings_saved("marker", 1);
    let text = read_until(&mut body, "\"revision\":1,\"section\":\"marker\"").await;
    assert!(text.contains(&format!("id: {id}-5\n")), "{text}");
}

#[tokio::test]
async fn without_a_hub_the_controls_still_work() {
    let mut f = fixture();
    // As `tests/ops.rs` builds it: no hub layered.
    f.app = Router::new();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let token = auth.setup_token().unwrap();
    f.cookie = auth
        .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    f.app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth,
            index: None,
            reader: Arc::new(Mutex::new(
                Store::open_reader(&db, StoreConfig::default()).unwrap(),
            )),
            filters: None,
            settings: Some(Arc::new(FakeSettings {
                values: Mutex::new((
                    [
                        ("paused".to_string(), json!(false)),
                        ("intensity".to_string(), json!("low")),
                    ]
                    .into(),
                    0,
                )),
            })),
            ops: Some(Arc::new(FakeOps)),
            user_data: None,
            insights: None,
            adult: Default::default(),
            hot: None,
            events: None,
        }),
    );
    assert_eq!(
        send(&f, req(&f, "POST", "/api/v1/crawler/pause", "")).await,
        200
    );
}
