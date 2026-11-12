# X-win-upd-history decisions (#722): the Windows install history, and the notice after a rollback

Brief: `runner-state:briefs/X-win-upd-history.md`; PK-guide-3 found the gap, and rv-708 I5 the notice's. The code is `crates/lodestar/src/update/windows/history.rs` (new), `check.rs` and two calls in `portable.rs`. Calls here are the runner's under §2.19; the owner can veto them.

## Who writes the history: the service, not the helper
- **The service writes `<data>\bin\history.json`; the elevated update helper is unchanged.** The service reads the helper's last answer (`<data>\updates\result.json`), which the helper already writes for display, and turns that answer into at most one entry.
  - **When it looks** (self-review B1): the helper answers only once the service it installed is up and healthy, so after the notifier's start. A look only at start would miss nearly every answer. And Install now's hand-over removes the answer, so a missed one would be lost for good. The service looks:
    - at the start;
    - at every `/updates` status;
    - at each wake of the check loop, at most 15 minutes apart, so an answer is seen even with no Updates page open;
    - just before a hand-over.
  - **Why not the helper:** it would gain a new file to write in the service's folder, and a reason to trust or parse it as SYSTEM. The service reading an answer it can already read adds no privilege to either side (§2.47 Q13).
  - **The answer is untrusted input.** The service account can write it too, so it's checked before anything is kept:
    - the version must parse, and is stored as this build prints it;
    - the request id must be a request id (hex, 16 to 64 characters);
    - the outcome must be one that ends an install;
    - the time must be non-zero and no more than a day ahead of this clock.
  - Nothing in an entry comes from a path, an account or a setting: version, result, time, a reason code.
- **Which answers make an entry:**
  - `installed` → installed, unless it's `restart: true` (exit 3010): the old program then runs until Windows restarts, and the helper judges the install after that, so it isn't the end yet (self-review I1);
  - `install_failed` → failed (`install_failed`);
  - `rolled_back` → rolled back (`selftest`);
  - `rollback_halted` → failed (`rollback_halted`);
  - `install_interrupted` → failed (`install_interrupted`).
  - **`from`** is this build while it's older than the answer's version, because it's still the old one or back to it. Otherwise it's left empty: the answer doesn't say what it replaced, and a later build may have been installed by hand since (self-review I2).
  - The others make none, because they don't end an install: `nothing`, `install_blocked` with retries left, `installer_busy`, `rollback_pending`, `handed_off`, `copy_refused` and `refused`.
- **The portable build** records its own outcomes, since it has no helper (runner-approved shared edit, two calls in `portable.rs`). `passed` records installed. A rollback is recorded where it finishes, at the old version's next start in `rolling_back`, not in `failed`, which only starts it. Neither has a reason code, because the journal doesn't keep why the self-test failed.

## The file
- **The Unix engine's file, format and cap:** `history.json` in `<data>\bin`, under `bin`'s own ACLs, newest last, 20 entries kept. It's written by temporary file and rename, acknowledged.
  - `update/history.rs` is Unix-only (`#[cfg(unix)]` in `update/mod.rs`), so `windows/history.rs` mirrors its types, as `check.rs` already does for `updater.json` and `skipped`. A test pins that a Unix file reads and writes back byte for byte.
- **Once per answer:**
  - An entry is skipped when one with the same time, version and result is there.
  - `history-seen.json` (Windows only) keeps the request id and outcome of the answer last recorded. The answer stays in `result.json` after it's recorded and is looked at again and again, so without it, Clear history would be undone at the next look. It's keyed by id and outcome, not time, so no clock change can hide a new answer (self-review P1). The same request's next outcome, a rollback after an install, is new.
  - The portable build keys its entries by the new program's SHA-256 (first 16 hex digits). That key is the same at every repeat of a journal step, so a repeat adds nothing.
- **Damaged:** the history shows as empty, and `update.history_unreadable` (new, Warn) is logged once at the start, not at every status. The next entry starts the file again. It's display only, so nothing else is held.
  - The new event has its own rows in the logging feature's names (en, zh) and the golden log catalogue, which are runner-approved shared edits.
- **A failed write** of an entry is logged (`update.state_unsaved`, `file` = `history.json`), and the answer is looked at again at the next start. Clear history is acknowledged only once the file is gone durably, with `not_saved` otherwise, as on Unix.

## Serving it
- **`/updates`** returns the entries, newest last, and adds the `rolled_back` notice while the newest entry is a rollback, as on Unix.
- **`rolled_back` is published once per new rollback entry,** through `updater.json`'s `rolled_back_seen`, as on Unix. `check.rs` used to keep that field as found and never set it; it now sets it, and the doc comment says so.
  - Because this now runs at every status, an in-memory mark also keeps it to once per process when that save fails. It's published again at the next start.
- **No `install_failed` notice after a rollback** (self-review B2). The notice is built from the helper's last answer, and it used to match a `rolled_back` answer too. Its body, "download the new version's installer and run it", asks for the build that failed. A rollback has its own notice.

## The notice after a rollback (rv-708 I5)
- **`settings_updates_rolled_back_next`** said "Check the history below, then decide whether to try again", which invites reinstalling the build that failed. It now says: "You're running the version that worked, and the history below shows what happened. Wait for a newer version instead of installing this one again." zh says the same (runner-approved). A vitest case pins that it never says "try again" or "reinstall".
- **Not after a manual rollback** (self-review P5; runner-approved shared edit of `UpdatesScreen.svelte`, that one condition). A build rolled back by hand may have been fine, so that sentence is shown only when the rollback's reason isn't `manual`. A vitest case covers both.

## Residuals
- **A Clear history during the portable build's probation** can be undone. `passed` writes the history outside the notifier's lock, so it can write back the old entries with the new one. The window is narrow, and the file is display only. (`rolling_back` runs at the start, before the notifier exists.)
- **A damaged `history-seen.json`** is read as none. So after a Clear history, the answer still in `result.json` comes back once. The same happens if the history write succeeds and the marker's fails.
- **The portable build's key** is the new program's hash. The same build installed again while its earlier entry is still among the 20 kept isn't recorded a second time. That needs a downgrade and the same update again.

## Checks
- **Mutation-checked, all killed:**
  - the future-time check;
  - the request-id check;
  - `restart: true` not recorded;
  - `from` only while this build is older;
  - the once-per-answer marker (id and outcome);
  - the look at status, at the check loop's wake, and before a hand-over;
  - no `install_failed` notice after a rollback;
  - the in-memory announcement mark;
  - the entry-level duplicate check;
  - `rolled_back_seen`;
  - the `rolled_back` notice;
  - a non-ending outcome recorded;
  - the start not recording;
  - Clear history doing nothing;
  - both portable calls;
  - the damaged-file log.
- No fixed sleeps. The waits are condition loops with 20 s deadlines; the check-loop test waits on a test-only count of looks. No tokio `test-util`.
