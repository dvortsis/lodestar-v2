# Owner decision: the Windows Event Log in the Legal Notice (draft, runner, 2026-11-11)

## Why this needs the owner
- The Legal Notice §5 lists every place Lodestar keeps data outside its data folder ("four exceptions"). Changing that wording is the owner's.
- **The fact has changed on `main`.** Since PK-win-upd-3 (#644, merged 2026-11-09), Lodestar's Windows update helper writes to the Windows Application event log (`crates/lodestar/src/update/windows/helper.rs` `event_log`, `rollback.rs`). It does this when an update can't be installed, is skipped, or has to be rolled back.
- The Application log can be read by every user of the computer. The notice and `legal_facts.rs` still say no code writes there. The test only checks that the notice doesn't name the Event Log, which is why it still passes.
- **No released version is affected.** v0.5.1 and `release/v0.5.2` don't contain the update helper. This must be settled before a release that ships the Windows updater.
- **Second, older question (first logged 2026-11-07T22:00Z, X-win-refusal-ux):** should the Windows service also write its start refusal reason there? Event 7024 today shows only the misleading "The system cannot find the path specified".

## What the helper writes
Fixed English sentences with version numbers, the step that stopped, error codes and how to recover, for example: "Lodestar's update helper didn't install 0.7.0: its copy refused (...). Nothing was changed." It writes no torrent, search, account or network data. The recovery text can name folder paths.

## Runner recommendation: approve option A
**A. Add a fifth exception** (recommended). Bump NOTICE_VERSION once, folded into Notice v5 (owner item 11) so users see one change. The new clause in §5:

> on Windows, when an update can't be installed or has to be undone, or the service refuses to start, Lodestar writes what happened and how to fix it (version numbers, the step that stopped and folder paths only) to the Windows Application event log, which every user of the computer can read;

and "four exceptions" becomes "five exceptions". The service refusal reason (the 2026-11-07 question) goes there too, refusals only, with no data values. The `event_log_errors` setting (general errors) stays hidden.

**B. No Event Log.** The update helper stops writing there and keeps the reason only in `result.json` and Lodestar's own log. An administrator then has no standard Windows place to see why an update was undone while the service is stopped. The notice stays as it is.

## After the answer (runner)
One slice: the notice text in both copies (`crates/api/src/legal/notice.md` and `docs/legal/legal-notice.md`), NOTICE_VERSION, `legal_facts.rs` turned into a positive check (the notice must name the Event Log where code writes there), `docs/legal/README.md`, and for A the MSI event source plus the service refusal write.
