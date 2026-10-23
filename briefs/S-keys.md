# S-keys: Keyboard remapping and shortcut sheet
**Model:** Sonnet (helper: UI over W9's key registry and B2b's `preferences` section; the server validates every remap). **Branch:** `slice/S-keys`. No `/security-review`: there's no auth, filters or untrusted input; the server checks shortcut keys (`preferences.rs` `check_shortcuts`).

**Spec:**
- `runner/standards.md`, `docs/owner-decisions.md`, and CLAUDE.md's UI rules.
- Architecture:
  - §10.16 (the map; "Single-key shortcuts can be turned off or remapped");
  - §10.13 (the shortcuts bullet: WCAG 2.1.4; not colour alone; focus);
  - §10.10.8, the **Keyboard** row: single-key shortcuts on/off, remapping with conflict detection, reset; section `preferences`; owner S-keys;
  - §10.10.1, §10.10.4, §10.10.5 (a field, saving, `if_revision` conflicts) and §10.10.11 (accessibility and tests) for the screen;
  - §10.10.3's palette bullet ("Turn off single-key shortcuts");
  - §10.11 (`Kbd`);
  - §10.14.
- `docs/ui-glossary.md`, `docs/design-tokens.md` §6 and §8 (mono for shortcuts).
- What exists on `main`:
  - **The registry** (`web/src/lib/keys/`):
    - its `README.md`;
    - `registry.svelte.ts`: `register`, `pushScope`, `list()` → `{id, title, section, scope, keys, keywords, palette, active}[]` (reactive), `remap(id, combos | null)`, `setSingleKeys`, `singleKeys`, `conflicts()`;
    - `combo.ts`: `normalizeCombo` lowercases letters, so `J` means `j`, and `shift+j` is distinct;
    - `display.ts` (`formatCombo`, ⌘ on a Mac);
    - `defaults.ts`.
  - **The shell:** `web/src/lib/shell/Chrome.svelte` loads `$lib/keys` and the shell's commands on the first interaction (or after 30 s), attaches the listener and replays the first key. `web/src/lib/shell/README.md` names S-keys's hand-off.
  - **The server's preferences:**
    - `crates/lodestar/src/settings/preferences.rs`: `shortcuts: bool`, `shortcut_keys: {action: key}` with `x-widget: shortcuts` and group `keyboard`, `DEFAULT_SHORTCUTS`, and `check_shortcuts`: one visible character, no control, whitespace or invisible characters, at most 64, and every action's effective key unique among the defaults and the remaps.
    - `docs/decisions/2026-10-22-B2b.md` ("Preferences", "Shortcut keys").
    - The API: `GET /settings/preferences` returns `SectionValues {fields: {name: SettingValue {value: unknown, …}}, revision}`. `PATCH /settings/preferences` takes the body `{field: value, if_revision?}` (`crates/api/src/settings.rs` `IF_REVISION`); its errors are 409 `stale_revision` (with `current`), 422 `invalid_setting` and 409 `read_only`. The `settings` event `{section, revision}` comes from `web/src/lib/state/events.svelte.ts` (`events.on('settings', …)`).
    - `SettingsPatch` is generated as `Record<string, never>`, so a typed body needs a cast. Note it in the PR as a schema gap; don't write a local type.
  - **Primitives:** `web/src/lib/ui/overlays/Dialog.svelte`, `web/src/lib/ui/labels/Kbd.svelte`, `web/src/lib/ui/fields/Toggle.svelte`, `web/src/lib/ui/controls/Button.svelte`, `web/src/lib/ui/feedback/{Banner.svelte,toasts.svelte.ts}`.
- Skills: the standards §3 "Any screen or primitive" row.

## Own
- **`web/src/lib/features/shortcuts/**` (new) and its tests:**
  - `boot.ts`: registers `help.shortcuts` and the palette toggle, applies the saved preferences;
  - `ShortcutSheet.svelte` (lazy);
  - `ShortcutsField.svelte`: the `x-widget: shortcuts` editor;
  - `KeyboardScreen.svelte` and `screen.ts`;
  - `catalog.ts`: the remappable actions;
  - `messages/{en,zh}.json`, with the prefix `shortcuts_`.
- `web/src/dev-routes/shortcuts/**`: a harness page for the screen.
- e2e: `web/e2e/shortcuts.routes.test.ts`, `web/e2e/shortcuts.keyboard.test.ts` and `web/e2e/shortcuts.fixture.test.ts` (local only).
- `docs/decisions/<date>-S-keys.md`.
- **`web/src/lib/keys/defaults.ts`, its `README.md` examples and `keys.test.ts` ids** (step 1). `lib/keys` is unowned since W9 merged; it's yours in this batch.
- **Hub lines, exactly these:**
  - **`web/src/lib/shell/Chrome.svelte`:**
    - one more entry in the first-interaction `Promise.all`: `import('$lib/features/shortcuts/boot')`;
    - after `registerShellCommands()`: `offShortcuts = shortcutsBoot.startShortcuts()`;
    - `offShortcuts?.()` in the cleanup.
  - **`web/src/lib/shell/README.md`:** the S-keys hand-off line (the new id).
- **Don't edit:**
  - any other `lib/shell` file;
  - `web/src/lib/state/**`, `web/src/lib/api/**`, `web/src/lib/ui/**`;
  - `web/src/routes/settings/**` (S-settings-1, which hasn't started);
  - `web/src/lib/features/settings/**` (M-settings-form, possibly in flight);
  - `features/search` and `features/detail` (S-search-1 and S-detail, in flight);
  - `web/e2e/support/routes.ts` (your page is a dev route, outside `src/routes`);
  - `package.json`, `.size-limit.mjs`, `playwright.config.ts`.
- **Open PRs nearby:** #48 M4, #128 B-update and #129 B-log2 part 2 touch no file here beyond the generated `openapi.json`/`schema.d.ts`. Merge `origin/main` and never hand-merge those files. #130 merged as `57296e6`.
- **In-flight neighbours:**
  - **S-detail** registers `detail.copy_magnet` and `detail.open_magnet`, with explicit `keys` so it works before or after your rename.
  - **S-search-1** registers `search.focus`.
  - **M-settings-form** builds the settings form engine and widget registry (`web/src/lib/features/settings/form/`).

## Build
1. **Use the server's action ids.** The ids the server stores and checks (`DEFAULT_SHORTCUTS`) differ from W9's `DEFAULT_KEYS`, and nothing registers W9's ids yet, so rename them in `defaults.ts`:

   | W9 id | New id |
   |---|---|
   | `list.next` | `results.next` |
   | `list.previous` | `results.previous` |
   | `magnet.open` | `detail.open_magnet` |
   | `magnet.copy` | `detail.copy_magnet` |
   | `client.send` | `detail.send` |
   | `row.select` | `results.select` |
   | `shortcuts.open` | `help.shortcuts` |

   - Keep `search.focus`, `detail.open` (Enter), `dismiss` (Esc) and `palette.open` (⌘/Ctrl+K). They aren't single characters, and the server's list leaves them out.
   - Update the README examples, `keys.test.ts` and the shell README line.
   - Add a test that the single-character entries of `DEFAULT_KEYS` equal the eight pairs in `preferences.rs` `DEFAULT_SHORTCUTS`, with a comment naming that file. Then a drift on either side is caught by review of the test.
   - Record the rename. The persisted contract is the server's.
2. **`catalog.ts`:** the eight remappable actions, each with its id, its default key from `DEFAULT_KEYS` and a title message (`shortcuts_action_results_next`, …), in §10.16's order.
   - The Settings screen lists these, **not** `keys.list()`: on the Settings page the search and detail commands aren't registered.
   - The sheet uses `keys.list()` (step 4).
3. **`boot.ts`: `startShortcuts()`.** The shell calls it on the first interaction (hub line). It returns the stop function.
   - **Synchronously:**
     - register `help.shortcuts` (global; `?` from `DEFAULT_KEYS`; title "Keyboard shortcuts") to open the sheet;
     - register `shortcuts.single_keys` ("Turn off single-key shortcuts" or "Turn on…", palette only, `palette: true`, no key);
     - apply the cached preferences from `localStorage` `lodestar.shortcuts` `{shortcuts, shortcut_keys}`. Wrap every read and write in try/catch: it's a per-viewer cache, and the server stays the source of truth.

     Doing this synchronously means the replayed first key already sees the user's mapping.
   - **Then:**
     - `GET /settings/preferences`;
     - apply `shortcuts` with `keys.setSingleKeys(value)`;
     - apply `shortcut_keys` with `keys.remap(id, [key])` for each entry, and `remap(id, null)` for every catalog action not in it;
     - update the cache.
   - **Narrow `value: unknown` at runtime** (a boolean; a record of strings with ids matching `^[a-z][a-z0-9_.]*$`). Anything else is ignored and logged once.
   - **On `events.on('settings')` for `section === 'preferences'`**, fetch and apply again. That covers another tab, another device and a palette toggle.
   - **The palette toggle:** `PATCH /settings/preferences {shortcuts: !current}`, without `if_revision` (a single-field toggle). Then toast "Single-key shortcuts are off" or "on". A failure shows an error toast and changes nothing locally.
   - **Errors:** a 401 is the shell's. A network error keeps the cached state and stays quiet: the keys still work.
4. **The shortcut sheet** (`ShortcutSheet.svelte`, loaded on the first `?`):
   - **The container:** a `Dialog` titled "Keyboard shortcuts". Focus is trapped and restored, and Esc closes it. Bare keys are already muted inside a dialog, so `?` can't stack it.
   - **The contents:**
     - `keys.list()` filtered to commands with keys, grouped by `section`, with the active ones first and inactive ones under "On other screens";
     - each row shows the title and its combos through `formatCombo` in `Kbd`, mono;
     - conflicts from `keys.conflicts()` show as a warning `Banner`, icon plus text, naming both commands.
   - **When `keys.singleKeys` is false:** an info line "Single-key shortcuts are off", the single-key rows marked "Off" as text (not dimmed alone), and a "Turn on" button that runs the palette toggle's code.
   - **A "Change shortcuts" link** to `/settings/keyboard`. It's provisional: it shows 404 until S-settings-1's route exists, as W9's overflow targets do. Say so.
5. **The editor** (`ShortcutsField.svelte`). It's controlled: `value: Record<string,string>`, `onchange(next)`, `errors?: Record<string,string>` and `disabled?`. It doesn't save.
   - **M-settings-form's widget contract** (in flight; per its brief): `registerWidget(name, Component<WidgetProps>)` from `features/settings/form/`, with `WidgetProps { form, key, controlProps, dense? }`. S-keys registers the `'shortcuts'` widget.
   - **If the engine is on `main` when you start,** add `ShortcutsWidget.svelte`: a thin adapter from `WidgetProps` to `ShortcutsField`, reading and writing the field through `form`, with `locked` and `error` coming from the field state. Register it from `screen.ts`'s loader.
   - **If not,** leave the adapter as a named follow-up, so `ShortcutsField` itself stays engine-free.
   - **One row per catalog action:** the title, the effective key in `Kbd` (the remap, else the default), "Change", and "Reset" (shown when remapped). A "Reset all" sits at the end.
   - **Capture:**
     - "Change" enters capture mode. It says "Press a key" through a polite live region, and sets `aria-pressed` on the button.
     - The next keydown is taken with `preventDefault`. A single visible character with no Ctrl, Alt or Meta is accepted.
     - **Letters are stored in lower case.** Shift plus a letter is refused with "Use a key without Shift": the registry folds case, so `J` would act as `j`.
     - Whitespace, control and invisible characters are refused, mirroring `preferences.rs` `invisible`.
     - Esc and Tab cancel the capture. Tab stays a way out.
   - **Conflict detection mirrors `check_shortcuts`:**
     - every action's effective key must be unique among the eight;
     - a clash names the other action ("Already used by Copy magnet") with an icon and text, and offers "Swap" or "Cancel".
     - Also warn, without blocking, when the key collides with a non-remappable single-key command currently registered (`keys.list()`).
   - **The server's own 422 message** shows under the row it names, or under the field.
6. **The Keyboard screen** (`KeyboardScreen.svelte` plus `screen.ts`):
   - **The order:** the heading and one purpose sentence (§10.10.2), then the "Single-key shortcuts" `Toggle` (`shortcuts`), with help text explaining WCAG 2.1.4 plainly, then the `ShortcutsField`.
   - **Saving.** If `web/src/lib/features/settings/form/` is on `main` when you start, build the screen on that engine (`createSettingsForm`, `SettingsForm.svelte`, the `'shortcuts'` widget). Otherwise save directly:
     - Explicit Save and Cancel buttons; the form stays dirty until saved.
     - Send `PATCH /settings/preferences {shortcuts, shortcut_keys, if_revision}`.
     - A 409 `stale_revision` reloads the values and says "Changed in another tab or device; your edits were kept" (the edits are re-applied over the new values).
     - A 422 maps to the field.
     - A success applies at once through boot's apply function.
     - Record that the screen moves onto the engine when M-settings-form lands. Name it as a follow-up, for S-settings-1 or the polish slice.
   - **Values set elsewhere:** a field the environment sets, or that's `read_only`, shows locked with its source (glossary §5 labels).
   - **`screen.ts`** exports `{ id: 'keyboard', group: 'preferences', title: () => m.shortcuts_screen_title(), load: () => import('./KeyboardScreen.svelte') }`. That's the shape slices.yaml's note asks for ("the route finds it through the screen registry … the feature folders that export a settings screen").
     - S-settings-1 defines the registry. Record this shape as the proposal, so it can be adopted or adapted.
     - Until then the screen is reachable only on `/dev/shortcuts`.
7. **Accessibility:**
   - Every control has a name.
   - Key text is in `Kbd` with an accessible name ("Slash", "Question mark", "Ctrl+K"; add a small name map for symbols).
   - Capture mode is announced.
   - Conflicts and errors show icon plus text.
   - Targets are 44 px under `pointer: coarse`, and focus is visible.
   - Nothing fires while you type in the capture field, or in any field (the registry's rule).
8. **Budgets:**
   - **CSS:** the global 20 KB sheet is at 19.82 KB and is the owner's number. Use only utilities the build already emits, and write anything new in component `<style>` blocks with `@reference` to `app.css` and `@apply`. If it doesn't fit, stop and ask.
   - `boot.ts` loads with the shell's first-interaction chunk, so keep it small (≤ 1.5 kB gzipped; report it). It must not import the sheet, the screen or the editor statically.
   - The sheet and the screen are lazy chunks.
   - `npx size-limit` must not move the search route's entries. Lighthouse never interacts, so it doesn't load these.
   - If boot can't stay small, stop and report.
9. **Messages:** en in full (the titles of the eight actions, the sheet, the screen, the errors, the toasts), and zh proposals for your own keys. Use glossary words where they apply.
10. **Decide yourself, and record them:**
    - the id rename;
    - the cache key;
    - lowercase letters;
    - Swap on conflict;
    - the provisional link;
    - the `screen.ts` shape;
    - the Kbd names.

    **Stop and ask:**
    - any change to the registry's behaviour, beyond the id rename;
    - a server or schema change. That includes the `SettingsPatch` typing, which you report, and an action id the server doesn't know;
    - editing settings-route or form-engine files;
    - any budget overrun;
    - a new dependency or token.

## Tests
- **vitest:**
  - **The rename:** the parity test with `DEFAULT_SHORTCUTS`; `keys.test.ts` passes with the new ids.
  - **`boot`:**
    - synchronous registration;
    - the cached apply, and storage that throws;
    - the server apply (`remap` and `setSingleKeys` called, and reset for removed remaps);
    - a malformed value ignored;
    - the refetch on the settings event;
    - the palette toggle's PATCH, success and failure.
  - **The editor:**
    - capture accepts `n` and `/`, and refuses Shift+J, Ctrl+K, Space, U+200B and a combining mark;
    - Esc and Tab cancel;
    - the conflicts from `preferences.rs`'s own test cases;
    - Swap;
    - Reset and Reset all;
    - `errors` rendered.
  - **The screen:** the PATCH body carries `if_revision`; 409 stale with the edits kept; 422 mapping; a locked env field.
  - **The sheet:** grouping, the active and inactive split, `formatCombo` on Mac and elsewhere, the conflict banner, the single-keys-off state.
- **Playwright** (mock harness, on a private port: a copied config with `--strictPort`):
  - **`shortcuts.routes.test.ts`:**
    - on `/search`, after a first key, `?` opens the sheet; it lists Ctrl+K and `?`; Esc closes it and focus returns;
    - axe passes (light and dark, 1366 and 390) on the sheet and on `/dev/shortcuts`;
    - mock `getSettings` (the preferences section) and `patchSettings`.
  - **`shortcuts.keyboard.test.ts`:**
    - on `/dev/shortcuts`, with a sample command registered by the harness: remap `j` to `n` by keyboard only, then save, and `n` fires the command while `j` no longer does;
    - choosing `c` shows the conflict;
    - turning single-key shortcuts off stops `n`, and Ctrl+K still opens the palette.
- **Fixture backend** (`shortcuts.fixture.test.ts`, local only; skipped without `E2E_FIXTURE_URL`):
  - Run it against `serve --fixture` on a private port (`bench/README.md`).
  - Save a remap, reload, and check it holds (the real `PATCH` and `GET`).
  - The server refuses a clash with a 422, and the message shows.
  - Report the results.
- **No new baselines.** No key screen changes on load.

## Done when
- These pass in `web/` after merging `origin/main`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` (the search route unchanged) and `npx playwright test` (yours, plus `shell.keyboard.test.ts` and `boot-shell.routes.test.ts`, which cover the Chrome hub line).
- Axe is clean.
- No hex, arbitrary or off-ramp values, and strings only in `features/shortcuts/messages`.
- About 1,500 changed lines or fewer.
- A PR titled `S-keys: Keyboard remapping and shortcut sheet`.

## Report
The PR description lists:
- **The rename:** the old and new ids, and who registers each one: S-search-1 `search.focus`; S-search-2 `results.*` and `detail.open`; S-detail `detail.copy_magnet` and `detail.open_magnet`; nobody yet for `detail.send`.
- **The APIs:** the boot API and the Chrome hub lines; the editor's props, for the widget registry; and the proposed `screen.ts` shape.
- **The save path,** and the follow-up to move it onto M-settings-form.
- **Measurements:** chunk sizes (boot, sheet, screen), axe results and the fixture-backend run.
- **Schema gaps:** the `SettingsPatch` typing, and `value: unknown`.
- **Owner decisions:** any refinements (§2.19).

## Runner decisions (2026-10-23, apply over anything above)
- **CSS (owner's §10.1 limit, unchanged):** global CSS is 19.82 of 20 KB and nine web slices are in flight, so treat the headroom as zero. Don't add Tailwind utility classes that aren't already in main's built CSS. Put everything route-only in a component `<style>` block with `@reference` to `app.css`, using token variables and ramp spacing only (no px, no hex). It ships in the route chunk. Report `npx size-limit`'s CSS figure before and after. If it moves at all, say why in the PR; if you can't stay under 20 KB after merging main, stop and ask.
- **Route budgets (runner, §2.19):** each route's own JS + CSS is ≤ 15 KB gzipped (compare, discover, alerts, alerts/[id], t/[hash], drawer), and Insights is ≤ 45 KB including uPlot (§10.1). Dialogs and heavy pieces load lazily.
- **Fixture backend:** CI uses the OpenAPI mock. Do one manual check against `serve --fixture` on a private port and report it. Fixture specs stay opt-in through `E2E_FIXTURE_URL`; S-e2e will wire them into CI.
- **Shared files:** edit only the lines this brief assigns you in a shared file. On a conflict, merge `origin/main` and regenerate generated files (messages, OpenAPI, size entries) with the repo's tooling; never hand-merge them.
- **On main since this brief was drafted:** W9 (b8c884d) and B11b part 2 (#130, 57296e6: the Insights Δs, `export.csv` and the `insights` SSE event). M4 (#48, swarm fields) is still in review: use its fields only if it has merged when you start, otherwise use the null state.
- **Confirmed:** rename the web's command ids to the server's persisted shortcut ids (the server is the contract), and add a parity test against the server's list.
