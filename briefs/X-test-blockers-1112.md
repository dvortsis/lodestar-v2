# X-test-blockers-1112: two cloud-testing fixes from the tester
**Model:** Opus. A fresh Opus reviewer follows.

Runner brief, 2026-11-12T18:56Z. **Owner 2026-11-12: prioritise anything cloud testing needs**, and testing blockers come before features. This runs in one of the 4 cloud-testing slots, not a build slot. Keep it small and land it fast.

## 1. The leak test names a handshake that never completed
- **The tester's finding:** `infra/fleet/steps/vm/leak-test.sh` (and `start-block.sh` where it reports) fails with a generic message when the WireGuard handshake never completes, for example an expired PIA key. The kill switch held (0 packets), but the report didn't say why it failed.
- **Fix:** detect "no handshake" separately. For example, `wg show <if> latest-handshakes` is 0, or gluetun's health/status says so; use whatever the script already has access to. Report it as its own explicit outcome: "WireGuard handshake never completed (check the VPN credentials)".
- **It's still a failure, never a pass.** It must never be confused with a leak, and a real leak must still be reported as a leak.
- Tests go in `infra/fleet/tests/` (follow `test_steps.py` / `test_block.py`): no handshake, a leak, and clean.

## 2. The saturated-host restart test flake
- **The tester's finding** (Windows lane 5a): `a_portable_restart_serves_again_on_the_same_port` (`crates/lodestar/tests/windows_service.rs:~672`) fails 1 in 6 on a saturated 4-vCPU host.
  - The new copy serves (the console shows app.serving about 28 s later).
  - But `lodestar.jsonl` has none of its lines by the 90 s deadline. Both copies run at below-normal priority, so the log flush starves.
- **Fix it at the root, without weakening what the test proves:** the new process is serving on the same port.
  - Accept the console's app.serving as the readiness signal, or wait on the jsonl with headroom tied to an observable condition.
  - **No fixed sleeps**, and the deadline stays at least 10 s.
- `git log origin/main -- crates/lodestar/tests/windows_service.rs` shows the earlier restart fixes. Read them first and don't undo them.
- **You can't run Windows here.** Reason it out from the code, and if CI's `test (windows-latest)` runs it, say whether it passed.

## Owns
- `infra/fleet/steps/vm/leak-test.sh`, `infra/fleet/steps/vm/start-block.sh`, `infra/fleet/tests/**` (only the tests for these), the test function in `crates/lodestar/tests/windows_service.rs`, and `docs/decisions/<date>-X-test-blockers-1112.md`.
- **Ask the runner first** for anything else.
- **Never make a cloud call.** No terraform, no AWS or Linode API, no VPN connection. If the sandbox has AWS_* variables set, never read or print them.

## Done when
- Both fixes have tests, and CI is green.
- **Checks:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings` (stable 1.100: if your first CI is red only on the net.rs needless_range_loop, that's #776, so bring `main` in once after it merges);
  - `cargo test -p lodestar --test windows_service` (it compiles on Linux even where its tests are Windows-only);
  - the fleet tests the way CI runs them (see `.github/workflows`).
- **Size:** about 200 lines.
- **Process:**
  - follow `.claude/skills/steward/SKILL.md`, push once per round, never merge;
  - branch `slice/X-test-blockers-1112`; PR title `X-test-blockers-1112: leak-test handshake outcome and saturated restart flake`;
  - message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with "ready <pr> <full sha>" once CI is green, using the Claude Code Remote MCP `send_message` tool.
