# X-flakes: Make the timing-sensitive tests deterministic
**Model:** Opus (concurrency, filesystem timing). **Branch:** `slice/X-flakes`.

**Why:** these tests fail intermittently on unrelated PRs and cost CI cycles and review trust.
- `crates/search/src/rebuild.rs` `rebuild::tests::a_rename_blocked_for_a_moment_is_retried`. On macOS, rebuild.rs:1025 panics with `Os { code: 66, DirectoryNotEmpty }`, then :1028 panics on the join (seen on PR #73).
- `crates/search/tests/rebuild.rs` `an_interrupted_rebuild_resumes_from_its_watermarks`. On Windows CI, line ~124 "the rebuild finishes". Locally, :283 "resumed, not started over" (left 1050, right 1000): the background rebuild takes a step before the test reads `progress()`.
- `crates/search/tests/index.rs:214` `the_live_indexer_follows_the_store_within_seconds`, "NRT budget", on Windows CI.
- `crates/peer-wire/tests/utp.rs` `fetches_plaintext_and_encrypted_over_utp`. On Windows CI, WSAENOBUFS (10055) on `bind`.

## Own
- Those test functions and their helpers.
- In production code: only a test hook (e.g. a `#[cfg(test)]` pause point or an injectable clock), if a test can't be made deterministic without one. Justify each in the PR.
- **Don't change** production behaviour, and don't edit `facets.rs`, `count.rs`, `lease.rs`, `schema.rs` or `doc.rs` (B6, #71, is open there).

## Rules
- **Root-cause each one.** Never skip, ignore, `#[cfg_attr(windows, ignore)]`, retry-loop or widen a timeout as the fix.
  - Replace sleeps and wall-clock budgets with explicit synchronisation: channels, barriers, pause hooks, or polling a condition with a generous deadline that only guards against hangs.
  - Where a test asserts a *performance* budget (the NRT one), split it: a deterministic correctness test, plus the budget measured where §3 budgets live (bench or a `--gate` job). Say how, in §16.
- **macOS DirectoryNotEmpty:** find out what holds or creates files in the directory during the rename or remove (Tantivy's lock or meta files, a lingering writer thread, Spotlight's `.DS_Store`?). Fix the test's setup, or the production retry if it's a real bug. If it's a production bug (a rename retried against a non-empty target), fix it properly and say so.
- **WSAENOBUFS on Windows:** often ephemeral-port or socket-buffer exhaustion from many sockets opened quickly. Check whether the test leaks sockets or binds too many, and fix that.
- Prove each fix: run the test in a loop (e.g. 200 iterations, `--test-threads` high) locally, before and after, and report the counts.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass after merging `origin/main`.
- The PR states, per test, the root cause, the fix, and the loop results before and after.
- PR title: "X-flakes: Make the timing-sensitive search and uTP tests deterministic".
