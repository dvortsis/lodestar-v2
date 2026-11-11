# X-win-upd-window (Opus, core, concurrency, /security-review): Windows update follow-ups after #659 and #677

Assigned by the runner on 2026-11-11 to the session that built #659 (PK-win-portable-upd, merged as bc717d45). The reviews are `reviews/rv-659-c48421bb.md` and `reviews/rv-677-f4243e6c.md`.

## Do
1. **Install now respects Off and pause** (the #677 flag, already sent to the owner under §2.19).
   - When updates are switched Off or a pause starts, `check.rs` removes the pending `request.json`, atomically, under the same lock or ordering the writer uses.
   - Then the helper's boot retry can't replay Install now.
   - Test: request, then Off, then a boot retry does nothing; and the same with a pause.
   - Don't edit helper.rs, msi.rs or rollback.rs; X-win-upd-reoffer (slot 2) owns them. If the helper side also needs a check, tell the runner instead.
2. **The install_blocked wording.** `install_blocked` currently shows the install_failed notice. Give it its own plain wording: the update is waiting because Windows refused to start the service, and nothing changed. Use the existing notice or message files that check.rs feeds. If those are in web/, don't edit them; tell the runner.
3. **N4: the serve::stopped journal race** (`main.rs` ~325). It reads, modifies and writes the journal while `passed` may be running in spawn_blocking, and can write Swapped back after previous.exe is gone. Serialize the two by awaiting the self-test task or sharing a lock. Add a test that forces the interleave without sleeps.
4. **#659 nits:**
   - N1: make `at_start` (portable.rs ~458) `#[cfg(test)]`, or have the tests call `begin`, so the Windows build has no dead-code warning.
   - N2: reword the "What the fallback can't help" bullet. On Windows, the rename replaces an existing lodestar.exe unless it's running.
   - N3: add a test that the old program is put back from "Swapping, program missing, previous.exe present".

## Owns
- `crates/lodestar/src/update/windows/check.rs`
- `crates/lodestar/src/update/windows/portable.rs`
- `crates/lodestar/src/main.rs` (serve::stopped and the probation hand-off only)
- `docs/decisions/2026-11-11-PK-win-portable-upd.md` (N2)
- `docs/decisions/2026-11-11-X-win-upd-window.md`

## Bar
- fmt, clippy (workspace, `-D warnings`), and tests.
- `cargo check --target x86_64-pc-windows-gnu` with no warnings, if the target is installed.
- Mutation-check each new guard. No fixed sleeps.
- Branch `slice/X-win-upd-window`, PR `X-win-upd-window: <title>`, steward skill.
- Reply "ready" with the head when CI is green, Windows included.
