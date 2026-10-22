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
