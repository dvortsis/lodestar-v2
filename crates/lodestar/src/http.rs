//! The M1 HTTP surface: a basic status page, its JSON, Prometheus metrics
//! and a health check. All read-only; the real API and UI arrive later.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use lodestar_api::{ApiState, require_session};
use lodestar_dht::node::StatsSnapshot;
use lodestar_pipeline::Monitor;
use lodestar_search::{IndexStats, SearchIndex};
use lodestar_sources::Stage;
use lodestar_store::Store;
use serde_json::{Value, json};

use crate::metrics::{self, FAILURE_KINDS, LOSSES};

const STATUS_PAGE: &str = include_str!("status.html");

#[derive(Clone)]
pub struct AppState {
    pub monitor: Monitor,
    pub dht: Option<Arc<Mutex<Option<StatsSnapshot>>>>,
    /// A read-only store connection for the funnel and recent torrents.
    pub reader: Arc<Mutex<Store>>,
    pub search: Option<SearchState>,
}

#[derive(Clone)]
pub struct SearchState {
    pub index: SearchIndex,
    pub stats: Arc<IndexStats>,
}

impl AppState {
    fn dht(&self) -> Option<StatsSnapshot> {
        self.dht
            .as_ref()
            .and_then(|d| *d.lock().expect("dht stats lock"))
    }
}

/// The page and health/metrics endpoints are open (the page asks for a
/// login itself; metrics carry no torrent names). Status and the API need
/// a session.
pub fn router(state: AppState, api: ApiState) -> Router {
    let status_route = Router::new()
        .route("/api/status", get(status))
        .route_layer(axum::middleware::from_fn_with_state(
            api.clone(),
            require_session,
        ))
        .with_state(state.clone());
    Router::new()
        .route("/", get(page))
        .route("/metrics", get(prometheus))
        .route("/healthz", get(healthz))
        .with_state(state)
        .merge(status_route)
        .nest("/api/v1", lodestar_api::router(api))
}

async fn page() -> Response {
    (
        [
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Html(STATUS_PAGE),
    )
        .into_response()
}

async fn prometheus(State(s): State<AppState>) -> Response {
    let mut body = metrics::render(&s.monitor, s.dht().as_ref());
    if let Some(search) = &s.search {
        metrics::render_search(&mut body, search.index.num_docs(), &search.stats);
    }
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response()
}

/// Healthy while every source plugin is running.
async fn healthz(State(s): State<AppState>) -> Response {
    let down: Vec<&str> = s
        .monitor
        .sources()
        .iter()
        .filter(|src| !src.running)
        .map(|src| src.plugin)
        .collect();
    if down.is_empty() {
        (StatusCode::OK, "ok\n").into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("stopped: {}\n", down.join(", ")),
        )
            .into_response()
    }
}

async fn status(State(s): State<AppState>) -> Response {
    let reader = s.reader.clone();
    let now = unix_now();
    let hour = now.div_euclid(3600);
    // SQLite reads block; keep them off the async workers.
    let from_db = tokio::task::spawn_blocking(move || {
        let store = reader.lock().expect("reader lock");
        let cohorts = store.cohorts(hour - 23, hour + 1)?;
        let latest = store.latest(10)?;
        Ok::<_, lodestar_store::Error>((cohorts, latest))
    })
    .await;
    let (funnel, latest) = match from_db {
        Ok(Ok((cohorts, latest))) => {
            let stage = |st: Stage| -> i64 {
                cohorts
                    .iter()
                    .filter(|c| c.stage == st as u8)
                    .map(|c| c.count)
                    .sum()
            };
            let mut lost = serde_json::Map::new();
            for (loss, label) in LOSSES {
                let n: i64 = cohorts
                    .iter()
                    .filter(|c| c.stage == Stage::Lost as u8 && c.reason == loss as u8)
                    .map(|c| c.count)
                    .sum();
                lost.insert(label.into(), n.into());
            }
            let funnel = json!({
                "harvested": stage(Stage::Harvested),
                "peer_found": stage(Stage::PeerFound),
                "fetched": stage(Stage::Fetched),
                "stored": stage(Stage::Stored),
                "lost": lost,
            });
            let latest: Vec<Value> = latest
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "size": t.size,
                        "files": t.file_count,
                        "discovered_at": t.discovered_at,
                    })
                })
                .collect();
            (funnel, Value::from(latest))
        }
        _ => (Value::Null, Value::Null),
    };

    let m = &s.monitor;
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    let p = m.pool_stats();
    let mut failures = serde_json::Map::new();
    for (kind, label) in FAILURE_KINDS {
        failures.insert(label.into(), p.failure_count(kind).into());
    }
    let mut lost = serde_json::Map::new();
    for (loss, label) in LOSSES {
        lost.insert(label.into(), p.lost_count(loss).into());
    }
    let i = m.intake_stats();
    let w = m.writer_stats();
    let dht = s.dht().map(|d| {
        json!({
            "routing_nodes": d.routing_nodes,
            "outstanding": d.outstanding,
            "reply_ratio": d.reply_ratio(),
            "queries_out": d.queries_out,
            "responses_in": d.responses_in,
            "harvested": {
                "sample": d.harvested_sample,
                "get_peers": d.harvested_get_peers,
                "announce": d.harvested_announce,
            },
        })
    });
    let body = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "now": now,
        "started_at": m.started_at(),
        "torrents": m.torrents(),
        "sources": m.sources().iter().map(|src| json!({
            "id": src.id.get(),
            "plugin": src.plugin,
            "name": src.name,
            "running": src.running,
            "dropped": src.dropped,
        })).collect::<Vec<_>>(),
        "intake": {
            "received": get(&i.received),
            "known": get(&i.known),
            "new": get(&i.to_pool),
            "dropped": get(&i.dropped),
        },
        "fetch": {
            "pending": get(&p.pending),
            "active": get(&p.active),
            "lookups": get(&p.lookups),
            "fetched": get(&p.fetched),
            "failures": failures,
            "lost": lost,
        },
        "writer": {
            "stored": get(&w.stored),
            "rediscovered": get(&w.rediscovered),
            "blocked": get(&w.blocked),
            "invalid": get(&w.invalid),
            "errors": get(&w.errors),
            "last_commit": get(&w.last_commit),
        },
        "dht": dht,
        "search": s.search.as_ref().map(|x| json!({
            "docs": x.index.num_docs(),
            "watermark": x.stats.watermark.load(Ordering::Relaxed),
            "last_commit": x.stats.last_commit.load(Ordering::Relaxed),
            "errors": x.stats.errors.load(Ordering::Relaxed),
        })),
        "funnel_24h": funnel,
        "latest": latest,
    });
    axum::Json(body).into_response()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
