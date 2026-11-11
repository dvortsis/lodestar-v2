# TZ-0 (Opus, /security-review): Torznab server

Runner brief, 2026-11-11 (UTC). Slice entry: `runner/slices.yaml` TZ-0. Plan: `docs/plan/2026-11-05-v07-m7.md` (Torznab section) and `docs/decisions/2026-11-05-P-v07.md`. Spec: architecture §18.1, §18.7, §18.9, §20.9 and §12. Use the steward skill; Auto mode.

## Do
- A Torznab endpoint in `crates/api/src/torznab.rs` (plus `crates/api/src/torznab/**`):
  - `t=caps`, and the search modes the plan lists (search, tvsearch, movie, music, book as planned), with parameter parsing that treats every query parameter as untrusted (length caps, numeric bounds, unknown parameters ignored or refused as the plan says);
  - a streaming-safe XML writer that escapes every value (names, file names, categories) and never emits invalid XML for hostile titles (control characters, invalid UTF-8 already replaced, `]]>`);
  - the category map;
  - **exclusions**: blocklisted, withheld and `private_source` items never appear (§18.7, §18.9, §20.9); `torznab_adult` decides whether adult items appear, default per the plan.
- **Auth:** `KeyScope::Torznab` exists (B12). Only that scope's key, by the same constant-time path the API uses; the key never appears in logs or URLs it writes back. A wrong or missing key gets the Torznab error document, not a full API error.
- Use the existing search query path; don't fork ranking or counts. If something you need isn't exposed, ask the runner before editing `crates/search` or other API files.
- **Tests** in `crates/api/tests/torznab.rs`: caps, each mode, paging (`offset`, `limit` capped), each exclusion, adult on and off, hostile titles produce well-formed XML (parse the output back), auth refusals, and a golden caps document. No fixed sleeps.
- Decisions file `docs/decisions/2026-11-11-TZ-0.md`; OpenAPI only if the plan says Torznab is described there (regenerate `openapi.json` and `schema.d.ts` with the repo's tooling, never by hand).

## Owns
`crates/api/src/torznab.rs`, `crates/api/src/torznab/**`, `crates/api/tests/torznab.rs`, the router line in `crates/api/src/lib.rs` (shared one-line edit; OUT-0 adds one too), the decisions file, and regenerated OpenAPI files if needed.

## Bar
- runner/standards.md; fmt, clippy `-D warnings`, `cargo test -p lodestar-api` (or the crate's name) and the workspace tests you touch.
- Branch `slice/TZ-0`, PR `TZ-0: <title>`. About 900–1,400 lines; if it will pass ~1,800, tell the runner and propose a split before building on.
- Self-review including /security-review; push once per round; reply "ready" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and full head SHA when ci-ok is green.
