# B-search-meta: an approximate flag on suggestion counts, and a field catalogue endpoint
**Model:** Opus (search API and the query language's field set: correctness-critical). The runner reviews. Runner brief, 2026-11-01. Follow-up from #250.

## Why
- `SuggestItem.count` (`crates/api/src/dto/suggest.rs`) has no approximate flag, so the UI can never show "≈" for a suggestion count even when the count comes from a sampled or capped computation. Search results already carry exactness for their totals (`≈`, §8/§10.4); suggestions should follow the same rule.
- There's no field catalogue endpoint: the query Builder's field list is typed from schema enums in the web client, so a new field needs a web release to appear.

## Read first
- `docs/architecture.md` §8 (query language, fields, counts and `≈`), §10.4 (suggestions, the Builder), §3 (search latency budget).
- `crates/api/src/dto/suggest.rs`, the suggest handler, how search totals express exactness (find the existing `approximate`/`exact` field on search responses and reuse its name and meaning), the query language's field registry (in `crates/search` or the query parser crate).
- `crates/api/openapi.json` and its drift check.

## Do
1. **`SuggestItem.approximate: bool`** (optional in the schema, omitted or false when exact), set true exactly when the count is not exact under the same rule search totals use (sampling, a cap, a time budget). Field names, categories and "did you mean" keep no count, so no flag. Tests for an exact and an approximate count.
2. **`GET /search/fields`**: the field catalogue the query language accepts. For each field: its name, aliases, value type (text, number, size, date, enum…), the operators it accepts, enum values where finite, and whether it's facetable. It's derived from the parser's own registry (one source of truth, no second list), cached, cheap and stable. Respect content-safety: no field or enum value that would reveal hidden or adult categories while 18+ is off, if any such exist (check §20 and the B17 visibility rules); say in the decisions file what you found.
3. Regenerate `openapi.json` and `web/src/lib/api/schema.d.ts` with the repo's tooling, and add API goldens. **Don't change web feature code**; a later web slice will use both.
4. **Performance:** no measurable change to suggest latency (the flag comes from data already computed), and `/search/fields` is O(fields) from a static or cached table.

## Rules
- No fixed sleeps; condition waits ≥ 10 s. Don't run formatters over files you didn't change. Run long checks in the foreground.

## Own
`crates/api/src/dto/suggest.rs`, the suggest and new fields handlers and their routes, the search/query crates only where the flag and the registry export need it, `crates/api/openapi.json`, `web/src/lib/api/schema.d.ts` (regenerated), tests and goldens, `docs/decisions/2026-11-01-B-search-meta.md`.
**Don't edit** web feature code, the index format, `crates/dht`, store schema, or `vite.config.*`.

## Size
≲ 600 changed lines (goldens included).

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the openapi drift check, the search latency job if it exists locally (`bench/`), and in `web/` `npm run check`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
