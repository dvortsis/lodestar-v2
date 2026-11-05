# RP-6: Settings → Filters & Safety, Packs: versions, changelog, pending versions, override editor with diff, corpus status
**Model:** Sonnet (UI on a settled API). `/security-review` covers the weakening and acceptance dialogs, and a fresh Opus reviewer follows. Runner brief, 2026-11-05T10:20Z.

## Why
RP-3 (#478, merged at e5bc6d5e) added the API: pack versions and changelogs, per-rule state, overrides (`pack_override`), pending versions, `accept_pack_versions` (strengthening or weakening), re-confirming paused overrides, the review queue, and `GET /filters/fixtures`. Users need a screen for it. Until then, MW-6 and RP-1b's versions stay pending on upgraded installs, with no way to accept them.

## The spec
The "RP-6" bullet in `docs/plan/2026-11-04-v06-rulepacks.md`, plus architecture §20.7, §20.10, §10.10, §10.10.7 and §10.14. Read these too:
- `docs/decisions/2026-11-05-RP-3.md`: what is strengthening and what is weakening, the `shadow-in-doubt` state, and the review queue;
- `docs/decisions/2026-10-23-M-filters-dialogs.md`: the existing effect-warning and password dialog. Reuse it; don't build a new one.

## Do (branch `slice/RP-6` from `origin/main`)
1. **One panel per pack** under `web/src/lib/features/filters/screens/packs/`:
   - version, "What changed" (the changelog, rendered as text, never HTML), and corpus status from `/filters/fixtures`;
   - its rules, each with a toggle, the action, an override editor (an action select, plus `unless` through the existing query builder), and a diff against the base;
   - overrides whose base changed are flagged, and paused ones get **Confirm again**.
2. **Pending versions** show as a notice with the impact preview from `POST /filters/preview`, and an **Accept** button.
   - Ask for the password **only when the API says the acceptance weakens.** The UI must never decide "strengthening" by itself.
   - `in doubt` (`?`) is shown plainly as a reason, for example "We can't confirm this change is safe, so it needs your password".
   - `changed_since_preview` re-asks.
3. **Weakening** opens the existing effect-warning and password dialog. Plain-language text per §10.14. Status is shown with an icon and text. en and zh message keys. Tokens only, no hex.
4. **Mount** with one line in `screens/Switches.svelte`, which is a shared file.
5. **Size:** keep the screen lazy, so the Filters first view stays within budget (~31.07 kB now). Add a size-limit entry for the packs chunk. Don't raise existing limits. If you must, ask the runner first.
6. **Tests:**
   - vitest for each state: no pending, a pending strengthening version, a pending weakening version, in doubt, a paused override, a base that changed, and `changed_since_preview`;
   - a Playwright spec with the e2e mock for accepting a weakening version (password asked) and a strengthening one (not asked);
   - axe and the keyboard;
   - no fixed sleeps.
7. **User guide:** the Filters chapter's packs section (the `user-docs` skill): what a pack version is, why you're asked for your password, and what "in doubt" means.

## Own
`web/src/lib/features/filters/screens/packs/**`, the one mount line in `screens/Switches.svelte`, the filters message keys for these screens, the e2e mock and spec for them, the size-limit entry, and the user guide's packs section. **Don't touch** `crates/**`. If the API is missing something, message the runner.

## Done when
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` and Playwright for your spec (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`). Then run `/security-review` on the dialogs.

Open a PR against `main` titled "RP-6: Settings: Filters & Safety, Packs: versions, changelog, pending versions, override editor with diff, corpus status". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
