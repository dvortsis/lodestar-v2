# PK-guide-2 (Sonnet, docs): user guide chapter 4 for main, covering Windows and Linux installs and the filesystem and port warnings

Assigned by the runner on 2026-11-11 to the session that built PK-ui (#701, merged as 9d664250). The slices.yaml entry is PK-guide-2, spec §11 and docs/owner-decisions.md §2.23. Use the user-docs skill, which builds on github-docs.

## 1. Read first
- **`docs/user/04-install-on-windows-and-linux.md` on main.** It is still the v0.5.1 text. This slice brings it up to what main builds, and it is a rewrite of that chapter in place: keep the SmartScreen and checksum sections (4.1, 4.2) unless they're wrong.
- **`docs/operations.md`.** PK-ops (#700) is rewriting it, so read it on its PR branch if #700 hasn't merged. It is the operator reference. Link to it for CLI detail; don't duplicate it.
- **PK-ops' version-mark convention:** "(newer than v0.5.1)". Use the same marks, so a user on v0.5.1 isn't sent to a step that doesn't exist yet.

## 2. Do
1. **Windows MSI.**
   - Cover each install option the MSI really offers: the properties and features in `packaging/windows/main.wxs`, and what each does.
   - Cover the service: its name, the account it runs as, start mode, and where its data folder is.
   - Cover the Start-menu shortcut (it opens `localhost:8090`), and uninstall: what stays behind and how to remove it.
   - Recommend the safe choice at each step.
2. **Windows portable ZIP.**
   - Where to unzip it (not a synced or network folder), and how `Start Lodestar.cmd` behaves.
   - Where data lives, and how it differs from the service.
3. **Linux tarball and systemd.**
   - The layout, the shipped unit file (packaging/), DynamicUser and the state folder, and enable and start.
   - How to read logs with `journalctl`.
   - The real file names and commands from packaging/.
4. **Filesystem warning: what the wizard's Storage step shows** (PK-ui, `fsdetect.rs`).
   - Unraid user share: use `/mnt/cache/appdata/lodestar`.
   - FUSE, network or shared-folder filesystems: move to a local disk.
   - Explain why in one or two plain sentences.
5. **Windows port warning: the Insights exception ephemeral_ports** (PK-ui and `digest.rs`).
   - What the netsh command does, and that it's run in an admin PowerShell or Command Prompt.
   - How to check the result. When there's no fix, the text tells the user to lower the crawler's intensity.
   - Give the command only in the form Insights produces; don't invent ranges.
6. **`06-troubleshooting.md`.**
   - Add install-time problems for Windows and Linux: the service won't start, the port is in use, the data folder refused.
   - Once #702 (X-event-log-notice) has merged, §6.2 points Windows users to Event Viewer → Windows Logs → Application, source Lodestar, which holds folder paths and the fix only. If #702 hasn't merged when you open your PR, add that part marked "(newer than v0.5.1)" only after checking main, or leave it out and say so in the PR.
7. **Windows dialogs are SVG illustrations** (`docs/user/images/windows-*.svg`) with a SHOTLIST entry, because cloud sessions can't capture them.
8. **Glossary, index, revisions and appendix-a:** add only the terms and answers you use.

## 3. Don't
- Don't edit `docs/operations.md` (PK-ops), `docs/architecture.md`, `runner/`, packaging or any code.
- Don't write about updates, rollback or Restart now on Windows. That's PK-guide-3.

## Owns
- The slices.yaml PK-guide-2 owns: 04, 06, glossary, index, revisions, appendix-a, and `docs/user/images/windows-*.svg`.
- Plus `docs/user/SHOTLIST*` if that's where the shotlist lives. Check first.
- Plus a decisions file, `docs/decisions/2026-11-11-PK-guide-2.md`.

## Bar
- Every fact is checked against the code on main: file:line in your own notes, and the facts in `docs/setup.facts.md` if the skill uses it.
- `docs_tool.py sync` and `sync --check`, and `lint_docs.py --strict`, with 0 failed and 0 warnings.
- No dangling anchors. No 12-digit numbers and no personal data.
- Branch `slice/PK-guide-2`, PR titled `PK-guide-2: <title>`. Follow the steward skill, including a fresh self-review before you ask for review.
- Expect about 500–700 lines with the SVGs. Reply "ready" with the PR number and head SHA when CI is green.
