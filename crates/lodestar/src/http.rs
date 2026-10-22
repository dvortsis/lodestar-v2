//! The HTTP surface: the API, the status page (`/status`) and its JSON,
//! Prometheus metrics, a health check, and the embedded UI for every other
//! path (see `web.rs`).

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
    /// Fetch outcomes per transport (TCP, uTP).
    pub fetch: Arc<lodestar_peer_wire::FetchStats>,
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

/// The pages and health/metrics endpoints are open (the pages ask for a
/// login themselves; metrics carry no torrent names). Status and the API
/// need a session. The UI is the fallback, so it never shadows a route.
pub fn router(state: AppState, api: ApiState) -> Router {
    let status_route = Router::new()
        .route("/api/status", get(status))
        .route_layer(axum::middleware::from_fn_with_state(
            api.clone(),
            require_session,
        ))
        .with_state(state.clone());
    Router::new()
        .route("/status", get(page))
        .route("/metrics", get(prometheus))
        .route("/healthz", get(healthz))
        .with_state(state)
        .merge(status_route)
        .nest("/api/v1", lodestar_api::router(api))
        .fallback(crate::web::serve)
}

/// The status page keeps its own policy: it is one file with its script
/// and styles inline, and renders peer-supplied names as text only.
pub const STATUS_CSP: &str = "default-src 'self'; style-src 'unsafe-inline'; \
    script-src 'unsafe-inline'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; \
    form-action 'self'";

async fn page() -> Response {
    (
        [
            (header::CONTENT_SECURITY_POLICY, STATUS_CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-cache"),
            (
                header::HeaderName::from_static("permissions-policy"),
                crate::web::PERMISSIONS,
            ),
        ],
        Html(STATUS_PAGE),
    )
        .into_response()
}

async fn prometheus(State(s): State<AppState>) -> Response {
    let mut body = metrics::render(&s.monitor, s.dht().as_ref(), &s.fetch);
    if let Some(search) = &s.search {
        metrics::render_search(&mut body, search.index.num_docs(), &search.stats);
    }
    if let (Some(health), Some(hot)) = (s.monitor.health(), s.monitor.hot()) {
        let stored = s
            .monitor
            .writer_stats()
            .swarm_updates
            .load(Ordering::Relaxed);
        metrics::render_scrape(&mut body, health, hot, stored);
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
        "swarm": s.monitor.health().map(|h| {
            let st = h.stats();
            let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
            json!({
                "rounds": get(&st.rounds),
                "alive": get(&st.alive),
                "dead": get(&st.dead),
                "unreached": get(&st.unreached),
                "suspect": get(&st.suspect),
                "dht_requested": get(&st.dht_requested),
                "dht_estimates": get(&st.dht_estimates),
                "hints_dropped": get(&st.hints_dropped),
                "private_skipped": get(&st.private_skipped),
                "trackers": h.trackers().iter().map(|t| {
                    let mut v = json!({
                        "url": t.url,
                        "health": metrics::health_name(t.health),
                        "failures": t.failures,
                        "last_ok_at": t.last_ok_at,
                    });
                    for (code, label) in metrics::SCRAPE_OUTCOMES.iter().enumerate() {
                        v[*label] = json!(t.rounds[code]);
                    }
                    v
                }).collect::<Vec<_>>(),
            })
        }),
    });
    axum::Json(body).into_response()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::STATUS_PAGE;

    /// The page reads errors through the API's envelope
    /// (`{ "error": { "message", "start", "end" } }`), not as a string.
    #[test]
    fn the_status_page_reads_the_error_envelope() {
        for read in [
            "r.data.error?.message",
            "err.message",
            "err.start",
            "err.end",
        ] {
            assert!(STATUS_PAGE.contains(read), "status.html should read {read}");
        }
        for stale in ["r.data.error ||", "data.error +", "data.start", "data.end"] {
            assert!(
                !STATUS_PAGE.contains(stale),
                "status.html still reads {stale}"
            );
        }
    }
}
