# X-win-upd-history-polish decisions: follow-ups to #722

From the runner's review of #722 (`rv-722-d038d17e`). The code is in `crates/lodestar/src/update/windows/check.rs`, `history.rs` and `portable.rs`. Calls here are the runner's under §2.33.

- **With updates Off, the check loop still wakes to look at the helper's answer,** at most 15 minutes apart (`MAX_SLEEP`), and runs no check. Before, its sleep with no check due never ended, so `2026-11-12-X-win-upd-history.md`'s "at most 15 minutes apart" held only with updates on.
- **An answer whose record failed is kept in memory and tried first at the next look.** Before, a look that failed just before a hand-over lost the answer, because the hand-over removes `result.json`.
  - **Residual:** a restart before the next successful look still loses it, since the copy is in memory only.
- **Each portable install attempt is its own history entry.** The journal gains a random `attempt` id, which the record uses as its key. Before, the key was the new program's hash. A build that rolled back without being skipped (it never started) and then failed again was neither recorded nor announced the second time.
  - **Older builds still read the journal:** it accepts unknown fields by design. The rollback runs in the version that wrote the journal, and the new version is always later, so both know the field.
  - **A journal from before the field** falls back to the hash.
- **A failed `history-seen.json` write is logged under that name,** not as `history.json`.
- **`settings_updates_rolled_back_next`** now ends "Wait for a version newer than the failed one.", in place of the ambiguous "this one". zh says the same.
  - **Size:** the Filters screen's first view shares the chunk these strings are in, and sits at its budget (31.14 kB of 31.14 kB). Two other wordings went 1 to 6 bytes over, so this one was picked to fit. The next string added there may not fit.
- **Left for X-win-upd-first-rollback:** a `rollback_halted` answer still gets the `install_failed` notice.

## Checks
- **Mutation-checked, all killed:**
  - Off never waking;
  - the failed answer not kept;
  - the attempt id ignored;
  - the marker's failure named as `history.json`.
- **No fixed sleeps:** condition loops with 20 s deadlines. In tests the loop's longest sleep is shortened to 20 ms, through a field the tests set. No tokio `test-util`.
