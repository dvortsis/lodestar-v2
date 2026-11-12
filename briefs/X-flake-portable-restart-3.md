# X-flake-portable-restart-3: the relaunched copy's 30 s wait times out on a loaded runner (Opus, core)

**Why:** `test (windows-latest)` failed on #717 (RX-0, head 3557c1f2; run job 130967136165, 2026-11-12T04:57Z). The failing test is `a_portable_restart_serves_again_on_the_same_port` (`crates/lodestar/tests/windows_service.rs:652`, "no new copy served"). RX-0 doesn't touch `crates/lodestar`, so this belongs to the portable restart path that X-flake-portable-restart-2 (abf890e1) last hardened.

## What the log shows
- **The old process stopped cleanly:**
  - `app.stopped_cleanly`, `app.stop clean=true` and `app.relaunch`, all at 04:56:10.213Z;
  - the test then saw it exit with 80, because the assertion before :652 passed.
- **The new copy printed `WATCH_TIMED_OUT` and `predecessor_refusal`, then exited:**
  - "Lodestar's wait for the copy it replaces to exit timed out."
  - "…still running 30 s after it shut down…"
- **The sibling test passed,** in the same binary and at the same moment: `a_portable_restart_starts_a_new_copy`, on any port.

## Build
1. **Find the root cause first.** Don't raise the bound blindly. Candidates to check:
   - **The watch script's first cmdlets are slow.** `Get-Process` / `Wait-Process` module autoload on a cold, Defender-scanned runner. Rust measures 30 s from `started` (`await_watch`), which the script prints before loading anything.
   - **`Wait-Process -Id` misses the exit, or the PID check races.** Look at the start-time compare, and at the old process's PID being reused.
   - **The old process really lived more than 30 s past `app.relaunch`.** Something after the stop: a log flush, a stderr pipe the test holds, or a child or handle inherited across the spawn. The test's `child.try_wait()` result helps here: note when it fired relative to the copy's refusal.
   - **Contention between parallel tests in `windows_service.rs`.**

   Add whatever diagnostics you need to the test's failure dump, never the token, so a CI failure shows which case it is.
2. **Fix it at the cause.**
   - If the 30 s budget is simply too tight for a cold PowerShell, measure the budget from the moment the watch is actually ready, for example a second status word after the first `Get-Process`, rather than raising `PREDECESSOR_WAIT`.
   - Any change to a user-visible bound or wording is the runner's call under §2.26: name it in the PR.
   - Keep everything that X-flake-portable-restart and -2 established:
     - Constrained Language Mode safety: cmdlets only, no .NET method calls;
     - the token never reaches the watch;
     - the bounded line reader;
     - "exited only after the process object signals".
3. **Tests:**
   - unit tests for any script or text change;
   - the Windows test passes reliably. Loop it locally if you can; otherwise say so, and give the evidence from CI.
   - Show that each new test fails when its change is reverted.
   - Condition waits with deadlines of at least 10 s. No fixed sleeps, and no tokio `test-util`.
4. **Decisions file:** `docs/decisions/2026-11-12-X-flake-portable-restart-3.md`.

## Owns
- `crates/lodestar/src/system/restart.rs`
- `crates/lodestar/src/windows/relaunch.rs`
- `crates/lodestar/tests/windows_service.rs`
- its decisions file

## Not owned
Open PRs that touch nearby files: #734 (`services.rs`) and #733 (`main.rs`, the companions crate). Ask the runner before editing any file outside Owns.

## Checks
- `cargo fmt --check`.
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`. Also run `cargo clippy --target x86_64-pc-windows-gnu -p lodestar --all-targets` if mingw is available, because Linux CI doesn't lint `cfg(windows)` code.
- `cargo test -p lodestar`.
- CI's `test (windows-latest)` must be green.

Branch `slice/X-flake-portable-restart-3`; PR title "X-flake-portable-restart-3: <title>". Follow the steward skill, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) "ready <pr> <sha>" when CI is green.
