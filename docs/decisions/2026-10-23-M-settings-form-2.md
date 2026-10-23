# M-settings-form follow-up decisions (engine diet, time-range)

The runner's follow-up to the Settings form engine (`web/src/lib/features/settings/form/`, architecture §10.10.5, §10.10.11, §10.1). S-settings-1 measured `/settings/[section]` over the owner's 45 KB route budget, so the engine loads on demand what a section's first view doesn't need, and the client learns B-update's `time-range` format. Items marked **(§2.19)** refine an owner decision and are for the runner to flag.

## What loads when
- **The save pipeline stays in the first view.** The owner's Budget decision (`2026-10-23-M-settings-form.md`) chose the 45 KB route budget over async seams on the merge and the draft restore. So the pipeline (`saving.ts`: gates, the PATCH, refusals, Undo, the merge, drafts, validation) and the UI it shows (the save bar, the error summary, the banners, the draft offer, a field's conflict and saved lines, the questions) are static imports, as on main. Loading them on demand as one edit chunk is parked on `slice/M-settings-form-3` until the owner answers the runner's question (numbers below).
- **The store's state is an `Engine` class** that is itself the `SettingsForm` a screen holds, so the parts that load on demand can work on it. `saving.ts` holds the pipeline; `form.svelte.ts` the rest.
- **Every built-in control but the toggle and the text input loads per type**: number, slider, the enum controls (segmented, radio and select in one file), secret, chip list and JSON. `SettingsForm.svelte` starts the ones a form needs as soon as it knows its fields (while the values load), so inside a form they're normally in place at first paint. A field rendered before then shows a hidden box of the control's height: an input's for the controls that share the control height, a row per option for a radio group, the four-row Mono textarea for JSON. A chip list that already has chips grows by its chip row when the control arrives.
- **Instant mode (`instant.ts`) loads with the first instant change**, and on an instant form when the browser is idle after the first view. Changes made while it loads keep their order on its chain. If it can't load, the value goes back with "Part of this page couldn't load…", as a failed send does.
- **Previews (`preview.ts`) load during the first preview's 300 ms debounce**, with what they say (estimates, warnings, "Automatic · value").
- **The field menu loads when first opened** (`menu.ts`: the menu, its labels and the "Show keys" list). **⌘/Ctrl+S's registration loads with the shortcut registry** (`save-key.ts`). **Copy's toast loads on the first copy**; the clipboard write stays in the click, where Safari allows it. **The `settings` event stream loads after mount**: events before it opens are covered by the page-load values and the revision check on save.
- **Messages move with their paths.** Units moved to `units.ts`, imported only by the number control, validation and display; the `index.ts` barrel no longer re-exports `unitText` (no screen uses it, and the re-export kept it eager).
- **A control or instant mode that fails to load says so.** Browsers keep a failed module, so the same import fails again: the field shows "Part of this page couldn't load. Check your connection, then reload the page." in the control's place (one new message).
- **The error summary takes focus once per failed save.** It mounts again when its error count returns from 0 (a field fixed, then made invalid again); it doesn't take focus then, only for a save it hasn't focused yet.

## Behaviour
- **No behaviour changes beyond timing.** Existing tests pass unchanged except where a test needed the on-demand parts in place up front: `form.test.ts`, `SettingField.test.ts` and `SettingsForm.test.ts` preload them in `beforeAll`, as they already did for the dialog and menu. `lazy.test.ts`, `LazyControl.test.ts` and `SettingsForm.lazy.test.ts` load each part cold and check it works.

## Size (§10.1, §10.10.11)
Measured on a scratch merge of this branch, current main and S-settings-1 (`0bb33fe`): the screen route's static first load beyond `/search`, gzip level 9, from the build manifest. Dynamic imports aren't counted. The engine's share apportions each chunk's gzip by its sourcemap bytes from `form/**` and the `settings_form_*` messages, an estimate.

| | Route first view | Engine's share |
|---|---|---|
| S-settings-1 on main | 57.70 KB | ~24.3 KB |
| With this PR | 54.96 KB | ~24.8 KB |
| With this PR and the parked edit chunk | 44.46 KB | ~14.1 KB |

- **This PR alone leaves the route over 45 KB.** The controls' savings are mostly the `lib/ui` primitives they bring, outside the engine's share. The `Engine` class and the loading plumbing add about 0.5 KB to the engine's share.
- **The edit chunk is the lever:** it takes the route under the budget. It waits on the owner.
- **The ≤ 22 KB engine target means what every section pays on its first view** (runner, §2.19). The engine's total after an edit isn't gated. On the parked branch it's about 26.6 KB summed over its chunks.
- **Size-limit:** the owner's gate is S-settings-1's "Settings screen route" entry at 45 KB, using the shared `own()` with an `onDemand` list, which S-alerts #138 adds. This PR adds the engine's on-demand chunks to that list and a "Settings controls (on demand)" entry for them, at their measured size plus 10%, once #138 is on main. No engine-only entry (runner): the engine README lists the eager set and the reason for each file.

## Validation
- **`time-range` is `HH:MM-HH:MM`, 24-hour, two digits each, and may wrap midnight**, as B-update's `update::window::parse` reads it. A window that starts and ends at the same minute is refused with its own message, as the server refuses it. The client is a little stricter than the server on odd input (`+3:00`, which Rust's integer parse accepts); the server still has the final say. B-update adds `'time-range'` to `schema.test.ts`'s expected formats in its own PR, so that list is untouched here.
- **Two new messages**, `settings_form_error_time_range` and `settings_form_error_time_range_same`, in the existing pattern (what's wrong and what to enter, §10.14). English only, like the other engine errors.
