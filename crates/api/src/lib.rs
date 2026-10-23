//! The HTTP API (architecture §9, §12): `/api/v1`, JSON, behind a
//! session cookie. M2 serves auth, query parsing, search and torrent
//! detail; facets, suggest, SSE and the rest arrive with their features.
//!
//! **Layout:** one module per domain, each with a `routes()` that
//! returns its `Router<ApiState>`, its session-protected half wrapped in
//! [`auth::protected`]. To add a domain, add a module, add one line to
//! [`router`].
//!
//! **Contract:** handlers take and return the typed DTOs in [`dto`],
//! and each module lists its handlers in an `ApiDoc` that [`openapi`]
//! merges into `/openapi.json` (see `README.md` for the pattern new
//! endpoints follow).
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
mod collections;
mod compare;
mod discover;
pub mod dto;
pub mod error;
pub mod events;
pub mod extract;
mod facets;
pub mod filters;
pub mod insights;
pub mod keys;
pub mod openapi;
pub mod ops;
mod saved;
mod search;
pub mod settings;
pub mod system;
mod torrents;
mod user_items;

use std::sync::{Arc, Mutex};

use axum::{Extension, Router};
use lodestar_search::SearchHandle;
use lodestar_store::Store;

pub use auth::{Auth, require_session};
pub use error::{ApiError, ApiResult};
pub use extract::ClientIp;
pub(crate) use search::unix_now;
pub use search::{AdultVisibility, MATCHED_FILES, MAX_LIMIT, MAX_WINDOW};

#[derive(Clone, Debug)]
pub struct ApiState {
    pub auth: Arc<Auth>,
    /// `None` when search is turned off. Searches take its index once per
    /// request, so they follow an index swap (§8).
    pub index: Option<SearchHandle>,
    /// A read-only connection for detail and enrichment.
    pub reader: Arc<Mutex<Store>>,
    /// `None` when the filters API isn't served (tests, tools).
    pub filters: Option<Arc<filters::FiltersApi>>,
    /// `None` when the settings registry isn't served (tests, tools).
    pub settings: Option<Arc<dyn settings::SettingsApi>>,
    /// `None` when no pipeline runs (tests, tools).
    pub ops: Option<Arc<dyn ops::OpsApi>>,
    /// A read-write connection for user data: saved searches, user
    /// items, collections and API keys (§7, §12). `None` when not
    /// served (tests, tools).
    pub user_data: Option<Arc<Mutex<Store>>>,
    /// "Show adult content" (§20.9), followed live; hidden by default.
    pub adult: AdultVisibility,
    /// `None` when Insights isn't served (tests, tools).
    pub insights: Option<Arc<dyn insights::InsightsSource>>,
}

/// The `/api/v1` routes. The server must be run with
/// `into_make_service_with_connect_info`.
pub fn router(state: ApiState) -> Router {
    Router::new()
        // Add a module, add one line.
        .merge(auth::routes())
        .merge(search::routes())
        .merge(facets::routes())
        .merge(torrents::routes())
        .merge(filters::routes())
        .merge(settings::routes())
        .merge(ops::routes())
        .merge(compare::routes())
        .merge(discover::routes())
        .merge(insights::routes())
        .merge(events::routes())
        .merge(keys::routes())
        .merge(user_items::routes())
        .merge(collections::routes())
        .merge(saved::routes())
        .merge(system::routes())
        .merge(openapi::routes())
        // What `auth::protected` checks sessions against.
        .layer(Extension(auth::SessionAuth::of(&state)))
        // For open routes; `auth::protected` sets its own behind the
        // session guard, and this doesn't replace it.
        .method_not_allowed_fallback(error::wrong_method)
        .fallback(error::no_route)
        .with_state(state)
}
