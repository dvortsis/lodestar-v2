# X-win-upd-history decisions: the Windows install history, and the notice after a rollback

Brief: `runner-state:briefs/X-win-upd-history.md`; PK-guide-3 found the gap, and rv-708 I5 the notice's. The code is `crates/lodestar/src/update/windows/history.rs` (new), `check.rs` and two calls in `portable.rs`. Calls here are the runner's under §2.19; the owner can veto them.

## Who writes the history: the service, not the helper
- **The service writes `<data>\bin\history.json`; the elevated update helper is unchanged.** At the notifier's start, it reads the helper's last answer (`<data>\updates\result.json`), which the helper already writes for display. It turns that answer into at most one entry.
  - **Why not the helper:** it would gain a new file to write in the service's folder, and a reason to trust or parse it as SYSTEM. The service reading an answer it can already read adds no privilege to either side (§2.47 Q13).
  - **The answer is untrusted input.** The service account can write it too, so it's checked before anything is kept:
    - the version must parse, and is stored as this build prints it;
    - the outcome must be one that ends an install;
    - the time must be non-zero and no more than a day ahead of this clock.
  - Nothing in an entry comes from a path, an account or a setting: version, result, time, a reason code.
- **Which answers make an entry:**
  - `installed` → installed;
  - `install_failed` → failed (`install_failed`);
  - `rolled_back` → rolled back (`selftest`);
  - `rollback_halted` → failed (`rollback_halted`);
  - `install_interrupted` → failed (`install_interrupted`).
  - The others make none, because they don't end an install: `nothing`, `install_blocked` with retries left, `installer_busy`, `rollback_pending`, `handed_off`, `copy_refused` and `refused`.
- **The portable build** records its own outcomes, since it has no helper (runner-approved shared edit, two calls in `portable.rs`). `passed` records installed. A rollback is recorded where it finishes, at the old version's next start in `rolling_back`, not in `failed`, which only starts it. Neither has a reason code, because the journal doesn't keep why the self-test failed.

## The file
- **The Unix engine's file, format and cap:** `history.json` in `<data>\bin`, under `bin`'s own ACLs, newest last, 20 entries kept. It's written by temporary file and rename, acknowledged.
  - `update/history.rs` is Unix-only (`#[cfg(unix)]` in `update/mod.rs`), so `windows/history.rs` mirrors its types, as `check.rs` already does for `updater.json` and `skipped`. A test pins that a Unix file reads and writes back byte for byte.
- **Once per answer:**
  - An entry is skipped when one with the same time, version and result is there.
  - `history-seen.json` (Windows only) keeps the newest answer time recorded. The answer stays in `result.json` after it's recorded, so without it, Clear history would be undone at the next start.
  - The portable build keys its entries by the new program's SHA-256 (first 16 hex digits). That key is the same at every repeat of a journal step, so a repeat adds nothing.
- **Damaged:** the history shows as empty, and `update.history_unreadable` (new, Warn) is logged once at the start, not at every status. The next entry starts the file again. It's display only, so nothing else is held.
  - The new event has its own rows in the logging feature's names (en, zh) and the golden log catalogue, which are runner-approved shared edits.
- **A failed write** of an entry is logged (`update.state_unsaved`, `file` = `history.json`), and the answer is looked at again at the next start. Clear history is acknowledged only once the file is gone durably, with `not_saved` otherwise, as on Unix.

## Serving it
- **`/updates`** returns the entries, newest last, and adds the `rolled_back` notice while the newest entry is a rollback, as on Unix.
- **`rolled_back` is published once per new rollback entry,** through `updater.json`'s `rolled_back_seen`, as on Unix. `check.rs` used to keep that field as found and never set it; it now sets it, and the doc comment says so. If the write fails, the event is published again at the next start.

## The notice after a rollback (rv-708 I5)
- **`settings_updates_rolled_back_next`** said "Check the history below, then decide whether to try again", which invites reinstalling the build that failed. It now says: "You're running the version that worked, and the history below shows what happened. Wait for a newer version instead of installing this one again." zh says the same (runner-approved). A vitest case pins that it never says "try again" or "reinstall".

## Checks
- **Mutation-checked, all killed:**
  - the future-time check;
  - the once-per-answer marker;
  - the entry-level duplicate check;
  - `rolled_back_seen`;
  - the `rolled_back` notice;
  - a non-ending outcome recorded;
  - the start not recording;
  - Clear history doing nothing;
  - both portable calls;
  - the damaged-file log.
- No fixed sleeps. The one wait, for the start's record, is a condition loop with a 20 s deadline. No tokio `test-util`.
