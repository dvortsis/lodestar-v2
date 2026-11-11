# X-win-upd-bounds (Opus, /security-review): bounds and test gaps left by the Windows MSI updater

Assigned by the runner on 2026-11-11. PK-win-upd-3 merged as #644 (f44df6a8) with these items accepted as residuals for this slice. The reviews are on runner-state: `reviews/rv-644-*.md` and `reviews/sec-644-*.md`; read the last three.

## Owns
- `crates/lodestar/src/update/windows/rollback.rs`, `helper.rs`, `msi.rs`, `check.rs`; `crates/lodestar/src/update/layout.rs`; `crates/lodestar/src/windows/service.rs` (the idle path only); their tests; `docs/decisions/2026-11-11-X-win-upd-bounds.md`.
- **Not** `update/windows/portable.rs` or the one `mod.rs` line PK-win-portable-upd adds (running at the same time). If you need `mod.rs`, keep edits to your own lines; whichever lands second brings `main` in.

## Build (each with a test that fails without it)
1. **LOW-B / P-8:** a boot-identity bound on the 3010 reinstall wait, so a restart that never happens can't hold the helper forever.
2. **LOW-C:** a copy waits only for its own parent helper, not any helper.
3. **P-3:** after a deadline kill of msiexec, wait (bounded) for Windows Installer to be idle before the next step.
4. **P-4:** `try_wait` exit classification, and the HandedOff race.
5. **`update/layout.rs:~239`:** the busy-lock retry (bounded).
6. **R5-1:** the rollback Fake's service must behave like the real idle service (exit/continue on MarkerGone), so `a_rollback_leaves_the_service_running_when_the_msi_started_it` would fail on 93a26643. Prove it by checking the test against that behaviour.
7. **R5-2:** test the drop path's marker removal (`rollback.rs:~414`).
8. **R5-3:** the long doc line at `windows/service.rs:~152-153`.

## Rules
No new privilege, trigger or principal beyond what §2.47's Q13 row approved. No fixed sleeps; fake clocks or condition waits ≥ 10 s. Never approve tokio `test-util`/`testing` features on the `lodestar` package.

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/X-win-upd-bounds` from the latest `origin/main`; PR title `X-win-upd-bounds: <title>`.
- Before review: mutation-check every guard you add (the last reviews were mostly about guards with no test). No fixed sleeps in tests; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. Keep the PR under ~1,500 changed lines (generated files aside); if it grows past that, tell the runner before splitting.
- Anything beyond this brief (a new crate, a `Cargo.toml` change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `X-win-upd-bounds: PR #<n> ready for review, head <sha>` with CI green. Blocked: `X-win-upd-bounds: blocked on <what>`. The runner reviews and merges; you never merge.
