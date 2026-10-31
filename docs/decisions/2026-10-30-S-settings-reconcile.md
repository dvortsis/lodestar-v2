# S-settings-reconcile decisions (#217)

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

## Budgets (§10.1): split code; three limits outside Settings raised by owner ruling

Every Settings budget is met by splitting code; no Settings limit was raised. The only limits that changed are the three below, outside Settings, by owner ruling. Measured with `npm run build` on this branch, gzipped:

| Entry | Before (848e10a0 on main) | Now | Limit |
|---|---|---|---|
| Settings overview (own JS + CSS) | 9.9 KB | 11.19 KB | 45 KB |
| Settings section (own JS + CSS) | 50.67 KB | 41.32 KB | 45 KB |
| Settings controls (on demand) | 56.5 KB | 35.91 KB | 38 KB |
| Settings edit chunk (on demand), **new** | — | 10.25 KB | 11.3 KB |
| Settings KeyboardScreen (first view), **new** | — | 34.92 KB | 45 KB (the §10.1 route cap) |
| Settings screen extras (on demand), **new** | — | 22.7 KB | 25 KB |

- **The save pipeline loads on demand, as one edit chunk (owner, 2026-10-24: option A, relayed by the runner).**
  - The runner checked this against the owner's own session and recorded it in runner-state's `state.json` (`owner_decision_settings_split`): "draft/conflict code loads on first edit; 45 KB limit kept".
  - It supersedes the "no async seams on the merge and the draft restore" note in `2026-10-23-M-settings-form.md` and its follow-up in `2026-10-23-M-settings-form-2.md`. The 45 KB route budget stays.
  - A stored draft's offer appears once the chunk has loaded, and the merge and the restore stay synchronous after that.
  - The engine change is M-settings-form-3's, applied unchanged:
  - `form/edit.ts` holds `saving.ts`, the merge, validation, reading and restoring drafts, and the UI they show (save bar, error summary, notice, draft offer, conflict and saved lines, questions).
  - It loads with the first edit, a stored draft, a newer `settings` event or a save, and the browser prefetches it when idle.
  - Writing drafts stays eager (`draft-store.ts`).
  - A chunk that doesn't load shows a Reload banner, or a "couldn't load" draft offer with Retry. The form README has the details.
- **The edit chunk has its own entry.** It's new code, so its limit follows the "measured + 10%" rule (10.26 KB → 11.3 KB). "Settings controls (on demand)" leaves the edit chunk out, so it keeps measuring only the controls under its existing 38 KB.
- **Each kind of screen has its own first view, and each is gated (§2.19; runner's review, 2026-10-30).**
  - The registry imports every `screen.ts` eagerly. What a screen shows is a dynamic import made at mount: `SectionForm`, with the form engine, on a generated screen, and the body (`load`, named `*Screen.svelte`) on a hand-built one. `SettingsSection` starts the form's import alongside `loadSettingsIndex()`, so there's no extra waterfall. A hand-built screen never loads the engine.
  - **"Settings section"** is a generated screen's first view: the route, the form and the engine's first view (41.32 KB).
  - **"Settings `<Body>` (first view)"** is a hand-built screen's first view: the route without the form, plus the body's own code. There's one entry per body, generated from the manifest, all against the 45 KB route cap, so a later hand-built screen is gated the moment it lands. Keyboard's is 34.92 KB.
  - **"Settings screen extras (on demand)"** gates General's export panel beyond the section's first view, plus what a body loads only on an interaction: Keyboard's shortcut sheet and its single-key toggle (S-keys' `boot.ts`). It's 22.7 KB, so its limit is measured + 10% (25 KB).
  - **The bodies and extras are named, never pattern-guessed:** a body is `*Screen.svelte`, and the extras are listed by path. `.size-limit.mjs` refuses to measure if any of them is imported statically, so a regex can never hide first-view code from these gates.
  - **The vendor chunk's comment is updated to the measurement:**
    - It is 31.09 KB with this PR (30.81 KB with `848e10a0` merged alone, 30.42 KB before Settings), against the 31.1 KB budget.
    - The growth is Svelte runtime the form engine and Keyboard use (runner's review: not reasonably avoidable). Keyboard stays in the navigation (runner, 2026-10-30).
    - A later route that passes the budget goes to the owner.
- **Three routes outside Settings went over their limits, and the owner raised those three (owner ruling, 2026-10-30, relayed by the runner).**
  - Before the ruling: Alerts route 29.03 KB (limit 29), Alert route 45.77 KB (limit 45), Detail sections 34.41 KB (limit 33.9). On `main` they measured 28.38, 44.95 and 33.82 KB.
  - The cause, from sourcemaps: Settings is now a production route, and Rolldown splits shared chunks by importer set. `ui/fields/Toggle.svelte` leaves the alerts chunk, `format/bytes.ts` is split the same way (the form's memory estimate uses it), and the shared Svelte input-bindings chunk grows. The modules are the same; the extra cost is per-chunk overhead.
  - **Chunk grouping can't fix it (runner, 2026-10-30: try `vite.config.ts` groups first, with no growth on /search).** Every variant was built and measured, then reverted:
    - A group holding Toggle and bytes, or Toggle alone, with dependencies included (Rolldown's default): the group must take Svelte's `bindings/input.js`, which `Button`, `Input` and `Select` import, so /search's first load imports the group chunk. The stray check fails.
    - The same group without dependencies (`includeDependenciesRecursively: false`): Rolldown refuses the build alongside SvelteKit's `preserveEntrySignatures: "strict"`.
    - Moving Toggle back into the Alerts chunk would make Settings load Alerts code.
  - **The owner's ruling (option (a), 2026-10-30):**

    | Entry | Old limit | New limit | Measured with `main` at `fa836c4e` merged |
    |---|---|---|---|
    | Alerts route (own JS + CSS) | 29 KB | 29.1 KB | 29.03 KB |
    | Alert route (own JS + CSS) | 45 KB | 45.8 KB | 45.76 KB |
    | Detail sections (on demand) | 33.9 KB | 34.5 KB | 34.41 KB |

    - No other limit changed. `.size-limit.mjs` cites the ruling and the cause next to the three entries.
    - Alert route has about 40 B of headroom. A later change that adds to it goes back to the owner.

## Tests

- **`shell/screens.test.ts`** covers the registry:
  - the glob finds the five generated screens and Keyboard, in §10.10.2 order;
  - every export shape counts once;
  - group, order and title sorting;
  - invalid and repeated screens are dropped.
- **M-settings-form-3's tests come with the edit chunk:** `lazy.test.ts` (cold loads, the first edit, events and drafts, failure paths), `SettingsForm.offline.test.ts` and the `lazy-imports.test.ts` list.
- **`SectionForm.test.ts`** waits for the save bar, which now arrives with the edit chunk.
- **`shell/SettingsSection.test.ts`** uses a fake registry to cover:
  - a hand-built screen's loading state, which shows its title as the `<h1>`, and its body taking over with exactly one `<h1>`;
  - its load failure, which keeps the title and shows the Reload banner;
  - a generated screen with its purpose, notice, form and extra, and an extra that fails;
  - a body already loaded renders at once on a return visit, without the loading line;
  - switching from a hand-built screen to a generated one;
  - an unknown id, which loads nothing.
- **`shell/SettingsSection.failure.test.ts`:** a generated screen whose form code can't load shows Reload.
- **`lazy.test.ts`** adds: an event, then a blur, on a form destroyed before the edit chunk is in fetches nothing afterwards. The engine's deferred `blur` and `received` now return early once the form is destroyed.
- **Playwright:** `settings.routes.test.ts` runs at 390 and 1366 px. That includes opening Keyboard from the Settings navigation, with one `<h1>`, no horizontal scroll and axe. The `settings-form` specs (routes, fixture, keyboard; axe in Riverstone light and dark at 390 and 1366) and the shortcuts and boot-shell specs pass. No `waitForTimeout`.
- **The Settings navigation changed visually** (the merged Overview, the Preferences group and Keyboard). The `smoke settings` baselines were regenerated by CI (`web/e2e/.update-snapshots`, all themes) and reviewed.
- **`soak-followups.routes.test.ts`** asserted the old "not available in this build" placeholder at `/settings`. It now checks the Overview's sections and the Back to Settings path at 390 px.
