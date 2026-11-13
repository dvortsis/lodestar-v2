# X-flake-companions-loopback (Opus)

Runner brief, 2026-11-13Z. Branch `slice/X-flake-companions-loopback` from current main. PR title `X-flake-companions-loopback: <short title>`.

## Problem
`crates/companions/tests/detect.rs` has tests that bind `127.0.0.1:0`, read the port, `drop` the listener, then assert nothing answers there
(around :362 and `nothing_on_a_loopback_port_is_not_found` at :388). Between the drop and the probe, another test in the same binary
(or another process on a busy CI runner, Windows especially) can be handed the same ephemeral port, so the probe finds something and the test fails.

## Do
1. Make every "nothing listens here" test race-free. Acceptable shapes, your call with the reason in the decisions file:
   - pick a closed port and confirm it is closed immediately before the assertion, retrying with a fresh port a bounded number of times if something answers (count retries; fail loudly when exhausted, never pass silently);
   - or hold the port in a way that is guaranteed to refuse connections for the test's duration on Linux, macOS and Windows (only if you can show it refuses on all three).
2. Look for the same bind-then-drop pattern elsewhere in `crates/companions/tests/` and fix it the same way; name any found outside your owns as a follow-up rather than editing them.
3. Do not add new skips or early returns that make a test pass without asserting. Never use fixed sleeps; any wait is a condition with a deadline of at least 10 s.
4. Decisions file `docs/decisions/2026-11-13-X-flake-companions-loopback.md` (README format).

## Owns
`crates/companions/tests/detect.rs`, any shared helper you add under `crates/companions/tests/`, the decisions file.

## Checks
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-companions` (or the crate's real name) run several times locally. No cloud or AWS calls. Follow `.claude/skills/steward/SKILL.md`. Push once per round; reply to the runner `ready <PR> <full 40-char sha>`.
