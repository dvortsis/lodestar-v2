//! `GET /openapi.json` (§9): the OpenAPI 3.1 description of `/api/v1`,
//! built from the typed DTOs in [`crate::dto`] and each module's
//! `#[utoipa::path]` handlers. It's open (it holds no data).
//!
//! `lodestar`'s `openapi` binary writes the same document to
//! `web/src/lib/api/openapi.json`, which the web client's types are
//! generated from; CI fails if the committed copy drifts.

use std::sync::OnceLock;

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use utoipa::openapi::path::Operation;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme};
use utoipa::openapi::{ContentBuilder, OpenApi as Doc, RefOr, ResponseBuilder, ServerBuilder};
use utoipa::{Modify, OpenApi};

use crate::ApiState;
use crate::auth::COOKIE;
use crate::dto::error::ErrorEnvelope;

/// What the binary nests the router under.
pub const PREFIX: &str = "/api/v1";
/// The session cookie's security scheme.
const SESSION: &str = "session";
/// Paths that need no session.
const OPEN: [&str; 5] = ["/auth", "/setup", "/login", "/logout", "/openapi.json"];

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Lodestar API",
        description = "Every node is a new discovery. The JSON API under `/api/v1` (architecture §9): \
            errors are always the `Error` envelope, and every path but the auth ones and this \
            document needs the session cookie.",
    ),
    paths(openapi_json),
    components(schemas(ErrorEnvelope)),
    tags(
        (name = "auth", description = "First-run setup, login and sessions (§12)."),
        (name = "search", description = "The query language and search (§8)."),
        (name = "torrents", description = "One torrent's details and files."),
        (name = "filters", description = "Filtering and safety (§20)."),
        (name = "classify", description = "Classification rules and corrections (§20.9)."),
        (name = "discover", description = "Newest, Rising and Recently resolved (§10.7)."),
        (name = "settings", description = "The runtime settings registry (§10.10, §11)."),
        (name = "crawler", description = "Stats, the crawler's controls, sources and the network check (§9, §10.9, §18.8)."),
        (name = "events", description = "The live server-sent event stream (§9)."),
        (name = "library", description = "Your marks, collections and saved searches (§7, §10.5, §10.8)."),
        (name = "meta", description = "This document."),
    ),
    modifiers(&Envelope)
)]
struct Base;

/// The whole document: the base plus one line per module, as
/// [`crate::router`] merges their routes.
pub fn document() -> Doc {
    let mut doc = Base::openapi();
    // Add a module, add one line.
    doc.merge(crate::auth::ApiDoc::openapi());
    doc.merge(crate::search::ApiDoc::openapi());
    doc.merge(crate::facets::ApiDoc::openapi());
    doc.merge(crate::suggest::ApiDoc::openapi());
    doc.merge(crate::torrents::ApiDoc::openapi());
    doc.merge(crate::filters::ApiDoc::openapi());
    doc.merge(crate::settings::ApiDoc::openapi());
    doc.merge(crate::ops::ApiDoc::openapi());
    doc.merge(crate::compare::ApiDoc::openapi());
    doc.merge(crate::discover::ApiDoc::openapi());
    doc.merge(crate::insights::ApiDoc::openapi());
    doc.merge(crate::events::ApiDoc::openapi());
    doc.merge(crate::keys::ApiDoc::openapi());
    doc.merge(crate::user_items::ApiDoc::openapi());
    doc.merge(crate::collections::ApiDoc::openapi());
    doc.merge(crate::saved::ApiDoc::openapi());
    doc.merge(crate::export::ApiDoc::openapi());
    doc.merge(crate::jobs::ApiDoc::openapi());
    finish(&mut doc);
    doc
}

/// The document as served and committed: pretty-printed, keys in a
/// stable order, a trailing newline.
pub fn json() -> String {
    let mut text = serde_json::to_string_pretty(&document()).expect("the document serializes");
    text.push('\n');
    text
}

/// The security scheme and the error response every operation shares.
struct Envelope;

impl Modify for Envelope {
    fn modify(&self, doc: &mut Doc) {
        let components = doc.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            SESSION,
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                COOKIE,
                "The session cookie `/setup`, `/login` and `/password` set. HttpOnly, SameSite=Strict.",
            ))),
        );
        components.responses.insert(
            "Error".to_string(),
            RefOr::T(
                ResponseBuilder::new()
                    .description(
                        "Any failure: the error envelope. `error.code` is stable; branch on it.",
                    )
                    .content(
                        "application/json",
                        ContentBuilder::new()
                            .schema(Some(RefOr::Ref(utoipa::openapi::Ref::from_schema_name(
                                "ErrorEnvelope",
                            ))))
                            .build(),
                    )
                    .build(),
            ),
        );
        doc.servers = Some(vec![ServerBuilder::new().url(PREFIX).build()]);
        // utoipa fills the license from Cargo's, which the workspace
        // doesn't set; an empty name isn't a license.
        doc.info.license = None;
    }
}

/// Once every module is merged: each operation's `default` response is
/// the error envelope, and every one outside [`OPEN`] needs a session.
fn finish(doc: &mut Doc) {
    for (path, item) in doc.paths.paths.iter_mut() {
        let open = OPEN.contains(&path.as_str());
        for op in operations(item) {
            op.responses.responses.insert(
                "default".to_string(),
                RefOr::Ref(utoipa::openapi::Ref::from_response_name("Error")),
            );
            op.security = Some(if open {
                Vec::new()
            } else {
                vec![SecurityRequirement::new::<_, _, &str>(SESSION, [])]
            });
        }
    }
}

fn operations(item: &mut utoipa::openapi::PathItem) -> impl Iterator<Item = &mut Operation> {
    [
        item.get.as_mut(),
        item.put.as_mut(),
        item.post.as_mut(),
        item.delete.as_mut(),
        item.options.as_mut(),
        item.head.as_mut(),
        item.patch.as_mut(),
        item.trace.as_mut(),
    ]
    .into_iter()
    .flatten()
}

pub fn routes() -> Router<ApiState> {
    Router::new().route("/openapi.json", get(openapi_json))
}

/// This document.
#[utoipa::path(
    operation_id = "getOpenApi",
    get,
    path = "/openapi.json",
    tag = "meta",
    responses((status = 200, description = "The OpenAPI 3.1 document.", content_type = "application/json", body = Object))
)]
pub(crate) async fn openapi_json() -> impl IntoResponse {
    static JSON: OnceLock<String> = OnceLock::new();
    (
        [(header::CONTENT_TYPE, "application/json")],
        JSON.get_or_init(json).as_str(),
    )
}
