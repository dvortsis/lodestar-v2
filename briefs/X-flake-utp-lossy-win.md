# X-flake-utp-lossy-win

Runner brief (2026-11-21). Branch `slice/X-flake-utp-lossy-win`; PR title `X-flake-utp-lossy-win: make the lossy-link uTP test robust on Windows`.

## Why
`crates/peer-wire/tests/utp.rs` `fetches_over_a_lossy_link` failed on `test (windows-latest)` on #1003's head (2026-11-21T06:45Z), a PR that doesn't touch peer-wire. It panicked at `utp.rs:386:33` with "Require: the first window didn't arrive in time". #629 (X-flake-utp-lossy) fixed this test once before; it has now recurred. A re-run only hands the flake to the next PR (CLAUDE.md, multi-session work).

## Do
1. **Root-cause it first.** Find where "the first window didn't arrive in time" comes from (the fetch path's first-window or handshake deadline under MSE `Require`) and what timing it depends on. Read #629's decisions file to see what was changed then and why it wasn't enough.
   - The link's loss is seeded, so the dropped packets are deterministic, but timer firing, retransmit timing and scheduling on a slow Windows runner aren't.
   - Reproduce on Linux under stress if you can: run the test in a loop, with CPU contention (for example `stress`-style busy threads in the test process, or `taskset` to one core), and record the failure rate before and after.
2. **Fix the test's robustness, not the product's limits.** Acceptable fixes include:
   - giving the test its own deadlines that scale for a lossy link (at least 10 s each, never a fixed sleep);
   - making the loss pattern unable to drop every copy of a handshake-critical packet within the deadline;
   - running the test on a current-thread or paused-time runtime where that keeps it meaningful.
   - Don't weaken what the test proves: the fetch completes over a link that really loses and reorders packets (`resent > 0` stays), in both `Require` and `Disable` modes.
3. **If the root cause is a product bug** (for example a first-window deadline that a real lossy link would also miss, or a retransmit that never fires), stop and tell the runner before changing product code. That's a different slice, with a fresh review of the uTP/MSE code.
4. Add `docs/decisions/2026-11-21-X-flake-utp-lossy-win.md` with the root cause, the before and after failure rates, and why the fix holds on the slowest runner.

## Owns
- `crates/peer-wire/tests/utp.rs` and any test-support file under `crates/peer-wire/tests/` that only this test uses.
- The new decisions file, `runner/slices.yaml` (your entry), `runner/dashboard.yaml` (your placement line, only if no rule covers `X-`).

## Not
- No product code change without the runner's OK (step 3). No skipped, ignored or quarantined test. No fixed sleeps.
- No new dependency.

## Checks
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-peer-wire`, plus your stress loop's numbers. CI's windows-latest job must pass on your head.

When CI is green on your head, send the runner `ready <pr> <sha>`.
