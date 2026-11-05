# SRC-13: torrent-cache enrichment (hash-checked .torrent fetch, never for private_source)
**Model:** Opus (untrusted bytes; privacy). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05.

## Spec
`docs/plan/2026-11-01-v06-sources.md` §4 "SRC-13" and §5 Q3 and Q8; architecture §18.2 (enrichment #3), §18.5, §18.7; owner-decisions §2.37's Q3 answer ("off by default, External consent, a user-editable list that starts empty with suggestions shown, never for `private_source`, only for user-interest and unverified items"). Read SRC-7's plugin README (`crates/sources-plugins/README.md`), SRC-5's egress client and SRC-4's consent gate first, and copy their patterns.

## Do
- Fetch a `.torrent` by info hash from the configured caches, only through the plugin egress client (SRC-5), with per-host rate limits and caps on bytes and time.
- Bytes go through `crates/metainfo` (fuzzed). Use the info dict only if its SHA-1 (or v2 hash) equals the key (§5 Q8); then it counts as verified metadata, provenance "torrent cache", and still passes the full filter pipeline before storage (SRC-2's path).
- **Never** query a cache for a `private_source` hash, at any layer (a test proves the request is never built). Only user-interest and unverified items are eligible.
- Off by default; External consent (SRC-4); the cache list is user-editable and starts empty, with suggestions shown but not enabled.
- No live network in tests: fixtures only, with a fake cache server. Tests: hash mismatch refused; oversize and slow responses capped; private_source never sent; consent gate enforced; filter pipeline applied (a blocklisted name in a fetched torrent is dropped); no fixed sleeps.

## Own
`crates/sources-plugins/src/torrent_cache.rs`, `crates/pipeline/src/torrent_cache.rs` (new), their tests and fixtures, one `mod` line each in `crates/sources-plugins/src/lib.rs` and `crates/pipeline/src/lib.rs`, and `docs/decisions/2026-11-05-SRC-13.md`. `enrich.rs` stays untouched.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `/security-review`. PR "SRC-13: …" against `main` from `slice/SRC-13`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and head via the claude-code-remote `send_message` tool. Never merge. Push once per round.
