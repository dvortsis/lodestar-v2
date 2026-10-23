# The Settings form engine

The engine behind every Settings screen and wizard step (architecture §10.10). Screens import from `index.ts`; the components (`SettingsForm.svelte`, `SettingField.svelte`) by path. Decisions: `docs/decisions/2026-10-23-M-settings-form.md` and `2026-10-23-M-settings-form-2.md`.

## What's in a section's first view, and why

A Settings screen has the owner's 45 KB route budget (§10.1), and the engine is most of it. So what a section shows on first paint, and the save pipeline the owner chose to keep in it (no async seams on the merge and the draft restore, `2026-10-23-M-settings-form.md`), are static imports. Everything else is a dynamic import, loaded once per page when it's first needed.

**Eager (static imports):**

| File                                                                                                                    | Why it's in the first view                                                                                                                                          |
| ----------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `form.svelte.ts`                                                                                                        | The store: loading, the field view, staging an edit, the debounces, events. Its `Engine` class is the `SettingsForm` the screen holds.                              |
| `saving.ts`, `batch.ts`, `merge.ts`, `draft.ts`, `validate.ts`, `units.ts`, `display.ts`                                | The save pipeline, Undo, the merge, drafts and validation: kept in the first view by the owner's decision, so a merge or a draft restore never waits on a download. |
| `schema.ts`, `types.ts`, `requests.ts`, `values.ts`                                                                     | Reading the schema into fields, the requests, saved values and staging.                                                                                             |
| `messages.ts`, `message-map*`                                                                                           | Field labels and help.                                                                                                                                              |
| `widgets.ts`                                                                                                            | Picking each field's control.                                                                                                                                       |
| `SettingsForm.svelte`, `SaveBar`, `ErrorSummary`, `FormNotice`, `DraftBanner`, `questions.ts`, `dialogs.ts`, `leave.ts` | The form's frame and everything the save pipeline shows: the bar, the summary, the banners, the draft offer, the discard, leave and disruptive questions.           |
| `SettingField.svelte`, `FieldConflict`, `FieldSaved`                                                                    | The field: label, help, error, Automatic, the status line, a conflict, what a save did.                                                                             |
| `Control.svelte`                                                                                                        | The two controls cheap enough to keep: the toggle and the text input.                                                                                               |
| `LazyControl.svelte`, `controls.svelte.ts`, `lazy.svelte.ts`                                                            | Loading the rest, with a placeholder of the right height so nothing moves.                                                                                          |

**On demand (dynamic imports):**

| Chunk                                                                                             | Loads with                                                                            | Holds                                                                 |
| ------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| `instant.ts`                                                                                      | The first instant-mode change; prefetched when the browser is idle on an instant form | Instant mode                                                          |
| `preview.ts`                                                                                      | The first preview's debounce                                                          | Previews and what they say (estimates, warnings, "Automatic · value") |
| `NumberControl`, `SliderControl`, `ChoiceControl`, `SecretControl`, `ChipsControl`, `JsonControl` | A field of that type, or the form listing one (while the values load)                 | One control each; `ChoiceControl` is segmented, radio and select      |
| `menu.ts`                                                                                         | The first ⋯ menu opened                                                               | The menu and its "Show keys" list                                     |
| `save-key.ts`                                                                                     | Mounting `SettingsForm` (with the shortcut registry)                                  | ⌘/Ctrl+S in the registry                                              |
| `copy.ts`                                                                                         | The first copy                                                                        | The toast after a copy (the write itself stays in the click)          |
| `$lib/state/events.svelte`                                                                        | Mounting `SettingsForm`                                                               | The `settings` event stream                                           |

**Keep it that way.**

- A message used only on a lazy path belongs in a lazy file.
- A lazy file imports shared code from the eager files, never the other way round: an eager import of a lazy file makes it eager. `lazy-imports.test.ts` fails on one; add a new on-demand file to its list.
- The `index.ts` barrel exports only what screens use; a re-export there keeps its module in the first view.
- Rolldown pays about 400 B gz per chunk, so group what loads together into one entry (as `menu.ts` does) rather than many small ones.
- A chunk that fails to load can't be retried (browsers keep the failure), so every lazy path has a visible fallback: the control's "couldn't load" line, or instant mode putting the value back with it.
