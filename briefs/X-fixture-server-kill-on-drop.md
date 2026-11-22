# X-fixture-server-kill-on-drop (runner brief, 2026-11-22Z)

Kind: helper (tests only). Dep X-fixture-updates-on (#1051) is merged. This comes from #1051's session, whose mutation runs left three child servers running. It is recorded in `docs/decisions/2026-11-22-runner-records-r11.md`.

## The gap
`crates/lodestar/tests/fixture_server/mod.rs` `Server` holds a `Child` (`lodestar serve --fixture`), but it has no `Drop`. A test that panics after `spawn()` and before `stop()` (a failed assert, or `ready()` timing out) leaves its child server running after the test binary exits. Its users are `tests/fixture.rs` and `tests/analytics_validation.rs`.

## What to do
1. Give `Server` a `Drop` that kills the child if it hasn't been reaped yet, then waits for it, so no zombie or orphan is left. `stop()` keeps its behaviour: SIGTERM, a clean exit asserted, and the log returned. It just mustn't double-kill.
   - `stop(mut self)` moves the log handle out of `self`. With `Drop`, that needs an `Option` (or `mem::take`).
   - Drop must never panic, including while a panic is already unwinding: ignore kill and wait errors there.
   - A hard kill (`Child::kill`, which is SIGKILL) is fine in Drop. Only `stop()` needs a clean shutdown.
2. Add a test that a panicking test leaves no process. For example:
   - spawn a server, record its pid, and panic inside `std::panic::catch_unwind`, or in a spawned thread that is joined;
   - then wait, with a deadline of at least 10 s and polling in a loop, for the pid to be gone. On Linux, check `/proc/<pid>` or `kill -0`; whichever you use, guard it with `cfg(unix)` as the helper already does with `kill`.
   - No fixed sleeps. The first start builds the fixture and can be slow, so the test may reuse a ready server, or skip `ready()` if a spawned child is enough.
3. Mutation check, by hand and not committed: remove the Drop body and see the new test fail.
4. Write the decisions file.

## Rules
- Tests only: no change outside the owns.
- No fixed sleeps. A wait polls a condition, with a deadline of at least 10 s.

## Owns
- `crates/lodestar/tests/fixture_server/mod.rs`
- `docs/decisions/2026-11-22-X-fixture-server-kill-on-drop.md`
- Put a new test file under `crates/lodestar/tests/` only if the test can't live in `fixture.rs`. If it needs `fixture.rs`, ask me first: `fixture.rs` is not in the owns.

## Branch and PR
- Branch `slice/X-fixture-server-kill-on-drop`, cut from `origin/main`. PR title "X-fixture-server-kill-on-drop: The fixture-server test helper stops its child when a test panics".
- Gates: `cargo fmt --check`, `cargo clippy -p lodestar --all-targets -- -D warnings`, and `cargo test -p lodestar --test fixture --test analytics_validation`, plus your new test.
- No step 0 is needed unless something here is wrong. Push once, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
