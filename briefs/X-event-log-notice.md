# X-event-log-notice: Legal Notice version 5, the Windows Event Log as section 5's fifth exception (Opus, /security-review)

**Authority:** owner §2.55 (2026-11-11, option A), in `docs/owner-decisions.md` once runner PR #697 merges. The draft and its background are in `briefs/X-event-log-notice-draft.md`.

## Why
- On `main`, the Windows update helper writes to the Windows Application event log (`crates/lodestar/src/update/windows/helper.rs` `System::event_log`, and `rollback.rs`).
- The Legal Notice §5 says otherwise: it lists four exceptions and doesn't name the Event Log.
- `legal_facts.rs:416-421` and `docs/legal/README.md:29` assert the notice must not name it, because "no code writes to the Windows Event Log". That premise is false.
- The service's start refusal still shows only event 7024's misleading text (X-win-refusal-ux deferred this to the owner).

## Scope
1. **The notice, in both copies.** `crates/api/src/legal/notice.md` and `docs/legal/legal-notice.md` stay byte-identical where they are today. In §5, "four exceptions" becomes "five exceptions", and the clause below is added verbatim, before "and the optional safe-restart switch…" (or at the position the list's order suggests, with the wording unchanged):
   > on Windows, when an update can't be installed or has to be undone, or the service refuses to start, Lodestar writes what happened and how to fix it (version numbers, the step that stopped and folder paths only) to the Windows Application event log, which every user of the computer can read;
2. **NOTICE_VERSION 4 → 5** in `crates/api/src/legal.rs`, with whatever golden, test or e2e files change because of the version.
   - X-prowlarr-register adds its §2 text later under the same version 5. Say so in the decisions file.
3. **`legal_facts.rs` becomes a positive check:**
   - The notice must contain a canonical phrase, for example `Windows Application event log`.
   - Every Event Log writer in the code is enumerated: a grep-style source check over `crates/lodestar/src` for the writer function or `ReportEventW`. It fails, naming §5, when a new writer appears outside the allowed modules (the update helper, rollback, and the service refusal).
   - Update `docs/legal/README.md`'s phrase table and the "four exceptions" count line.
4. **The service refusal reason goes to the Event Log** (`crates/lodestar/src/windows/service.rs`).
   - When the service refuses to start (the ServiceSpecific exit, data-folder ACL and the others), write the same plain reason Lodestar's own log gets.
   - Write only paths, account names and the fix (the icacls command). Never config values, tokens or data.
   - Reuse the helper's writer if it can be shared without editing `update/windows/**`'s behaviour. Otherwise add a small `windows` module function, and leave the helper's own writer alone.
   - Source name: "Lodestar". Check how the helper registers or names its own source ("Lodestar update helper") and stay consistent.
5. **The MSI registers the event source(s)** in `packaging/windows/main.wxs` (util:EventSource or the registry keys), so the messages render without the "description … cannot be found" text. It must be removed on uninstall.
6. **`settings.logging.event_log_errors` stays hidden and unread.** General errors are out of scope (§2.55).

## Owns
- `crates/api/src/legal/notice.md`, `docs/legal/legal-notice.md`, `docs/legal/README.md`, `crates/api/src/legal.rs`
- `crates/lodestar/tests/legal_facts.rs`
- `crates/lodestar/src/windows/service.rs`
- `packaging/windows/main.wxs`
- the decisions file `docs/decisions/2026-11-11-X-event-log-notice.md`
- the version-bump goldens that the existing tests point to

**Ask the runner first** before editing `update/windows/**`, `windows/mod.rs` or `web/**` beyond the version-bump goldens.

## Tests and gates
- `legal_facts` passes. It fails when the clause is removed, or when a writer is added in an unlisted module (show both in the PR).
- Unit-test the refusal message builder: only paths and accounts, nothing from config.
- Windows CI green. Run the WiX build locally if possible (the `packaging` job).
- Standard gates: fmt, clippy, workspace tests, `npm run check` if web goldens change. No fixed sleeps.
- Run `/security-review` on what is written to a log that every local user can read.
