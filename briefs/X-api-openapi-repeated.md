# X-api-openapi-repeated

Runner brief, 2026-11-21Z. Slice id `X-api-openapi-repeated`, branch `slice/X-api-openapi-repeated`, PR title `X-api-openapi-repeated: <title>`. The plan entry is in `runner/slices.yaml` (#1012). Follow-ups from #1010: read the "Follow-ups" and "Any `ast` on GET or HEAD" sections of `docs/decisions/2026-11-21-X-api-repeated-params.md` first.

## What
1. **OpenAPI 400 descriptions name `repeated_parameter`.** `/facets`, the logs routes (`crates/api/src/logs/params.rs`) and `/events` list their 400 codes one by one, and they don't name `repeated_parameter` yet (`/events` didn't name the old `bad_parameter` either). Make each list complete and accurate for what the route actually answers. Check every other route's 400 text against the X-api-repeated-params table too, and fix any that's wrong, staying within these owns.
2. **`GET /export` with `ast` or `hashes` in the query** gives the same named error as `/search` and `/facets`: `400 ast_needs_post` with `field: "ast"`, and `400 hashes_need_post` with `field: "hashes"` (the handler's existing codes, unreachable on GET today). Answer it before the extractor runs, repeated or not, on GET and HEAD, after the auth guard (a signed-out request is still `401`). Reuse #1010's layer rather than writing a second one. No new error code. Tests go in `crates/api/tests/export.rs`: single, repeated and bare, GET and HEAD, plus signed-out `401`.
3. **Only after CR-1c (#1014) has merged:** the `POST /ops/intensity` summary in `crates/api/src/ops.rs:416` ("Sets the crawler's intensity at once…") should say that with `dht.preset_slots` on, the slot count follows the new preset only after a restart, while running lookups follow it at once (see `docs/decisions/2026-11-21-CR-1c.md` once merged). If CR-1c hasn't merged by the time the rest is ready, skip this item and tell me; I'll tell you when to bring main in.
4. **Regenerate** `web/src/lib/api/openapi.json` and `schema.d.ts` with the repo's tooling (find it in `.github/workflows/` under "openapi.json drift"; never hand-edit them). Check `.github/scripts/licenses.py --check` too.
5. **A decisions file** `docs/decisions/2026-11-21-X-api-openapi-repeated.md`, heading with `(#<this PR>)`.

## Owns
- `crates/api/src/export.rs`, `crates/api/src/facets.rs`, `crates/api/src/logs/params.rs`, `crates/api/src/events.rs`, `crates/api/src/ops.rs` (item 3's doc comment only)
- the shared repeat-layer file from #1010, only if reusing it needs a change (name it to me first)
- `crates/api/tests/export.rs`, and any golden the change forces (name it)
- `web/src/lib/api/openapi.json`, `web/src/lib/api/schema.d.ts` (generated)
- the decisions file, and this slice's `slices.yaml` entry

Send me the exact list before you edit anything else.

## Bars
`runner/standards.md`. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-api` (and the workspace if shared code changes), and the web checks for the generated files (`npm run check` in `web/`). Push once per round, and send "ready <pr> <sha>" when CI is green.
