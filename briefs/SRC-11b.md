# SRC-11b: Sonarr/Radarr webhooks (inbound, token-authenticated, size-capped)
**Model:** Opus (a new inbound endpoint parsing untrusted JSON). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05.

## Spec
`docs/plan/2026-11-01-v06-sources.md` §4 "SRC-11b" and §5 Q9 (accepted: a per-integration token, constant-time compare, size caps; may slip to v0.7 without blocking the tag); architecture §18.2 rank 17 (download clients, Sonarr/Radarr webhooks), §18.7, §12 (security). SRC-11 (`crates/sources-plugins/src/clients.rs`) is the sibling pattern: every find is `private_source`.

## Do
- `POST` webhook routes for Sonarr and Radarr "Grab"/"Download" events. Each integration has its own random token (generated server-side, shown once, stored hashed), compared in constant time; no unauthenticated route; a missing/wrong token is a uniform 401 with no detail.
- Body size capped before parsing; bounded JSON parsing using the repo's bounded reader (`crates/sources-feeds/src/json.rs` pattern), never an unbounded parse; caps on fields; malformed bodies counted and dropped.
- Map an event to keys + claims (info hash, title, size, the client's category) as `private_source` finds, through the same filter-before-storage path SRC-2 set; nothing from them reaches Torznab, feeds, exports or a public lookup.
- Rate-limit per integration; log no token and no body.
- Tests: wrong/missing token, constant-time compare (a test that it's used, not a timing test), oversize body, malformed JSON, fuzz target for the payload parser, private_source never leaks to each output surface, no fixed sleeps. Golden API tests and the regenerated `openapi.json` / `schema.d.ts`.

## Own
`crates/api/src/webhooks.rs`, `crates/sources-plugins/src/arr.rs`, their tests and fixtures, a fuzz target under `fuzz/` (one `[[bin]]` and a README row), one `mod` line each where needed, the route registration line, `openapi.json`, `schema.d.ts`, the webhook goldens under `crates/api/tests/golden/`, and `docs/decisions/2026-11-05-SRC-11b.md`. Don't touch files SRC-17a owns (`crates/api/src/dto/torrents.rs`, `search.rs`, `facets.rs`, `torrents.rs`); if both of you regenerate `openapi.json`, the later PR brings `main` in and regenerates.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the openapi drift check, a short fuzz run (report runs), `/security-review`. PR "SRC-11b: …" against `main` from `slice/SRC-11b`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via the claude-code-remote `send_message` tool. Never merge. Push once per round.
