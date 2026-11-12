# X-event-log-followups decisions

Follow-ups after #702 (X-event-log-notice, Legal Notice version 5), from the runner's brief and rv-702's nits. The Legal Notice text itself doesn't change. These are the slice's decisions under §2.33. None loosens an owner decision or a security decision.

## What goes to the Application log
Section 5 of the Legal Notice allows the Application log to hold what happened, how to fix it, version numbers, the step that stopped, and folder paths. Every local user can read that log. This slice keeps each writer inside that, and tighter where the text came from a file on disk or could name a place in a user profile.

- **The update helper (`update/windows/helper.rs`):** both notes, the blocked install (error 1920) and the install that exited 0 without replacing the program, are built by functions that take only Lodestar's parsed values: a count and semver versions, whose grammar holds `0-9 A-Z a-z . - +` alone.
  - The journal's `from` is text read back from disk. It shows only if it parses as a version, else as "an earlier version".
  - `event_log_reasons_have_fixed_shapes` pins both texts, and checks that a path, an account, a setting or a second line in `from` never reaches them.
- **The rollback (`update/windows/rollback.rs`, its Event Log text only; runner, 2026-11-11, option (a)):**
  - The copy-refused note shows the copy's refusal only as a code (`a-z` and `_`, at most 40 bytes, as `msi::Refusal::code` writes it), else "an unexpected reason". It's read back from the copy's result file.
  - **The bound is the shape, not a list,** for these codes and for versions: a lowercase word, or a semver pre-release such as `0.6.0-alice`, still passes. Both files sit in the private staging folder, which only SYSTEM and Administrators can write, and the helper writes only codes and a signed manifest's versions there. So a word could come only from an administrator's own edit. The shape still rules out paths, spaces, accounts (`DOMAIN\user`) and second lines.
  - The restart note shows `to` and `from` only as versions, else fixed words.
  - **The halt's recovery text has an Event Log copy, `recovery_event`:**
    - the same steps, the parsed versions, and the files by name (`journal.json`, `bin\rollback.json`);
    - the folders by role ("Lodestar's data folder", "Lodestar's update staging folder");
    - no reason text. The halt's reason comes from step errors, which can name files. Before step 1 nothing was touched and the journal is cleared, so the note points to `updates\result.json` in the data folder instead, where the helper's answer keeps the reason.
  - The full `recovery`, with the paths and the reason, still goes to the journal and `result.json`, for the administrator who follows it.
  - A configured data folder can sit in a user profile, so its path stays out of the Event Log even though section 5 would allow a folder path.
  - `the_rollbacks_event_log_text_has_fixed_shapes` pins it at every step and cause, with hostile journal text.
- **The service (`windows/service.rs`):** its note's folder name is now filtered through `acl`'s list of Unicode format and separator characters (`acl::invisible`, Cf, Zl and Zp), instead of its own shorter list. It newly drops:
  - U+00AD (soft hyphen);
  - U+061C (Arabic letter mark);
  - U+2060–U+2064 (word joiner and invisible operators);
  - U+206A–U+206F (deprecated format controls);
  - the rest of Cf.

  One function, so the two lists can't drift. Tested for each range the brief names.

## The update helper's `eventcreate` wait is bounded
- **`run_bounded` runs it through `supervise`,** the same bounded wait msiexec gets, with no input or output. Past `EVENT_LOG_WAIT` (10 s) the child is killed and reaped, and the entry is given up: it's best effort, and a hung `eventcreate` must never hold up a rollback or a restart.
- **No fixed sleep:** `supervise` polls with a deadline.
- **`a_hung_event_log_command_is_killed_at_its_bound`** uses a real child that never exits (`sleep 600`, or `Start-Sleep` on Windows). It's killed at a short bound, well inside the test's 10 s deadline. It also checks that a command that ends answers its code.
- **Not changed here: the service's own `eventcreate` is still unbounded** (`windows/service.rs`, run as it refuses; rv-702's N2, still open). The brief's item 5 names the helper only.
  - Nothing bounds it. The service has already reported Running when it refuses, so a hung `eventcreate` keeps the process Running until the Event Log service answers. The SCM doesn't end a Running service, and a stop request times out without killing it.
  - On the rollback path the hang comes before the idle wait, so the stop signal isn't seen either.
  - The fix is the same bounded run. `run_bounded` is `pub(super)` in `update::windows::helper` today, so moving it beside the service (for example into `windows/mod.rs`) is a small follow-up. I named it to the runner rather than widen this slice.

## Docs and checks
- **`settings/logging.rs`:** the comments said nothing writes to the Windows Event Log. They now say Lodestar's logging never does, that the service's and the update helper's fixed notes do, and that `event_log_errors` stays hidden and unused.
- **User guide, chapter 6.1:** the two table rows about the Windows service's failures now point to the Application log, marked "(Newer than v0.5.1)" as section 6.2 is:
  - "three failure events": when the cause is the data folder (section 6.2), three Lodestar notes too;
  - "stops at once and stays stopped": look for the Lodestar error, event ID 3, first.

  The wording matches the service's note: it names the data folder and the Repair fix. `lint_docs.py docs/user --strict`: 0 failed, 0 warnings.
- **`docs/legal/operator-guidance.md`:** its status line now describes the development builds on `main`, which lead to the next release cut from `main`, as version 5 of the Legal Notice. That's the runner's wording (owner-decisions §2.57 delegates it), since 0.5.2 itself ships version 4. The rest of the page says nothing about the Event Log, and needs nothing: the notes hold no visitor's data.
- **`docs/legal/README.md`:** #709 already rewrote the section 5 row to list .NET's `Diagnostics.EventLog` and `WriteEntry` beside `eventcreate` and the rest, ignoring case, as `tests/legal_facts.rs` checks. So this PR takes `main`'s row, and the file has no change of its own.
- **`packaging/windows/smoke.ps1`:** after the refused start (the administrator's Explorer entry on the data folder), it waits up to 30 s for an Application log event from the source `Lodestar`, ID 3, since the start. The wait polls, with no fixed sleep. It checks that:
  - the service's own account, `NT SERVICE\Lodestar` (its SID from `sc showsid`), wrote it;
  - it gives the Repair fix;
  - it doesn't name the extra account.
- **`docs/decisions/2026-11-11-X-event-log-notice.md`:** its heading gets `(#702)`.
