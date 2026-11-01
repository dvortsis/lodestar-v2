# S-updates-3: wire "Install now" on the Updates screen
**Model:** Sonnet (Svelte screen work in an existing pattern; the server enforces every rule). The runner reviews; an Opus reviewer follows because it starts an install. Runner brief, 2026-11-01. It completes S-updates now that B-update-5 (#312) shipped `POST /updates/install`.

## Why
`docs/decisions/2026-11-01-S-updates.md` and `-S-updates-2.md` ("Slots") left Install now unrendered because the API had no install request. #312 added it. `updates.test.ts` has a `TODO(part 2)` test to rewrite against the real button.

## Read first
- `docs/architecture.md` around l.1040–1065 (the Updates screen copy: "Install now · Skip this version", the revoked-key notice), §… "Releases that need manual steps are never auto-installed … a guided Install now" (around l.1470), and the revoked-key rule (around l.1537: its Install now skips the level and wait rules; the server decides that).
- `docs/decisions/2026-11-01-B-update-5.md` (the route, its answers, `installing`, `can_install`), `crates/api/openapi.json` / `web/src/lib/api/schema.d.ts` for `/updates/install`.
- `web/src/lib/features/settings/updates/**` (store, logic, Releases, UpdatesScreen, the SSE install steps it already shows), both S-updates decisions files.

## Do
1. **The button.** "Install now" beside each installable release (and beside Check now where the copy puts it), per the §10 copy. It is **not rendered** while `capability.can_install` is false for `trust_damaged` or while `trust_damaged` is a notice (keep the existing test's intent), nor while an install is under way (`installing`, or the SSE install state). For other `can_install: false` reasons, follow what the screen already does for "Install automatically" (disabled with the reason beside it) only if the server would still accept Install now; if unsure, hide it and say so in the PR.
2. **The request.** `store.install(version)` posts `{version, when: 'now', confirm_manual}` through the store's existing `run()` pattern (one action at a time, the busy state, stale handling).
   - **Manual steps:** a release marked as needing manual steps asks first in the existing confirm dialog, showing that its notes have steps. Only an explicit confirm sends `confirm_manual: true`. Cancel sends nothing. Never send `confirm_manual: true` without that confirm.
   - **Answers:** 202 → the screen shows the install steps it already renders from the `updates` SSE topic. Map `404 unknown_version`, `409 version_skipped`, `409 install_unavailable` (with its reason, e.g. `busy`, `trust_damaged`, `image_mode`, `updater_unreadable`), `422 manual_confirmation_required` and `500 updates_not_saved` to plain messages in the screen's existing error area. Reuse the reason labels the screen has; add messages only where none exist (en **and zh**: the zh completeness test requires both).
3. **Install in the window:** only if the copy calls for it and the release has `installs_at`, a "when: window" choice. Otherwise leave it out and note it in the decisions file.
4. **Don't** change the server, the schema or any other screen. No retries on failure.
5. **Tests:**
   - Rewrite the `TODO(part 2)` test against the real button (no Install now with damaged trust).
   - vitest: the button posts the right body; hidden while installing; manual steps: Cancel sends nothing, Confirm sends `confirm_manual: true`; each error answer shows its message and leaves the screen usable.
   - Playwright: the keyboard path to Install now on the Updates route with the mock (axe included), if an updates spec exists to extend; condition waits only.
6. **Sizes:** the Updates screen's entries stay under their limits; never raise one. Report deltas.
7. **Decisions file:** `docs/decisions/2026-11-01-S-updates-3.md`: what's wired, the window choice decision, which reasons hide vs disable the button.

## Own
`web/src/lib/features/settings/updates/**` (incl. its messages), an updates e2e spec, your decisions file.
**Don't edit** `features/filters/**`, `features/appearance/**`, `lib/shell/**`, `settings/form/**`, `settings/shell/**`, `settings/screens2/**` (other slices are in them), `crates/**` or `vite.config.*`.

## Size
≲ 400 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for settings*/updates* and core

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
