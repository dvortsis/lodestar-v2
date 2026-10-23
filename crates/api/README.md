# lodestar-api

The HTTP API under `/api/v1` (architecture §9, §12): auth, search, torrent detail, filters and classification. It's contract-first (`docs/build-plan.md` §1.3). The typed structs in `src/dto/` are the contract, `GET /api/v1/openapi.json` describes it, and the web client's types are generated from that document.

## Layout

- `src/dto/<domain>.rs` holds the request and response bodies for one domain. Each derives `Serialize`/`Deserialize` and utoipa's `ToSchema`.
- `src/<domain>.rs` (or `src/<domain>/`) holds the handlers. It has a `routes()` that `lib.rs::router` merges, and an `ApiDoc` (`#[derive(OpenApi)]`) listing those handlers, which `openapi::document` merges. Both take one line per module.
- `src/error.rs` defines `ApiError`, which renders the one error envelope (`dto::error::ErrorEnvelope`). In the document it's every operation's `default` response.
- `src/openapi.rs` builds the document and serves `/openapi.json`, which is open. It also declares the `session` cookie security scheme and applies it to every path except `/auth`, `/setup`, `/login`, `/logout` and `/openapi.json`.

## DTO rules

- **Names and nullability** follow `docs/metrics.md` §7 exactly.
  - Nullable there means always present and possibly `null`: `Option<T>` plus `#[schema(required = true)]`.
  - Absent means `#[serde(skip_serializing_if = "Option::is_none")]` plus `#[schema(nullable = false)]`.
  - Null is never 0: "never scraped" is `null`.
- **Response fields are in alphabetical order.** The handlers used to build `serde_json::Value`s, which serialize with sorted keys, and `tests/golden.rs` holds every endpoint to those bytes. Two exceptions:
  - `POST /filters/preview` has always sent `Assessment` in declaration order.
  - Nested values that came from `serde_json::to_value` (the query AST, an embedded `Assessment`) go through `dto::sorted`.
- **Types from other crates** that can't derive `ToSchema` get a schema-only mirror in the DTO file. One example is `search::query::Node`, mirrored as `dto::search::AstNode`. Each mirror has a round-trip test against the real type.

## Adding an endpoint

Later slices follow three steps, each a reviewable PR (or commit).

### 1. The typed DTO

Add the bodies to `src/dto/<domain>.rs`. Give the response an example, so the OpenAPI mock (W6) and the screens can start before the data exists:

```rust
/// `GET /facets`: term facets and histograms (docs/metrics.md §7.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "facets": { "type": [{ "value": "movie", "count": 12 }] },
    "histograms": {},
    "sample_rate": null,
    "sampled": false
}))]
pub struct FacetsResponse {
    pub facets: BTreeMap<String, Vec<FacetCount>>,
    pub histograms: BTreeMap<String, Vec<HistogramBin>>,
    /// Null when not sampled.
    #[schema(required = true)]
    pub sample_rate: Option<f64>,
    pub sampled: bool,
}
```

### 2. A 501 stub

The route and its documented responses land now. The handler returns `error::not_implemented()`, a 501 in the usual envelope (`"code": "not_implemented"`):

```rust
/// Facet counts for a query.
#[utoipa::path(
    get,
    path = "/facets",
    tag = "search",
    params(ParseQuery),
    responses((status = 200, body = FacetsResponse))
)]
pub(crate) async fn facets(Query(_): Query<ParseQuery>) -> ApiResult<Json<FacetsResponse>> {
    Err(crate::error::not_implemented())
}
```

Add the route to the module's `routes()` (behind `auth::protected` unless it's open, and add it to `openapi::OPEN` if it is), add the handler to the module's `ApiDoc` `paths(...)`, regenerate (see below), and commit `openapi.json` and `schema.d.ts`.

### 3. The handler

Replace the stub. The status codes, field names and nullability in the document must not change. If they have to, update `docs/metrics.md` §7 in the same PR. Add a golden case for the new endpoint to `tests/golden.rs`, record it with `LODESTAR_BLESS=1 cargo test -p lodestar-api --test golden`, and read the recorded file before committing it.

## Regenerating the client types

```sh
cargo run -p lodestar --bin openapi     # writes web/src/lib/api/openapi.json
cd web && npm run api:types             # writes src/lib/api/schema.d.ts
```

CI regenerates both and fails on any diff (`ci.yml` checks the JSON, `web.yml` checks the types), so commit both files with the change that caused them. Never merge them by hand; regenerate them after a merge instead.

## Confirmations, keys and proxy auth (B12)

These primitives live in `src/auth.rs`, `src/auth/` and `src/keys.rs`. Later slices use them rather than rolling their own.

- **Fresh password (§20.11).** A route whose change needs the password calls `auth::require_fresh_password(&auth, ip, body.password.as_deref())` on the blocking pool, or `auth::confirm_fresh_password(&auth, ip, body.password).await` from a handler.
  - No password (absent or `""`): `428 confirm_required` with `field: "password"`. No attempt is spent.
  - A wrong one: `401 bad_password` with `field: "password"`.
  - Too many attempts: `429 rate_limited`, from the same per-IP bucket as login.
  - Nothing is remembered between calls, and the password never reaches a log. Proxy-header sign-in doesn't skip it.
  - Check the request's other fields first, so a bad value is reported before the password is asked for. A 428 that carries more (filters' `assessment`) builds on `auth::confirm_required()`.
- **Routes keys may reach.** Wrap them in `auth::key_or_session(routes, keys::scope::TORZNAB)` (or `SEARCH`), or `auth::feed_token(routes)` for RSS. Everything else stays in `auth::protected`, which no key passes.
  - The handler reads `Extension<auth::Access>`: `Session`, `Key { id, scopes }` or `Feed { id, saved_search_id }`.
  - A feed handler answers 404 unless `access.reaches_feed(search_id)`.
  - Key requests are GET or HEAD only, and any `Set-Cookie` they'd get is dropped.
- **Scopes** are `keys::scope::{SEARCH, TORZNAB, FEEDS}`: bits stored with the key, never reused. A new scope takes a new bit and a `dto::keys::KeyScope` variant.
- **Client addresses.** `ClientIp` is the forwarded client behind a trusted proxy and the TCP peer otherwise. Use it for anything rate-limited per client. A test router mounted outside `router()` gets the guards' state from `auth::mount(routes, state)`.
