# SRC-11: download clients as a source
**Model:** Opus. Core, untrusted input, `/security-review`. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## Why
This is on the v0.6 sources gate's critical path (SRC-G), and SRC-11b's webhooks build on it.

## Read first
- `docs/plan/2026-11-01-v06-sources.md`: the row for §18.2 rank 17, §4.1 owned paths, §4.2 SRC-11, and §5 Q4 (download clients poll at ≥ 1 min) and Q6.
- `docs/architecture.md` §18.2, §18.7 (`private_source`).
- `crates/integrations/src/clients/` (today it only adds torrents), `crates/integrations/src/net.rs` (SRC-5's egress).
- `crates/sources-plugins/README.md` and `src/prowlarr.rs` with `tests/prowlarr.rs` (the plugin pattern), and `src/nyaa.rs`.
- The decisions files `docs/decisions/2026-11-02-SRC-{2,4,5,7}.md`.

## Do
1. **The `DownloadClient` trait** gains `list` (hash, name, size, state) and `export` (the `.torrent` bytes). Implement both for qBittorrent, Transmission and Deluge. rTorrent and aria2 return "unsupported".
2. **Client JSON is untrusted.** Parse it bounded (bytes, items, string lengths), skip and count malformed entries, and add a fuzz target `fuzz/fuzz_targets/client_list.rs` (plus the `[[bin]]` in `fuzz/Cargo.toml`).
3. **Exported bytes** go through `crates/metainfo` (fuzzed). Use the info dict only if its hash matches the listed key.
4. **The `sources-plugins/src/clients.rs` plugin:**
   - every find is `private_source` and never enters DHT `get_peers`, PEX or public announce (§18.7);
   - Account consent, reusing the existing settings secrets;
   - poll at ≥ 1 min;
   - egress through SRC-5's factory.
5. **Fixtures and tests:**
   - recorded fixtures per client (scrubbed; say if they're hand-written);
   - contract tests in SRC-7's pattern: no network, no fixed sleeps;
   - an isolation test that a find is private.
6. **Decisions file:** `docs/decisions/2026-11-04-SRC-11.md`.

## Own
`crates/integrations/src/clients/{mod,qbittorrent,transmission,deluge}.rs`, `crates/sources-plugins/src/clients.rs`, `crates/sources-plugins/tests/clients*.rs`, `crates/sources-plugins/fixtures/clients/**`, `fuzz/fuzz_targets/client_list.rs`, and your decisions file.

**Shared files:** one `pub mod` line in `crates/sources-plugins/src/lib.rs`, and the `fuzz/Cargo.toml` bin. Don't wire anything into `services.rs` or the pipeline.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the fuzz target builds, and `/security-review` and a fresh self-review are done.

Branch `slice/SRC-11`. Open the PR titled "SRC-11: Download clients as a source". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
