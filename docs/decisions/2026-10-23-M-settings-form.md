# M-settings-form decisions (part 1/2: the form store)

The Settings form engine (architecture §10.10.1, §10.10.4, §10.10.5, §10.10.7, §10.10.11). Part 1 is the store under `web/src/lib/features/settings/form/`; part 2 adds `SettingField`, `SettingsForm`, the widgets, the save bar, the dialogs and the leave guard. No owner decision is reversed or loosened. Items marked **(§2.19)** refine one and are for the runner to flag.

## The exposed API
- **As the brief lists it, plus what the components need.** `SettingsForm` also has `id`, `mode`, `saveError` (the refusal behind `offline` or `save-error`, with `retryAfter`), `undoSkipsSecrets`, `announcement` (`{id, text}`, the one polite live region's latest text), `href(key)`, `removeSecret(key)`, and `received(event)` / `resync()`, which `SettingsForm.svelte` calls from its `settings` subscription so the store stays free of component lifetimes. `undo(key?)` takes a key in instant mode (that field's inline Undo). `FieldState` also has `note` (an edit the engine dropped, and why), `undoUntil` (instant mode) and `secretSet`.
- **`widgetFor(spec)` returns a choice, not a component:** `{ builtin, mono }` for the engine's own controls, or `{ name, component }` for a registered one. The pure registry is tested without a DOM; part 2 maps the built-in names to components.
- **A single bounded number is a `number` widget** (an `Input` with the unit suffix and the bounds), not `RangeInput`: the `RangeInput` primitive is a min–max pair for filters. `x-widget: slider` with both bounds is the `Slider` plus input. An `x-widget` that names neither a registered widget nor a built-in hint (`radio`, `slider`) falls back to the shape's default with one dev warning per field, as the brief says. Today that's `duration`, `theme`, `language` and `time-zone`, and the four maps (`log-events`, `log-kinds`, `log-components`, `shortcuts`) get the JSON input until S-logs and S-keys register theirs.
- **An unknown `x-danger` is treated as `security`**, the strictest class, so a new class can't pass as safe.

## Values and the save
- **"Changed from default" means `source !== 'default'`.** The schema carries no default per property, so a value saved equal to the default still counts as changed. See the backend gaps.
- **Every danger class fails closed in the store**, `disruptive` included: `SettingsForm.svelte` registers the default disruptive dialog (part 2), so a form is never mounted without it, but the store alone refuses rather than saving unasked. Gates run strictest first (security, destructive, privacy, disruptive), all on one `POST /settings/preview` of the whole batch; the first refusal stops the save.
- **A stale save always returns `conflict`**, even when the merge found no field in conflict: the user sees the merged values and saves again (§10.10.5: never re-save automatically).
- **An `onConfirm` handler is asked once per save.** A second refusal with the same code shows the server's message on the field.
- **Secrets are cleared after every save attempt**, including one refused by client validation, so a typed secret is never held longer than one try (§10.10.4).
- **A secret is never reset through Save.** `reset()` on a `writeOnly` field does nothing: removing a saved secret is destructive and not undoable, so it runs only through `removeSecret` after its own confirmation (§10.10.7). Instant mode offers no Undo for a secret either.
- **Instant mode runs the same danger gates**, failing closed the same way, and holds `settings` events for the section while its own change is in flight, so the user's own change never shows as a conflict.
- **The draft is written before a save is sent**, not only after the debounce, so a `401` redirect or a crash right after ⌘S keeps it.
- **Editing a field in conflict rebases it on theirs**, as Keep mine does: the new value is the user's choice over theirs.
- **Retry re-runs what failed:** a failed Undo is retried as the Undo. If its window closed meanwhile, the banner clears.
- **A save of a secret alone offers no Undo** (there's nothing to send back); "Saved" still shows for the same 10 s.
- **`401` returns `refused` and changes nothing else:** W5's handler redirects, and the draft was already written.
- **Announcements are joined into one string** when one merge has two things to say (conflicts, and fields that no longer exist), since the live region reads only its latest text.
- **A stale Undo says so and refetches quietly**: the refetch doesn't follow it with "Updated elsewhere".
- **The inline preview also carries fields at their automatic value** (an `x-preview` field whose saved value is its `x-auto`), so "Automatic · 400" can show before anything is edited. Secrets and locked fields are never previewed.

## Concurrent edits
- **Events merge proactively.** A `settings` event for a section with staged edits refetches it and runs the merge at once, announcing conflicts; it never saves. A section without edits updates in place with "Updated elsewhere" when something visible changed.
- **Refetches after `set_by_environment`, `read_only`, a stale Undo and `onResync` apply the same revision too**: an environment variable or read-only change doesn't move the revision.
- **A field that becomes locked in a merge** keeps a note on the field ("Your change was dropped: {variable} now sets this setting") until it's edited or saved.

## Drafts
- **Key `lodestar.settings.draft.v1.<id>`, format `{ v: 1, at, sections: { [section]: { revision, base, mine, reset } } }`.** A reset has no value, so it's listed in `reset` with its base rather than in `mine`. Never secrets, never locked or read-only fields, at most 64 KiB (`JSON.stringify` length).
- **An offered draft isn't overwritten or cleared** before the user restores or discards it, not by a save or Discard either: edits made meanwhile are kept in the form, and drafts resume once the offer is answered. On Restore, edits made since the page loaded win over the draft's.
- **Section and field names from storage or events are looked up as own properties only**, so a draft naming `constructor` can't break loading; a draft that can't be planned is cleared.
- **The 64 KiB cap counts UTF-8 bytes.**
- **A draft that no longer differs from the server is cleared without asking.**

## Timings
- **300 ms for all three debounces:** the draft write, the inline preview and instant-mode text and numbers (§10.10.4's figure). Instant-mode toggles and segmented controls send at once. Undo is offered for 10 s (§10.10.5).
- **Instant-mode sends are serialized** on one chain, so two quick changes never race; a value that fails client validation isn't sent and isn't reverted (the user is typing).

## Messages
- **A generated map instead of a glob (§2.19, the brief's second option).** Paraglide's Vite plugin compiles one module per locale, so the per-message files a glob over `paraglide/messages/settings_*.js` would match exist only after the CLI compile, never in `vite build` or vitest. `message-map.ts` reads every placeholder-free `settings_*` message statically from `m`, generated by `node --experimental-strip-types src/lib/features/settings/form/message-map.gen.ts` from `features/settings/messages/en.json`; a test fails when it's out of date, and another fails if a form source indexes `m` dynamically. S-settings-1 reruns the generator after adding its keys. A follow-up could run it from `scripts/messages.ts` (a shared file).

## Recorded now, built in part 2
- **The error summary takes focus, and its links lead to the fields** (the GOV.UK reading of §10.10.5's "then focus moves to the first field with an error", confirmed by the runner).
- **Discard asks nothing for one change and confirms for more**, since one change is cheap to redo.

## Backend gaps (follow-ups)
- **No `default` per schema property:** Reset to default can't show the default value, and "Changed from default" is `source !== 'default'`.
- **`SettingsBatch`, `SettingsPatch` and `SettingsSection.schema` are untyped objects** in the OpenAPI document (`Record<string, never>`). `requests.ts` casts the batch body once; adding `additionalProperties` on the Rust side removes the cast.
- **No section has a secret yet** (B2b refuses them until sealing exists): secrets are tested with a fixture section.
- **No integrations section** for the wizard's Download client step.
- **The toast primitive has no action button**, so instant-mode Undo is inline under the field.
