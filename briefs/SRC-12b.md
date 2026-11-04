# SRC-12b: plugin kit `fetch_stream` with a per-plugin timeout, and a redacting `Request` Debug
**Model:** Opus. Core: the plugin HTTP path, security-relevant. `/security-review`. Runner brief, 2026-11-04.

## Why
SRC-12 (#413, merged) fails closed on full-scrape replies over 16 MiB or slower than the 30 s egress timeout, which covers the largest public trackers. Separately, `crates/sources/src/http.rs` `Request` derives Debug, which would print Cookie and Authorization headers and login bodies if it were ever logged (#414's review).

## Read first
- `crates/sources/src/http.rs` and `crates/sources/src/plugin.rs` (the `Kit`).
- `crates/sources-plugins/src/full_scrape.rs` and `docs/decisions/2026-11-04-SRC-12.md`.
- SRC-5's egress factory (`crates/integrations/src/net.rs`, `docs/decisions/2026-11-02-SRC-5.md`).

## Do
1. Add `fetch_stream` (or equivalent) to the kit: a streaming body with a per-plugin timeout and byte cap taken from the manifest, a hard ceiling, and the same egress, SSRF and kill-switch rules as `poll`. Hand chunks to the caller, never buffering the whole body.
2. Switch `full_scrape.rs` to it, with its own limits (for example 256 MiB and 10 min, recorded in the decisions file). This is the one shared edit outside your paths; list it in the PR.
3. Give `Request` a hand-written Debug that redacts Cookie, Authorization and any body.
4. Tests:
   - a large streamed fixture with flat memory;
   - a slow stream within the timeout succeeds;
   - over the timeout or cap fails closed;
   - the Debug output redacts.

   No fixed sleeps; use a paused clock.
5. Decisions file: `docs/decisions/2026-11-04-SRC-12b.md`.

## Own
`crates/sources/src/http.rs`, `crates/sources/src/plugin.rs`, their tests, and your decisions file. The listed shared edit is `full_scrape.rs`.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, and `/security-review` and a fresh self-review are done.

Branch `slice/SRC-12b`. The PR is titled "SRC-12b: Plugin kit fetch_stream with a per-plugin timeout". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
