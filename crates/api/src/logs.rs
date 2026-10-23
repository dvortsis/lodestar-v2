//! `/logs` (architecture §12.1): admin session only. API keys and feed
//! tokens can't read logs. B-log1 serves `GET /logs/catalog`; B-log2 adds
//! the history, the live stream and the files here.
//!
//! The catalogue comes from `lodestar_log`: the events the binary
//! registered (with their switches and counters) and the state its logging
//! runtime last applied. Without a runtime (tests, tools) the kinds are as
//! the Private preset has them and no output is listed.

use axum::routing::get;
use axum::{Json, Router};
use lodestar_log::{Component, Kind, Level, Output};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth;
pub use crate::dto::logs::{
    LogCatalog, LogComponent, LogDataKind, LogEvent, LogKind, LogKindOutputs, LogLevel, LogOutput,
    LogOutputName, LogPresets, LogTemporary,
};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(catalog))]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    auth::protected(Router::new().route("/logs/catalog", get(catalog)))
}

fn level(l: Level) -> LogLevel {
    match l {
        Level::Error => LogLevel::Error,
        Level::Warn => LogLevel::Warn,
        Level::Info => LogLevel::Info,
        Level::Debug => LogLevel::Debug,
        Level::Trace => LogLevel::Trace,
    }
}

fn component(c: Component) -> LogComponent {
    match c {
        Component::System => LogComponent::System,
        Component::Dht => LogComponent::Dht,
        Component::Fetch => LogComponent::Fetch,
        Component::Pipeline => LogComponent::Pipeline,
        Component::Search => LogComponent::Search,
        Component::Api => LogComponent::Api,
        Component::Sources => LogComponent::Sources,
        Component::Filters => LogComponent::Filters,
        Component::Integrations => LogComponent::Integrations,
        Component::Workers => LogComponent::Workers,
        Component::Companions => LogComponent::Companions,
        Component::Auth => LogComponent::Auth,
        Component::Libraries => LogComponent::Libraries,
    }
}

fn kind(k: Kind) -> LogDataKind {
    match k {
        Kind::Hash => LogDataKind::Hash,
        Kind::Name => LogDataKind::Name,
        Kind::Path => LogDataKind::Path,
        Kind::Url => LogDataKind::Url,
        Kind::Query => LogDataKind::Query,
        Kind::Client => LogDataKind::Client,
        Kind::Peer => LogDataKind::Peer,
        Kind::Size => LogDataKind::Size,
        Kind::PeerActivity => LogDataKind::PeerActivity,
        Kind::FilterChange => LogDataKind::FilterChanges,
    }
}

fn output(o: Output) -> LogOutputName {
    match o {
        Output::Console => LogOutputName::Console,
        Output::File => LogOutputName::File,
        Output::Live => LogOutputName::Live,
        Output::EventLog => LogOutputName::EventLog,
    }
}

/// The catalogue as `lodestar_log` has it now.
pub fn current(now: i64) -> LogCatalog {
    let status = lodestar_log::status();
    let events = lodestar_log::events()
        .map(|e| LogEvent {
            component: component(e.component),
            current: e.is_on(),
            default_on: e.default_on,
            help_key: e.help_key(),
            id: e.id.to_string(),
            kinds: e.kinds().into_iter().map(kind).collect(),
            label_key: e.label_key(),
            level: level(e.level),
            opt_in: e.opt_in.map(kind),
            rate_per_hour: lodestar_log::rate_per_hour(e),
            required: e.required,
        })
        .collect();
    let kinds = Kind::ALL
        .into_iter()
        .map(|k| {
            let (outputs, until) = match status
                .as_ref()
                .and_then(|s| s.kinds.iter().find(|x| x.kind == k))
            {
                Some(s) => (s.outputs, s.until),
                None => {
                    let on = k.default_on();
                    ([on; 4], if on { -1 } else { 0 })
                }
            };
            LogKind {
                default_on: k.default_on(),
                identifying: k.identifying(),
                kind: kind(k),
                opt_in: k.opt_in(),
                outputs: LogKindOutputs {
                    console: outputs[0],
                    file: outputs[1],
                    live: outputs[2],
                    event_log: outputs[3],
                },
                until,
            }
        })
        .collect();
    let (presets, outputs) = match status {
        Some(s) => (
            LogPresets {
                data: s.data.to_string(),
                temporary: s.temporary.map(|(v, until)| LogTemporary {
                    until,
                    verbosity: v.to_string(),
                }),
                verbosity: s.verbosity.to_string(),
            },
            s.outputs
                .iter()
                .map(|o| LogOutput {
                    detail: o.detail.map(str::to_string),
                    enabled: o.enabled,
                    format: o.format.map(str::to_string),
                    level: level(o.level),
                    output: output(o.output),
                })
                .collect(),
        ),
        None => (
            LogPresets {
                data: "private".into(),
                temporary: None,
                verbosity: "normal".into(),
            },
            Vec::new(),
        ),
    };
    LogCatalog {
        events,
        kinds,
        now,
        outputs,
        presets,
    }
}

/// Every log event (id, component, level, label and help keys, default,
/// current, data kinds, required, rate per hour), every data kind with its
/// switch per output and its deadline, the presets and the outputs. Admin
/// session only.
#[utoipa::path(
    operation_id = "getLogCatalog",
    get,
    path = "/logs/catalog",
    tag = "logs",
    responses((status = 200, body = LogCatalog))
)]
pub(crate) async fn catalog() -> Json<LogCatalog> {
    Json(current(crate::unix_now()))
}
