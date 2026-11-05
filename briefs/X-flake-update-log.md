# X-flake-update-log: isolate the update test's log capture from other tests
**Model:** Sonnet (test-only, follows an existing pattern). A fresh Opus reviewer follows. Runner brief, 2026-11-05T13:35Z.

## Why
`crates/lodestar/src/update/engine.rs:~2833` `the_log_over_an_install_names_events_and_never_urls` failed once in a full workspace run. It captured another test's `address.confirm_needed` log line, which RP-4 reported on 2026-11-05. Run alone, it passes. The capture listens to every event in the process, not only the test's own, so a parallel test in the same binary can leak into it. This is a flake in code no open PR touches, so the runner fixes it once (CLAUDE.md "Multi-session work").

## Do (branch `slice/X-flake-update-log` from `origin/main`)
1. **Read how the test captures logs.** Find the capture helper in `crates/lodestar` (grep `capture`, `log_events`, `LogCapture`, or the test's own subscriber). Then see how other tests isolate their capture, such as a per-test subscriber set with `tracing::subscriber::with_default` or a thread-local or scoped dispatcher, or a filter on the test's own span or task id.
2. **Make this test capture only its own events.**
   - Prefer the pattern the crate already uses elsewhere. If several tests share the same global capture helper and have the same weakness, fix the helper, and keep every caller's assertions unchanged.
   - Don't weaken the assertion: it must still prove the install's log names events and never URLs.
   - Don't add `#[serial]` or a global mutex unless no scoped option exists, and say why if you do.
3. **Show it holds.** Run the `lodestar` lib tests 5 times in full (`cargo test -p lodestar --lib`) with the default thread count, and report the runs in the PR. No fixed sleeps.
4. Add a short `docs/decisions/2026-11-05-X-flake-update-log.md`, plus your `runner/slices.yaml` entry and placement. Then run `cd tools/status-connector && npm test`.

## Own
The test and the capture helper in `crates/lodestar` (test code only), the decisions file, and your slices.yaml entry.
**Don't touch:** `crates/lodestar/src/services.rs` (LEGAL-2) or any non-test code.

## Done when
`cargo fmt --check`, `cargo clippy -p lodestar --all-targets -- -D warnings`, and `cargo test -p lodestar` all pass. Then open the PR "X-flake-update-log: Isolate the update test's log capture". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Push once per round.
