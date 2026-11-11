# PK-guide-3 (Sonnet, docs): user guide for Windows updates, rollback and Restart now

Runner brief, 2026-11-11 (UTC). Slice entry: `runner/slices.yaml` PK-guide-3, spec §12.2 and §10.10.6, and `docs/owner-decisions.md` §2.14 and §2.23. It is the last docs slice before the M6 exit gate (G-m6). Use the project's **user-docs** skill, which builds on github-docs.

## 1. Read first
- **`docs/user/04-install-on-windows-and-linux.md` on main.** PK-guide-2 (#704) just rewrote it, and #707 added the Event Viewer pointer to `06-troubleshooting.md` §6.2. Keep its structure, its voice and the **"(newer than v0.5.1)"** marks. v0.5.1 has **no** in-app update engine and no Restart now, so everything this slice adds carries that mark.
- **`docs/operations.md` §7 "Upgrades and Backups"** is the operator reference. Link to it for CLI detail; don't duplicate it.
- **The code is the source of truth.** Read these before you write a word:
  - The update engine is `crates/lodestar/src/update/`:
    - `policy.rs`, `window.rs`, `control.rs` and `pending.rs` cover the update setting (Off, notify, automatic), the install window (#693) and why **Install now** is withdrawn while updates are Off or paused;
    - `probation.rs`, `rollback_cache.rs` and `windows/rollback.rs` cover the check after an install and the automatic rollback;
    - `windows/msi.rs`, `windows/portable.rs` and `windows/helper.rs` cover how each install type updates;
    - `log_events.rs` has the log lines, and `unavailable.rs` gives the reasons an update can't run.
  - Restart now is `crates/lodestar/src/system/restart.rs` and the restart dialog in `web/`. That covers the estimate and hint, the upgrade-day pause of Restart now, and how the portable copy restarts.
  - **The portable ZIP** is `packaging/windows/portable/Start Lodestar.cmd` and `README.txt`: the console window's behaviour after a relaunch, and the guarded `previous.exe` fallback.
  - **The service's Event Log** is `crates/lodestar/src/windows/service.rs` `Refusal::event` and `event_log_text`. While a rollback is in progress, the service writes a **WARNING, event ID 4**, source `Lodestar`, to the Application log; #707 left this event for you. The text is fixed apart from the data folder's path, and it points to `%SystemRoot%\Temp\lodestar-service.log` (`windows/temp_log.rs`). The update helper writes under its own source, `Lodestar update helper` (`update/windows/helper.rs`).
  - Settings and their labels: `docs/user/appendix-b-settings-reference.md` and the settings code. Use the real labels from main, not guesses.

## 2. Do
1. **A new section in chapter 4:** keeping Lodestar up to date on Windows. Give it its own heading after the install sections.
   - The update setting:
     - what each choice does;
     - the recommended safe choice and why;
     - where it is in Settings.
   - The install window:
     - when installs may run;
     - what happens outside the window;
     - the notice when updates are Off.
   - What Lodestar checks before it installs (signature, manifest), in one or two plain sentences, so users know an update can't be swapped.
   - The MSI service and the portable ZIP, each its own short task with a result:
     - what the user sees;
     - how long the service is down;
     - where the previous version is kept.
   - **Rollback:**
     - what triggers it (the probation check);
     - what the user sees in the UI and in Event Viewer (the event 4 WARNING above, marked newer than v0.5.1);
     - what to do if a rollback itself fails. Point to §6 troubleshooting and to `data\logs\lodestar.log` (portable) or `%SystemRoot%\Temp\lodestar-service.log` (service).
2. **Restart now:**
   - when the button appears and what it does;
   - the estimate;
   - that it's paused on upgrade day;
   - the portable window's behaviour after a relaunch (what the console prints, and that the new copy keeps running in it).

   Recommend closing the browser tab only after the page comes back.
3. **Troubleshooting rows** in the new section, or in `06-troubleshooting.md` §6.1 if that's where the pattern lives. If you need 06, tell the runner first: it's outside your owns.
   - "Install now is greyed out" (updates Off or paused, or outside the window);
   - "the update rolled back";
   - "the portable window says Lodestar stopped but it's still running".
4. **Appendix B:** the update settings rows (key, label, default, safe choice).
5. **Glossary, index and revisions:** add only the terms you use, such as probation, rollback and install window.
   - These three files are **shared with CMP-D** (the companions guide, running now).
   - Whichever PR lands second brings main in and reruns `docs_tool.py sync`.
   - Never hand-merge generated content; regenerate it.
6. **Diagrams:** where a dialog helps, use an SVG illustration in `docs/user/images/` with a SHOTLIST entry, as PK-guide-2 did. Cloud sessions can't take Windows screenshots.

## 3. Don't
- Don't edit `docs/operations.md`, `docs/user/companions.md` (CMP-D's), `docs/architecture.md`, `runner/`, packaging or any code.
- Don't describe a behaviour you can't point to in main's code. If code and an older doc disagree, the code wins: note it in your decisions file and tell the runner.

## Owns
- The slices.yaml PK-guide-3 owns: `docs/user/04-install-on-windows-and-linux.md`, `docs/user/appendix-b-settings-reference.md`, `docs/user/glossary.md`, `docs/user/index.md`, `docs/user/revisions.md`.
- Plus new `docs/user/images/windows-update-*.svg` and their SHOTLIST lines.
- Plus the decisions file `docs/decisions/2026-11-11-PK-guide-3.md`.

## Bar
- Every fact is checked against the code on main (keep file:line notes for the reviewer).
- `docs_tool.py sync`, `sync --check` and `lint_docs.py --strict`: 0 failed, 0 warnings.
- No dangling anchors. No 12-digit numbers, no personal data, no local time zones. Times are UTC.
- Branch `slice/PK-guide-3`, PR titled `PK-guide-3: <title>`.
- Follow the steward skill, including a fresh self-review before you ask for review. Push once per round of fixes.
- Expect about 400–700 lines with any SVGs. Reply "ready" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once ci-ok, web-ok and e2e-ok are green.
