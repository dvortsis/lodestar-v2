//! The HTTP API (architecture §9, §12): `/api/v1`, JSON, behind a
//! session cookie. M2 serves auth, query parsing, search and torrent
//! detail; facets, suggest, SSE and the rest arrive with their features.
//!
//! **Layout:** one module per domain, each with a `routes()` that
//! returns its `Router<ApiState>`, its session-protected half wrapped in
//! [`auth::protected`]. To add a domain, add a module, add one line to
//! [`router`].
//!
//! **Errors:** every failure is an [`ApiError`], which renders the one
//! envelope `{ "error": { "code", "message", "field"?, "start"?, "end"? } }`
//! (see [`error`]). Use the extractors in [`extract`] rather than axum's
//! so bad requests get it too.
//!
//! **CSRF:** the session cookie is `SameSite=Strict`, every request that
//! changes state must be `application/json` (which a cross-site form
//! can't send without a CORS preflight we never grant), and a present
//! `Origin` must match `Host`.

pub mod auth;
pub mod error;
pub mod extract;
pub mod filters;
mod search;
mod torrents;

use std::sync::{Arc, Mutex};

use axum::{Extension, Router};
use lodestar_search::SearchIndex;
use lodestar_store::Store;

pub use auth::{Auth, require_session};
pub use error::{ApiError, ApiResult};
pub use extract::ClientIp;
pub(crate) use search::unix_now;
pub use search::{MATCHED_FILES, MAX_LIMIT, MAX_WINDOW};

#[derive(Clone, Debug)]
pub struct ApiState {
    pub auth: Arc<Auth>,
    /// `None` when search is turned off.
    pub index: Option<SearchIndex>,
    /// A read-only connection for detail and enrichment.
    pub reader: Arc<Mutex<Store>>,
    /// `None` when the filters API isn't served (tests, tools).
    pub filters: Option<Arc<filters::FiltersApi>>,
}

/// The `/api/v1` routes. The server must be run with
/// `into_make_service_with_connect_info`.
pub fn router(state: ApiState) -> Router {
    Router::new()
        // Add a module, add one line.
        .merge(auth::routes())
        .merge(search::routes())
        .merge(torrents::routes())
        .merge(filters::routes())
        // What `auth::protected` checks sessions against.
        .layer(Extension(auth::SessionAuth(state.auth.clone())))
        // For open routes; `auth::protected` sets its own behind the
        // session guard, and this doesn't replace it.
        .method_not_allowed_fallback(error::wrong_method)
        .fallback(error::no_route)
        .with_state(state)
}
