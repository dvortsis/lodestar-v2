# ENR-2a: move the shared bounded JSON reader out of archive_org.rs into json.rs (no behaviour change)
**Model:** Opus. Untrusted-input parser, `/security-review`. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## Why
The rule-pack plan's ENR-2b (the ThePornDB response parser) needs the same bounded, no-serde JSON reader that SRC-9 wrote inside `crates/sources-feeds/src/archive_org.rs`. ENR-2a moves it into a shared `json.rs`, so the audited reader is reused instead of copied.

## Read first
- `docs/plan/2026-11-04-v06-rulepacks.md`: the ENR-2a row, §4.2 and §4.3.
- `crates/sources-feeds/src/archive_org.rs`, about 470 lines of `impl Parser` with archive-specific `Draft` state, plus its tests and fuzz target, and `docs/decisions/2026-11-02-SRC-9.md`.

## Do
1. Extract the generic, bounded JSON reading (tokenizer, caps, depth tracking and the string, number and escape handling) into `crates/sources-feeds/src/json.rs`, with the same limits and error kinds. archive_org.rs keeps only its archive-specific mapping (`Draft`).
2. **No behaviour change.** SRC-9's tests and fuzz target pass unchanged, and the golden outputs are byte-identical.
3. Add unit tests for `json.rs`'s own API (caps, depth, malformed input), and a fuzz target `fuzz/fuzz_targets/json.rs` plus its bin.
4. Decisions file: `docs/decisions/2026-11-04-ENR-2a.md`.

## Own
`crates/sources-feeds/src/json.rs` (new), `crates/sources-feeds/src/archive_org.rs`, `crates/sources-feeds/tests/json*.rs`, `fuzz/fuzz_targets/json.rs`, and the `fuzz/Cargo.toml` bin. Also a one-line `mod` in `crates/sources-feeds/src/lib.rs`.

**Don't edit `rss.rs` or `torznab.rs`.** SRC-8b is changing rss.rs right now.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the fuzz targets build, and `/security-review` and a fresh self-review are done.

Branch `slice/ENR-2a`. The PR is titled "ENR-2a: Shared bounded JSON reader moved into json.rs". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
