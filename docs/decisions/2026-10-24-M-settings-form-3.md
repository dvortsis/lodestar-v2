# M-settings-form-3 decisions (the edit chunk)

The third Settings form engine PR (`web/src/lib/features/settings/form/`, architecture §10.10.5, §10.10.11, §10.1). It supersedes two notes in `2026-10-23-M-settings-form-2.md`: "The save pipeline stays in the first view", and the `+3:00` remark under Validation. Items marked **(§2.19)** refine an owner decision and are for the runner to flag.

## The owner's decision

- **The save pipeline loads on demand, as one edit chunk (owner, 2026-10-24: option A, relayed by the runner).** This supersedes the "no async seams on the merge and the draft restore" note in `2026-10-23-M-settings-form.md` and its follow-up in `2026-10-23-M-settings-form-2.md`. The 45 KB route budget stays as it is. The draft code now loads with the first edit, and a stored draft's offer appears once that chunk has loaded. The merge and the restore stay synchronous once it's in.

## What loads when

- **The edit chunk (`edit.ts`)** holds the save pipeline (`saving.ts`: gates, the PATCH, refusals, Undo, the merge, reading and restoring drafts, validation) and the UI they show: the save bar, the error summary, the network notice, the draft offer, a field's conflict and saved lines, and the discard, leave and disruptive questions.
- **What loads it:** the first staged edit, a stored draft (in parallel with the values, so the offer shows with the fields), a newer `settings` event, a save or a secret's removal. When the browser is idle after the first view, it loads anyway, so the first edit normally finds it in place. Instant mode loads it as well, since it sends through the pipeline.
- **Writing drafts stays in the first view (`draft-store.ts`).** An edit is drafted from the first keystroke, even when the chunk never loads. Reading a draft and restoring it (`draft.ts`) are in the chunk.
- **What a refusal names (`errorSection`, `errorKey`) moved to `requests.ts`.** Previews and the pipeline both use it, and the two load separately, so neither pulls in the other.
- **Before the chunk is in:** leaving a field still validates it once the chunk arrives. Discard, Undo, Use theirs, Restore and the error lines only exist after something loaded it. A field's "locked" note comes from the merge, so it's worded by the chunk.

## When it can't load

Browsers keep a failed module, so a failed chunk isn't tried again. Only a reload helps.

- **A stored draft whose offer can't load:** the draft banner's place shows "Your unsaved changes from earlier couldn't load. They're kept: check your connection, then retry." **Retry** reloads the page.
  - The stored draft isn't overwritten while its offer can't load, so the reload offers it again.
  - Edits made in the meantime aren't drafted over it. If there are any, the leave guard gives the browser's own prompt on the reload.
  - It replaces the general line below, so the form shows one banner.
- **Any other part of the chunk** (the bar, the summary, the notice): a banner says "Part of this page couldn't load. Check your connection, then reload the page." with **Reload**. Edits are still drafted. A save returns `network` and saves nothing. A staged secret is dropped with that line, as any tried secret is (§10.10.4).
- **Leaving with unsaved changes when the question can't load** goes by a full page load, so the browser's own prompt asks.
- **Two new messages:** `settings_form_draft_failed` and `settings_form_reload`, English only like the other engine lines. Retry reuses `settings_form_retry`.

## Size (§10.1, §10.10.11)

Measured on a scratch merge of this branch, current main (`c7b9d71b`) and S-settings-1 (`4fc138d7`), gzip level 9, from the build manifest. The engine's share is estimated by apportioning each chunk's gzip by its sourcemap bytes from `form/**` and the `settings_form_*` messages.

|                                                                 | This PR  |
| --------------------------------------------------------------- | -------- |
| "Settings screen route (own JS + CSS)", S-settings-1's gate     | 42.84 KB |
| Static first load beyond `/search` (the earlier tables' method) | 44.58 KB |
| Engine's share of that first load                               | ~14.5 KB |
| "Settings controls (on demand)"                                 | 46.23 KB |

- **The screen route is under the owner's 45 KB budget,** at 42.84 KB, which is inside the runner's 40–43 KB target. The gate is S-settings-1's entry, which now leaves the shell's chrome out.
- **"Settings controls (on demand)" now gates the edit chunk too.** Its limit is 51 KB, the measurement plus 10%. `settingsOnDemand` in `web/.size-limit.mjs` adds `edit.ts`, so the screen route's entry stops at it.
- **The ≤ 22 KB engine target is what a section pays on its first view** (runner, §2.19, as in `2026-10-23-M-settings-form-2.md`): about 14.5 KB.

## Behaviour and tests

- **No behaviour changes beyond timing and the failure lines above.** Existing tests keep their assertions; the suites that preload on-demand parts already loaded the pipeline through `preloadSettingsForm`.
- **`lazy.test.ts`** loads each part cold. It checks that the first view loads nothing, that the first edit loads the chunk and saves with it, that a field left before the chunk arrives is still validated, that an event loads the chunk to merge, and that a stored draft loads it with the values and is offered with the fields. With the chunk failing, it checks that an edit is still drafted and a save reports `network`, and that a stored draft whose offer couldn't load survives edits and `destroy()`.
- **`SettingsForm.offline.test.ts`** checks the component with the chunk failing: the Reload banner, the draft banner with Retry (and no second banner), and leaving by a full page load.
- **`lazy-imports.test.ts`** lists the edit chunk's files, so a first-view file that imports one statically fails.

## Validation (correction)

- **`+3:00` is refused by the server too.** The `2026-10-23-M-settings-form-2.md` note said the client was stricter than the server on `+3:00`, because Rust's integer parse accepts a leading `+`. That's wrong for current main. `update::window::parse` (`crates/lodestar/src/update/window.rs`) requires exactly two ASCII digits per field before parsing, so the client and the server agree on it. The client's rule doesn't change.
