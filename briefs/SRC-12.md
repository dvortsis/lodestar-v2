# SRC-12: BEP 48 full scrape
**Model:** Opus. Core, untrusted multi-megabyte input, `/security-review`. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## Why
This is on the v0.6 sources gate's critical path (SRC-G).

## Read first
- `docs/plan/2026-11-01-v06-sources.md`: the row for §18.2 rank 16, §4.1 owned paths, §4.2 SRC-12, and §5 Q2 (record the ToS check for each tracker class).
- `docs/architecture.md` §18.1 (≤ 1 full scrape per tracker per day), §18.2, §18.4 (schedule windows, 01:00–07:00 by default).
- `crates/tracker-scrape/` (`http::parse_scrape` and its fuzz target) and the scrape scheduler in `crates/lodestar/src/services.rs` (read it; don't edit it).
- The SRC-7 plugin pattern: `crates/sources-plugins/README.md`, `src/prowlarr.rs` and `tests/prowlarr.rs`.

## Do
1. **The parser, `crates/tracker-scrape/src/full.rs`:** a streaming bencode parser for a full-scrape reply, the `files` dict with no `info_hash` in the request.
   - Caps: total bytes, entries, and key and value shape. A hash key must be exactly 20 bytes.
   - It emits keys in bounded batches, skips and counts malformed entries, and never buffers the whole reply.
2. **The plugin, `crates/sources-plugins/src/full_scrape.rs`:**
   - at most once per tracker per day, inside the schedule window;
   - never on a tracker whose torrents are `private_source`;
   - egress through SRC-5's factory;
   - keys are emitted as discoveries.
3. **Gates:**
   - a fuzz target `fuzz/fuzz_targets/full_scrape.rs`, with the `fuzz/Cargo.toml` bin;
   - property tests: it never panics and never exceeds its caps;
   - **a 50 MB fixture parsed with flat memory.** Generate it in the test; don't commit 50 MB. Measure peak growth and assert a bound;
   - the per-day cap test, on a fake clock with no sleeps;
   - the private-tracker refusal test.
4. **Decisions file:** `docs/decisions/2026-11-04-SRC-12.md`.

## Own
`crates/tracker-scrape/src/full.rs`, `crates/tracker-scrape/tests/full*.rs`, `crates/sources-plugins/src/full_scrape.rs`, `crates/sources-plugins/tests/full_scrape*.rs`, `fuzz/fuzz_targets/full_scrape.rs`, and your decisions file.

**Shared files:** one `mod` line each in `crates/tracker-scrape/src/lib.rs` and `crates/sources-plugins/src/lib.rs`, and the `fuzz/Cargo.toml` bin.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the fuzz target builds, and `/security-review` and a fresh self-review are done.

Branch `slice/SRC-12`. Open the PR titled "SRC-12: BEP 48 full scrape". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
