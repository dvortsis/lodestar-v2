# S-settings-reconcile decisions

One Settings implementation, built on the owner's `848e10a0` (`work/soak-settings`, "preserve prepared screens and validation status"). It also takes what that commit lacked from the paused S-settings-1 and M-settings-form-3 (`runner-state:ports/`): the screen registry contract, the on-demand gating of a section's first view, and the save pipeline as an on-demand edit chunk. Architecture §10.1, §10.10.1, §10.10.2, §10.10.5 and §10.10.11. It supersedes `2026-10-23-M-settings-form-2.md` § "The save pipeline stays in the first view". Items marked **(§2.19)** refine an owner decision; the runner flags them to the owner.

## Base

- **`848e10a0` is merged, not rebased** (CLAUDE.md, shared branches). It brings files outside this slice's paths: `.gitignore`, `docs/plan/2026-10-27-settings-preparation.md`, its decisions file, `web/src/app.html` and `web/src/lib/shell/nav.ts`. The last two point the Settings link at `/settings`, the Overview (§10.10.2). They are taken as the owner wrote them.
- **The conflicts were resolved toward `main`:**
  - `.gitattributes` and `scripts/first-load.mjs` keep main's lines.
  - In `settings/messages/*.json`, main's uTP wording ("uTP (not active yet)", F-status) wins over the owner's earlier "Enable uTP". The owner's other new field labels are kept.
  - `form/message-map.ts` is regenerated from the merged `en.json`.
- **Tests follow the merged labels.** Specs that named raw keys (`dht_port`, `http_bind`, `timeout_secs`) or "Enable uTP" now use the labels the merge shows.

## The screen registry (§10.10.2)

- **Screens are found with `import.meta.glob`** over `features/settings/*/screen.ts` and `features/*/screen.ts` (`shell/screens.ts`). A module exports `screen`, `screens` or a default of either. `/settings/[section]` renders whatever the registry holds, so later screen slices (S-settings-2, S-logs, S-about, S-updates, M-security-settings, S-appearance) add a `screen.ts` and never touch the route.
- **The contract is S-settings-1's, which S-keys already ships:**
  - Every screen has `{ id, group, title }`, plus either `load` (a hand-built screen) or `sections` (one generated from the schema).
  - A hand-built screen is a dynamic import, and it renders its own `<h1>`. Keyboard (`features/shortcuts/screen.ts`) now appears in the navigation under Preferences.
  - A generated screen can add `purpose`, `icon`, `readOnly` and `notice` (Network's read-only line, which used to be hard-coded on `id === 'network'`), and `extra`, a lazily loaded panel after the form (General's export).
  - An invalid screen, or a repeated id, is left out and named in development.
- **Groups and order follow §10.10.2:** Preferences, Crawling, Library, Connections, System, and the screen order inside each group (`SCREEN_ORDER`). An unlisted id goes last in its group, by title. Preferences and Connections get labels now, so a later screen needs no shell change.
- **The five generated screens moved to `features/settings/screens/screen.ts`,** each with its icon. The shell no longer keeps a parallel icon array.
- **Settings code says "Settings" with its own key** (`settings_shell_title`), not the shell's `shell_nav_settings`. Sharing that one message split the shell's navigation-message chunk for every route.

## Budgets (§10.1): split code, no limit raised

All of `web/.size-limit.mjs`'s existing limits are unchanged. Measured with `npm run build` on this branch, gzipped:

| Entry | Before (848e10a0 on main) | Now | Limit |
|---|---|---|---|
| Settings overview (own JS + CSS) | 9.9 KB | 11.2 KB | 45 KB |
| Settings section (own JS + CSS) | 50.67 KB | 40.59 KB | 45 KB |
| Settings controls (on demand) | 56.5 KB | 35.92 KB | 38 KB |
| Settings edit chunk (on demand), **new** | — | 10.26 KB | 11.3 KB |
| Settings screen bodies (on demand), **new** | — | 42.56 KB | 47 KB |

- **The save pipeline loads on demand, as one edit chunk** (owner, 2026-10-24, option A, relayed by the runner; it was recorded in M-settings-form-3's unmerged decisions file, `runner-state:ports/M-settings-form-3.patch`). It supersedes the "no async seams" note in `2026-10-23-M-settings-form.md`, and the 45 KB route budget stays. The engine change is M-settings-form-3's, applied unchanged:
  - `form/edit.ts` holds `saving.ts`, the merge, validation, reading and restoring drafts, and the UI they show (save bar, error summary, notice, draft offer, conflict and saved lines, questions).
  - It loads with the first edit, a stored draft, a newer `settings` event or a save, and the browser prefetches it when idle.
  - Writing drafts stays eager (`draft-store.ts`).
  - A chunk that doesn't load shows a Reload banner, or a "couldn't load" draft offer with Retry. The form README has the details.
- **The edit chunk has its own entry.** It's new code, so its limit follows the "measured + 10%" rule (10.26 KB → 11.3 KB). "Settings controls (on demand)" leaves the edit chunk out, so it keeps measuring only the controls under its existing 38 KB.
- **A screen's body and extras are measured on their own (§2.19).**
  - The registry imports every `screen.ts` eagerly, so a section's first view would otherwise count every hand-built screen's dynamic `load` (Keyboard's 5.9 KB) and General's export panel, which only load on their own screen.
  - `settingsFirstView` stops at them. "Settings screen bodies (on demand)" gates them at measured + 10% (42.56 KB → 47 KB).
  - A later hand-built screen whose body would push this entry over adds its own entry.
- **Measured but not fixed here: three routes outside Settings go over their limits.**
  - Alerts route is at 29.03 KB (limit 29), Alert route at 45.77 KB (limit 45), and Detail sections at 34.41 KB (limit 33.9). On `main` they measure 28.38, 44.95 and 33.82 KB.
  - The cause, from sourcemaps: Settings is now a production route, and Rolldown splits shared chunks by importer set. `ui/fields/Toggle.svelte` leaves the alerts chunk, `format/bytes.ts` is split the same way (the form's memory estimate uses it), and the shared Svelte input-bindings chunk grows. The modules are the same; the extra cost is per-chunk overhead.
  - The fix is in shared files this slice doesn't own (`vite.config.ts`, `scripts/first-load.mjs`) or in those routes' limits. It went to the runner (see the PR).

## Tests

- **`shell/screens.test.ts`** covers the registry:
  - the glob finds the five generated screens and Keyboard, in §10.10.2 order;
  - every export shape counts once;
  - group, order and title sorting;
  - invalid and repeated screens are dropped.
- **M-settings-form-3's tests come with the edit chunk:** `lazy.test.ts` (cold loads, the first edit, events and drafts, failure paths), `SettingsForm.offline.test.ts` and the `lazy-imports.test.ts` list.
- **`SectionForm.test.ts`** waits for the save bar, which now arrives with the edit chunk.
- **Playwright:** `settings.routes.test.ts` runs at 390 and 1366 px. The `settings-form` specs (routes, fixture, keyboard; axe in Riverstone light and dark at 390 and 1366) and the shortcuts and boot-shell specs pass. No `waitForTimeout`.
- **No visual change beyond the merged screens and Keyboard's new navigation item,** so all-themes isn't rerun here.
