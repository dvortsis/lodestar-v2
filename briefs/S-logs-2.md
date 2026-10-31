# S-logs-2: Settings → Logging
**Model:** Opus: the screen decides what personal and torrent data reaches log files (§12.1 privacy), so it's security-relevant. Runner brief, 2026-10-31. The second half of S-logs; S-logs-1 (#284, merged 1eaf8c8b) built the live log page `/logs`. `/security-review` is required.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md`, `docs/ui-glossary.md` (`log_event`, `log_data_kind`, `log_private`, `live_log`, `full_log`).
- **`docs/architecture.md` §10.10.12, "Settings → Logging"** (the spec), §10.10.1–10.10.7 (the engine, risky changes, Danger zone), §10.10.11, **§12.1** (logging privacy: data kinds, identifying kinds, durations, opt-in only, never logged) and the owner's decision §2.11 (opt-in kinds).
- `docs/decisions/2026-10-22-B2b.md`, `2026-10-23-B-log1*.md`, `-B-log2.md`, `-B-log2-files.md`, `2026-10-31-S-logs-1.md`, `-S-settings-2b.md` and `-M-security-settings.md` (how hand-built screens use the engine, lazy parts, size rules), and the engine README.
- The API: `/settings/logging` (and `/settings/preview` if it serves the preview), `/logs/catalog`, `/logs/files`, `/logs/files/{name}`, `/logs/archive.zip`, in `web/src/lib/api/schema.d.ts`. `logging` is already in the registry's `system` group (`shell/screens.ts:27`); check whether a screen exists for it.

## Do
Build the screen per §10.10.12 "Settings → Logging". Generate what the schema covers, and hand-build only the following:
1. **Events:**
   - the Verbosity preset (a SegmentedControl, with plain words and the technical term);
   - the event list grouped by component, collapsible with a count;
   - each row shows a toggle, its plain name, its level (icon + text), its current rate from the counters, data-kind badges and one line of help;
   - required events show a lock and the reason;
   - its own search and Show changed only;
   - Reset to preset;
   - per-component levels under Advanced, and the raw filter under Expert.
2. **Data kinds:**
   - the preset (Private default / Hashes / Full / Custom);
   - one toggle per kind, saying which enabled events would carry it.
   - **Turning on an identifying kind opens a dialog** that says exactly what is recorded and where, and asks for a duration (1 hour / 24 hours / until I turn it off).
   - While any identifying kind is on, a **banner with the time remaining and "Turn off now"** shows on this screen. Wire `/logs`'s existing banner slot to the same Turn off now if the API allows; S-logs-1 left a link there.
   - Turning kinds off offers to delete the files written meanwhile.
3. **Opt-in only:** Peer activity and Filter change details, each off. Turning one on opens its own warning with a 1-hour or 24-hour duration. Presets never touch them, and each has its own banner.
4. **Never logged:** the fixed list, each with its reason.
5. **Outputs:** console format and detail; the log file settings with size previews; the Windows Event Log option (Windows service only).
6. **Log files:**
   - the list with "Contains torrent details" badges;
   - Download per file, with "Remove torrent details" on by default when the file has them;
   - Download all (zip);
   - the folder path with Copy;
   - the Docker hint.
7. **Danger zone:** Delete log files (all, or only those with torrent details), with the count and size in the dialog.
8. **Preview:** example lines and the estimated volume per day, updating as the user stages changes, but only if an API serves it. Otherwise leave a slot and say so.
9. **Carried from #284's review:**
   - in `features/logs/LogsPage.svelte`, the `role="status"` text joins "Paused" and the notice with no separator; separate them;
   - in `e2e/logs.keyboard.test.ts`, the scroll test must poll until the list is away from the bottom before reading `scrollTop`.
10. **Privacy invariants (blocking if broken):**
    - no preset ever turns on an identifying or opt-in kind without the dialog;
    - durations are enforced by the server, and the UI only displays them;
    - nothing turns on silently;
    - a failed request leaves the kind off;
    - the banner always reflects the server's state.
11. **Sizes:**
    - the Logging first view ≤ 45 KB with ≥ 0.3 kB of headroom, with the dialogs, event list and file list loaded on demand;
    - **no other Settings first view may grow** (Notifications is at about 44.8 of 45 KB). If adding the screen moves them, find the cause, or stop and report the numbers;
    - new entries at measured + 10%;
    - never raise an existing limit.
12. **i18n:** `settings_logging_*` keys in en and zh, regenerated with the repo's generator.
13. **Tests:**
    - vitest: each privacy invariant above, the duration dialogs (cancel leaves the kind off), opt-in separation from presets, Reset to preset, delete confirmations;
    - Playwright: the screen renders and saves against the mock, keyboard-only, with axe;
    - condition waits only, no fixed sleeps;
    - Settings baselines may change if the nav entry becomes live. Use the `.update-snapshots` marker and check that only Settings images change.
14. **Decisions file:** `docs/decisions/<date>-S-logs-2.md`.

## Own
`web/src/lib/features/settings/logging/**` (or wherever the registry expects the logging screen; follow the screens2/security pattern), its registry entry (one line, if one is needed), its message keys and regenerated maps, the logging mock fixtures, `web/e2e/settings-logging.*`, `features/logs/LogsPage.svelte` and `e2e/logs.keyboard.test.ts` (item 9 only), its size-limit entries and your decisions file. **Don't edit** the engine (`form/**`), `shell/**` beyond that one line, `screens2/**`, `security/**`, `lib/state/**`, `lib/ui/**` or `lib/api/**`. M-security-settings-2 is running in `security/**`.

## Size
≲ 1,500 changed lines. If it grows past that, split it: **2a** events, data kinds, opt-in, never logged and banners; **2b** outputs, files, danger zone and preview.

## Done when
These pass in `web/`: `npm run check`, `npm run e2e:typecheck`, `npx vitest run`, `npm run build` then `npx size-limit`, and `npx playwright test` for the Settings and logs specs (3× for new ones). Chromium is at /opt/pw-browsers; never run `playwright install`. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
