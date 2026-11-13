# X-wizard-labels-signout (build slot; Opus; owner priority, relayed 2026-11-13)

Branch `slice/X-wizard-labels-signout`, PR titled `X-wizard-labels-signout: Wizard field labels and Sign out`.
Follow `.claude/skills/steward/SKILL.md`. Tell the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when the PR is ready.

## Why
The 2026-11-13 UI/UX review found two blocking defects. The owner put them first, ahead of every other UI finding.
Evidence: `git fetch origin claude/codebase-review-bugs-x9ff7p` and read
`5c3e447a:docs/review/2026-11-13-ui-ux/01-review.md` lines 19-26 (B1, B2). Don't merge that branch; the runner re-commits the evidence.

## B1. The first-run wizard shows raw config keys as field labels
- What: `/welcome` → Crawl intensity shows `intensity` and options `low / balanced / aggressive`; Storage & retention shows `archive_raw` and `mmap_mib` with no help; field menus read "Options for archive_raw". Settings shows the same fields as "Crawl intensity" and "Keep raw metadata" with help.
- Where: `web/src/lib/features/wizard/FieldsStep.svelte` builds its form with `createSettingsForm({ id, fields: keys, drafts: false })` from `../settings/form/embed` and renders `SettingField` per `form.specs`. Bare keys reach it without the label/help/option message keys the Settings screens give.
- Fix: give the wizard the same specs (label, help, option labels, menu name) the Settings screens use, from one shared source, not a copy. Find where the Settings sections define them and reuse that.
- Tests:
  - vitest: for every wizard step that uses FieldsStep, no rendered label, option label or menu name equals its setting key (or the key's last segment), and every field the Settings screen shows with help also has help in the wizard.
  - e2e on the existing wizard spec: the Crawl intensity and Storage steps show the human labels.

## B2. There is no way to sign out
- What: `web/src/lib/features/auth/LogoutButton.svelte` exists (busy state, error toast; props `variant` quiet|secondary, `dense`; needs the shell's `<Toaster>`), but nothing imports it.
- Fix:
  1. Add "Sign out" to the overflow menu, `web/src/lib/shell/OverflowMenu.svelte` (today it maps only `MORE` routes through `Menu` and `goto`). It's an action, not a route, so handle it in `onSelect` and keep the Bits UI menu lazily loaded as now. Reuse LogoutButton's logic (extract a shared `signOut()` if the button can't sit in a menu item); don't duplicate the request.
  2. Add a "Sign out" (this device) action to Settings → Security → Sessions (`web/src/lib/features/settings/security/Sessions.svelte`, wiring in `SecurityScreen.svelte`, strings in `security/messages/en.json`), beside the existing "Sign out other sessions".
  3. After signing out, land on the sign-in screen; a failure shows the error toast and stays.
- Tests: vitest for the menu item and the Sessions action (success and failure); e2e: sign out from the overflow menu at 1366 px and at 390 px lands on sign-in, and a reload stays signed out.

## Rules
- Tokens only, no hex, no arbitrary values; spacing on the §10.2 ramp. Icons follow the existing `~icons/ph/*` style used next to them.
- Copy: plain words, "Sign out". Any new string goes in every locale file the way the repo already does it (paraglide); don't hand-translate beyond what the existing pattern requires.
- No fixed sleeps; condition waits with deadlines ≥ 10 s.
- Size budgets: `npx size-limit` must stay green; the overflow menu stays lazy.
- A decisions file under `docs/decisions/` only if you change a decision; otherwise none.

## Checks before every push (in `web/`)
`npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium npx playwright test` for the touched specs. Never run `playwright install`.

## Owns
`web/src/lib/features/wizard/**`, `web/src/lib/shell/OverflowMenu.svelte` (and `nav.ts` only if needed), `web/src/lib/features/auth/**`, `web/src/lib/features/settings/security/**`, the shared spec source for the wizard fields (read-only reuse preferred; a minimal export is fine), the matching message files, and the touched e2e specs.

## Don't
- Touch the other UI/UX findings (B3, B4, Important): they're separate slices.
- Make cloud calls, read AWS_* variables, or use AWS MCP tools.
