# M-settings-form follow-up decisions (engine diet, time-range)

The runner's follow-up to the Settings form engine (`web/src/lib/features/settings/form/`, architecture §10.10.5, §10.10.11, §10.1). S-settings-1 measured `/settings/[section]` over the owner's 45 KB route budget, so the engine loads on demand what a section's first view doesn't need, and the client learns B-update's `time-range` format. Items marked **(§2.19)** refine an owner decision and are for the runner to flag.

## What loads when

- **The save pipeline stays in the first view.** The owner's Budget decision (`2026-10-23-M-settings-form.md`) chose the 45 KB route budget over async seams on the merge and the draft restore. So the pipeline (`saving.ts`: gates, the PATCH, refusals, Undo, the merge, drafts, validation) and the UI it shows (the save bar, the error summary, the banners, the draft offer, a field's conflict and saved lines, the questions) are static imports, as on main. Loading them on demand as one edit chunk is parked on `slice/M-settings-form-3` until the owner answers the runner's question (numbers below).
- **The store's state is an `Engine` class** that is itself the `SettingsForm` a screen holds, so the parts that load on demand can work on it. `saving.ts` holds the pipeline; `form.svelte.ts` the rest.
- **Every built-in control but the toggle and the text input loads per type**: number, slider, the enum controls (segmented, radio and select in one file), secret, chip list and JSON. `SettingsForm.svelte` starts the ones a form needs as soon as it knows its fields (while the values load), so inside a form they're normally in place at first paint. A field rendered before then shows a hidden box of the control's height: an input's for the controls that share the control height, a row per option for a radio group, the four-row Mono textarea for JSON. A chip list that already has chips grows by its chip row when the control arrives.
- **Instant mode (`instant.ts`) loads with the first instant change**, and on an instant form when the browser is idle after the first view. Changes made while it loads keep their order on its chain. If it can't load, the value goes back with "Part of this page couldn't load…", as a failed send does.
- **Previews (`preview.ts`) load during the first preview's 300 ms debounce**, with what they say (estimates, warnings, "Automatic · value").
- **The field menu loads when first opened** (`menu.ts`: the menu, its labels and the "Show keys" list). **⌘/Ctrl+S's registration loads with the shortcut registry** (`save-key.ts`). **Copy's toast loads on the first copy**; the clipboard write stays in the click, where Safari allows it. **The `settings` event stream loads after mount**: events before it opens are covered by the page-load values and the revision check on save. It saves nothing today, because the shell's `status.svelte.ts` already loads it.
- **Messages move with their paths.** Units moved to `units.ts` and out of the `index.ts` barrel, which no screen uses for them. In this PR they stay eager anyway, through validation and the conflict line's display, so the move only matters with the parked edit chunk.
- **A part that fails to load says so, and isn't tried again.** Browsers keep a failed module, so the same import fails again, and the engine doesn't retry one either. A control's field shows "Part of this page couldn't load. Check your connection, then reload the page." in the control's place (one new message), in a group the field's label names and the line describes. A previewed field shows it when previews can't load, and a field whose ⋯ menu can't load shows it under the field. A secret staged for instant mode's Save is dropped with that line, as any tried secret is (§10.10.4). An instant change still waiting for instant mode to load counts as in flight: `save()` returns `busy`, so leaving doesn't drop it.
- **The API works detached** (`onDestroy(form.destroy)`, as the auth forms use their store): the `Engine` binds its public methods, as the closure it replaced allowed.
- **A field's label points at its control only once the control is in.** Until then it labels by id, as for a radio group, so `for` never names a missing element.
- **The error summary takes focus once per failed save.** It mounts again when its error count returns from 0 (a field fixed, then made invalid again); it doesn't take focus then, only for a save it hasn't focused yet.

## Behaviour

- **No behaviour changes beyond timing.** Existing tests keep their assertions. `form.test.ts`, `SettingField.test.ts` and `SettingsForm.test.ts` preload the on-demand parts in `beforeAll`, as they already did for the dialog and menu. `values.test.ts` and `messages.test.ts` import what moved from its new file, and `validate.test.ts` adds the time-range cases. `lazy.test.ts`, `LazyControl.test.ts` and `SettingsForm.lazy.test.ts` load each part cold and check it works. `lazy-imports.test.ts` fails if a first-view file imports an on-demand one statically.

## Size (§10.1, §10.10.11)

Measured on a scratch merge of this branch, current main and S-settings-1 (`0bb33fe`): the screen route's static first load beyond `/search`, gzip level 9, from the build manifest. Dynamic imports aren't counted. The engine's share apportions each chunk's gzip by its sourcemap bytes from `form/**` and the `settings_form_*` messages, an estimate.

|                                        | Route first view | Engine's share |
| -------------------------------------- | ---------------- | -------------- |
| S-settings-1 on main                   | 57.70 KB         | ~24.3 KB       |
| With this PR                           | 55.60 KB         | ~24.8 KB       |
| With this PR and the parked edit chunk | 44.46 KB         | ~14.1 KB       |

- **This PR alone leaves the route over 45 KB.** The controls' savings are mostly the `lib/ui` primitives they bring, outside the engine's share. The `Engine` class, the loading plumbing and the load-failure fallbacks add some back, and main moved between the measurements, so the rows aren't strictly comparable.
- **The edit chunk is the lever:** it takes the route under the budget. It waits on the owner.
- **The ≤ 22 KB engine target means what every section pays on its first view** (runner, §2.19). The engine's total after an edit isn't gated. On the parked branch it's about 26.6 KB summed over its chunks.
- **Size-limit:** the owner's gate is S-settings-1's "Settings screen route" entry at 45 KB, using #138's `own(route, { onDemand })`. This PR adds `settingsOnDemand` (the engine's on-demand chunks) for that entry to use, and "Settings controls (on demand)" for what it leaves out: 34.34 KB with S-settings-1's screen, so 38 KB (measured + 10%). Until a screen uses the engine there's nothing to measure, so the entry is added only when its files exist. No engine-only entry: the engine README lists the eager set and the reason for each file.

## Validation

- **`time-range` is `HH:MM-HH:MM`, 24-hour, two digits each, and may wrap midnight**, as B-update's `update::window::parse` reads it. A window that starts and ends at the same minute is refused with its own message, as the server refuses it. The client is a little stricter than the server on odd input (`+3:00`, which Rust's integer parse accepts); the server still has the final say. B-update adds `'time-range'` to `schema.test.ts`'s expected formats in its own PR, so that list is untouched here.
- **Two new messages**, `settings_form_error_time_range` and `settings_form_error_time_range_same`, in the existing pattern (what's wrong and what to enter, §10.14). English only, like the other engine errors.
