# SRC-11c: wire the Sonarr/Radarr webhooks into the host
**Model:** Opus (auth-adjacent; untrusted request bodies). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T10:55Z. Before SRC-G.

## Why
SRC-11b (#459) built `POST /webhooks/{integration}` and `ArrWebhooks`, but nothing in `crates/lodestar` registers them or wires a sink, so an authenticated call answers `503 webhooks_unavailable`. SRC-13b (#479) has merged and frees the host files.

## Read first
- `docs/decisions/2026-11-05-SRC-11b.md`, especially "Wiring (SRC-14 or the host slice)" (line ~42): build `ArrWebhooks`, register it with the pipeline, and add `Webhooks::new(sink).layer()` to the API router, the sink mapping `webhooks::Integration` → `arr::App` and `arr::Delivery` → `webhooks::Delivered`.
- `docs/decisions/2026-11-05-SRC-13b.md` (how SRC-13b wired the torrent-cache plugin into `services.rs`/`main.rs`, the claims path and `mark_adult_found`), `crates/lodestar/src/{services.rs,main.rs,http.rs}`, `crates/api/src/webhooks*`, `crates/sources-plugins/src/arr*` (or wherever `ArrWebhooks` lives).

## Do (branch `slice/SRC-11c` from `origin/main`)
1. Build and register `ArrWebhooks` with the pipeline, and wire its sink to the claims path (the same path other sources use, so content safety, the always-adult mask and private-tracker guards apply unchanged).
2. Add the `Webhooks` layer to the API router. Every SRC-11b contract stays: one `401 login_required` for every auth failure, body caps before and while reading, per-integration rate, per-address failure bucket, `200 {"dropped": true}` for real events dropped, `503 source_off` only for the Test call.
3. Webhook routes stay **ungated by the Legal Notice** (like API keys and feed tokens) — LEGAL-1 (#472) is adding that gate; check its exemption list covers `/webhooks/*` and coordinate through the runner if not.
4. An end-to-end test in `crates/lodestar/tests/`: a token made via the API, a Sonarr grab payload POSTed, the item reaching the claims path; a wrong token never reaching the sink; source off → 200 dropped for a real event and 503 for Test. No fixed sleeps (wait on a condition with ≥ 10 s deadline).
5. Docs: the webhooks section of the user guide's sources chapter, per the `user-docs` skill (how to add the connection in Sonarr/Radarr, safe choices), and `docs/decisions/2026-11-05-SRC-11c.md`.
6. Add your own `runner/slices.yaml` entry (`{id: SRC-11c, …}` with deps [SRC-11b, SRC-13b], owns, model opus) and a placement if none covers it; run `cd tools/status-connector && npm test`.

## Own
The wiring lines in `crates/lodestar/src/{services.rs,main.rs,http.rs}`, `crates/lodestar/tests/webhooks_e2e.rs` (new), the guide section, the decisions file, your slices.yaml entry.
**Overlaps:** #472 (LEGAL-1) edits `crates/lodestar/src/http.rs` and tests; X-source-adult-2 adds one field in `services.rs`; SRC-13c edits `crates/api` ops and `pipeline/control.rs`. Keep host hunks minimal; whichever merges later brings `main` in.

## Done when
`cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`. `/security-review` on the diff. PR against `main` titled "SRC-11c: Sonarr/Radarr webhooks wired into the host", label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head via the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
