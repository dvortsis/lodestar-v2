# X-win-portable-restart-load decisions

## The failure

On the AWS Windows test VM (X-test-vm, `main` at bd239634), `windows_service::a_portable_restart_serves_again_on_the_same_port` passed 20 of 20 runs on an idle machine. With four threads saturated at normal priority, it passed 11 of 20, and 4 of 5 in a later run under load.

Seven of the nine failures were the #717 class. The new copy's wait for the old process timed out with this timeline:

> started never, ready never, no answer at +60.0 s

The refusal "Close Lodestar and start it again" followed. The new copy's PowerShell watch never even printed its first word.

## Root cause

On Windows, Lodestar lowers itself to below-normal priority at startup (`windows::priority::apply`, architecture §11). Restart now's processes inherited that class:

- **The new copy:** the old copy spawned it with no priority class, and Windows starts a below-normal process's children below normal too.
- **The watch:** the new copy spawned its PowerShell watch on the old process (`watch_predecessor`) the same way, so the watch ran below normal as well.

Under normal-priority load, Windows' scheduler runs below-normal threads only on its starvation boost, one time slice every few seconds. A cold PowerShell needs a second or two of CPU before it can print `started`, so it never got there within the copy's 60 s (twice `PREDECESSOR_WAIT` before `ready`). The deadline was fine; the process waiting on it was starved.

## Calls

- **Restart now starts its processes at normal priority** (runner, approved). Both carry `NORMAL_PRIORITY_CLASS` in their creation flags, set through `CommandExt::creation_flags`, with no new `unsafe` and no PowerShell in the path:
  - **The new copy:** `SUCCESSOR_FLAGS`. It shares the old copy's console, as before.
  - **Its watch:** `WATCH_FLAGS`, which is `CREATE_NO_WINDOW` plus normal priority. `creation_flags` replaces the flags rather than adding to them, so the watch carries both.
- **Steady state is unchanged.** The new copy still lowers itself through its usual `priority::apply` at startup, which runs after its wait for the old process. So it serves at below normal, or at whatever `LODESTAR_PRIORITY` says, as any start does. Only the hand-over runs at normal priority.
- **Unchanged:** `PREDECESSOR_WAIT`, the watch's own limit, and the refusal's wording and conditions. When the old process really doesn't exit, the user still gets the same clear refusal.
- **The old copy's own shutdown still runs below normal.** Raising it for the hand-over would need a PowerShell call in the restart path. The other two failures' messages aren't in the test VM's report. If they show a slow exit, with the watch's `ready` arriving and then no answer, that becomes a follow-up with its own evidence (runner).

## Tests

- `restart_now_starts_its_processes_at_normal_priority` (`crates/lodestar/src/windows/mod.rs`, every platform) pins the flags Windows reads:
  - normal priority for the new copy;
  - no window plus normal priority for the watch;
  - no other priority class bit in either.
- `the_predecessor_watch_runs_at_normal_priority_from_a_below_normal_copy` (Windows) lowers the test process to below normal, then starts the watch through the real code path. It asserts PowerShell reports the watch at `Normal`, and restores the test process's priority afterwards. It checks the cause directly, with no load and no timing. Before this change, the watch would report `BelowNormal`.
- **Re-test under load:** the runner will have the X-test-vm coordinator run the 20× load repro against this change's head once CI is green. That confirms the fix in practice; it doesn't gate the merge.
