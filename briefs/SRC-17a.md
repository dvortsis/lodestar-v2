# SRC-17a: API fields for provenance, verified and source (before SRC-17's UI)
**Model:** Opus (API contract; content and privacy rules). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05.

## Why
SRC-17 (the UI) found that `main` exposes nothing to render from: `TorrentDetail` (`crates/api/src/dto/torrents.rs`) has no sources, verified flag, provenance or claims; `SearchHit` has no verified or source field; `/facets` has no `source`, `verified` or `network` term facets. Only SRC-3's query terms (`verified:`, `source:`, `network:`) exist.

## Spec
Architecture §18.5, §18.6, §18.7, §18.8; `docs/plan/2026-11-01-v06-sources.md` §4 (SRC-3, SRC-17); SRC-1's provenance tables (`crates/store`), SRC-3's search fields.

## Do
1. `TorrentDetail`: a `provenance` object: the sources that found the item (DHT and each source plugin's display name, first-seen per source), `verified` (bool), and, when an item has both a claim and verified metadata, the differences (claimed vs verified size, name, file count; bounded, no full file lists).
2. `SearchHit`: `verified` (bool) and the primary source name.
3. `/facets`: `source`, `verified` and `network` term facets, wired to SRC-3's fields, with the same caps as the existing term facets.
4. **Privacy rules (§18.7):** a `private_source` item's source name and any claim from it are returned only to the admin session, never through API keys, Torznab, feeds, exports or any public route; add tests for each surface. Never expose a private indexer's name in a facet visible to a non-admin.
5. Regenerate `openapi.json` and `web/src/lib/api/schema.d.ts` with the repo's tooling; golden tests for the new fields (search, detail, facets goldens only).

## Own
`crates/api/src/dto/torrents.rs`, the search and facets DTO/handler files, `crates/api/src/torrents.rs`, `crates/api/src/search.rs`, `crates/api/src/facets.rs`, the store read helpers they need (read-only queries, no schema change), `openapi.json`, `schema.d.ts`, the search/detail/facets goldens under `crates/api/tests/golden/` (not the filter goldens RP-DEF owns), and `docs/decisions/2026-11-05-SRC-17a.md`.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the openapi drift check, `/security-review`. PR "SRC-17a: …" against `main` from `slice/SRC-17a`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
