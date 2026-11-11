# X-flake-portable-restart-2: make the portable restart wait robust (Opus, core)

**Why:** the #662 review (X-flake-portable-restart), `reviews/rv-662-81fe7bb3.md`. The portable restart's PowerShell relaunch, in `crates/lodestar/src/system/restart.rs`, waits for the old process to exit before starting the new one. Three weak spots remain.

## Build
1. **A slow PowerShell start counts toward the 30 s.** Write a "started" marker the moment the relaunch script begins, before it waits. The parent and the test then measure the 30 s from that marker, not from the spawn.
2. **Under Constrained Language Mode, `$old.WaitForExit()` is probably blocked,** so the wait is silently skipped.
   - Use `Wait-Process -InputObject $old -Timeout <n>`, or by Id, which works in Constrained Language Mode.
   - Keep the timeout bounded, with the same 30 s or the existing constant.
3. **When the wait is skipped or times out,** write one plain stderr line saying so, then carry on as now.
   - It must never include the relaunch token.
   - `LODESTAR_RELAUNCH_TOKEN` stays out of anything spawned, as PK-win-restart's review requires.
4. **Tests.** Extend `crates/lodestar/tests/windows_service.rs` (cfg(windows)) to cover the started marker and the skip line. Use condition waits with deadlines of at least 10 s, never fixed sleeps. Add unit tests for the script text: it uses `Wait-Process`, and it writes the marker first. Show that each test fails if its change is reverted.
5. **Decisions file:** `docs/decisions/2026-11-11-X-flake-portable-restart-2.md`.
   - Record the CLM reasoning and the bounded wait.
   - Note that the Windows CI job takes about 30 s longer.

## Owns
- `crates/lodestar/src/system/restart.rs`
- `crates/lodestar/tests/windows_service.rs`
- its decisions file

## Not owned
PK-win-portable-upd (#659) is in review. Its owns include `services.rs`, `windows/mod.rs`, `main.rs`, `cli/util.rs`, `check.rs`, `update/windows/portable.rs` and `tests/update_windows_portable.rs`. Don't touch those. If you need one, ask the runner.

## Checks
`cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`. Run `cargo test -p lodestar` for the tests you can run on Linux. CI's windows-latest job runs the rest; it must be green.

Branch `slice/X-flake-portable-restart-2`; PR title "X-flake-portable-restart-2: <title>". Follow the steward skill.
