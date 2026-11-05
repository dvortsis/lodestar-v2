# X-fuzz-cache-1105: a sync torrent-cache parse path, fuzzed directly, plus `Verified::claims`
**Model:** Opus (untrusted-input parser). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T11:10Z.

## Why
X-fuzz-1105 (#484, merged 6648e69f) added a `torrent_cache` fuzz target, but the plugin's body check runs inside the async fetch path, so the target reaches it only indirectly. SRC-13b (#479, merged 7692ea2f) is in. X-fuzz-1105 handed this on as its follow-up: split the torrent-cache parse into a sync function, fuzz it directly, and fuzz `Verified::claims`.

## Read first
`fuzz/README.md`, `fuzz/fuzz_targets/torrent_cache*.rs`, `crates/sources-plugins/src/torrent_cache.rs`, `crates/pipeline/src/torrent_cache.rs`, `crates/sources-plugins/tests/torrent_cache.rs`, `docs/decisions/2026-11-05-SRC-13b.md`, the X-fuzz-1105 decisions file, and the `Verified` type and its `claims` (grep `fn claims`).

## Do (branch `slice/X-fuzz-cache-1105` from `origin/main`)
1. Factor the body verification into a pure sync `verify_body(…)`, with no I/O and the same checks, caps and errors, and have the async path call it. No behaviour change. Prove it: the existing tests unchanged and passing, plus a test that the async and sync paths agree on the fuzz seeds.
2. Retarget the `torrent_cache` fuzz target at `verify_body`, and add a `verified_claims` target for `Verified::claims`. Add seeds, and register both in `fuzz/Cargo.toml` and the README.
3. Run each target for at least 30 minutes, or 1M execs, whichever comes first. Report the execs, coverage and any crash. If a target finds a crash, fix it here, with a regression test.

## Own
The `verify_body` split in `crates/sources-plugins/src/torrent_cache.rs` (parse only), `fuzz/fuzz_targets/{torrent_cache,verified_claims}.rs`, `fuzz/Cargo.toml`, `fuzz/README.md`, the seeds, `docs/decisions/2026-11-05-X-fuzz-cache-1105.md`, and your own `runner/slices.yaml` entry.
**Overlaps:** SRC-13c (running) may edit the plugin's `Config` in the same file. Keep your hunk to the parse functions, and whichever PR merges later brings `main` in.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo +nightly fuzz build` (or the repo's documented fuzz build). Then `/security-review`. Open the PR titled "X-fuzz-cache-1105: sync torrent-cache parse fuzzed directly, plus Verified::claims". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, head and fuzz numbers. Never merge. Push once per round.
