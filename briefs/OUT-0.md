# OUT-0 (Opus, /security-review): bitmagnet-format export, export_adult, exclusions

Runner brief, 2026-11-11 (UTC). Slice entry: `runner/slices.yaml` OUT-0. Plan: `docs/plan/2026-11-05-v07-m7.md` (Outputs section) and `docs/decisions/2026-11-05-P-v07.md`. Spec: architecture §18.2, §18.7, §18.9, §20.9. Use the steward skill; Auto mode.

## Do
- An export in bitmagnet's format (`crates/api/src/export.rs`, `crates/api/src/dto/export.rs`), streamed, not built in memory; match bitmagnet's real format as the plan describes, with a fixture taken from the plan or from the existing importer's tests (`crates/import`), never from the network.
- **Exclusions:** blocklisted, withheld and `private_source` items never export. `export_adult` decides adult items, default per the plan.
- **Prove the existing export formats also leave out `private_source`** (the slice notes require it): tests against each existing format. If one doesn't, report it to the runner before fixing outside your owns.
- Auth, rate and size limits like the API's other bulk endpoints; the export never includes secrets or settings.
- **Tests** in `crates/api/tests/export_bitmagnet.rs`: the format round-trips through the existing bitmagnet importer (if `crates/import` exposes it to tests without a `testing` feature; otherwise a golden file), each exclusion, adult on and off, large exports stream (bounded memory), hostile titles. No fixed sleeps.
- Decisions file `docs/decisions/2026-11-11-OUT-0.md`. Regenerate `openapi.json` and `schema.d.ts` with the repo's tooling if the route is in OpenAPI.

## Owns
`crates/api/src/export.rs`, `crates/api/src/dto/export.rs`, `crates/api/tests/export_bitmagnet.rs`, the router line in `crates/api/src/lib.rs` (shared one-line edit; TZ-0 adds one too), the decisions file, regenerated OpenAPI files. Anything else: ask the runner.

## Bar
- runner/standards.md; fmt, clippy `-D warnings`, tests of the crates you touch.
- Branch `slice/OUT-0`, PR `OUT-0: <title>`. About 600–1,000 lines.
- Self-review including /security-review; push once per round; reply "ready" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and full head SHA when ci-ok is green.
