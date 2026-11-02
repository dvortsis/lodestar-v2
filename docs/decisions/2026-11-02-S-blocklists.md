# S-blocklists decisions

The UI to choose blocklist subscriptions (owner §2.9, architecture §20.13, UI §10.10), on the API from #362 (`2026-11-01-B-subscriptions-api.md`). Nothing here changes an owner decision, a number or a security decision.

## What it is
- **Settings → Blocklist subscriptions** (`/settings/blocklists`, library group, after Filters & Safety), in `web/src/lib/features/blocklists/`. Its own screen, since Filters & Safety is at its first-view limit. It lists your lists with their figures, the combined strictness, a *New version held* badge, and the catalog with one-click *Connect*.
- **Every change goes through the filters flows' dialog** (`flows-door` → `ChangeDialog`), the same preview → warning → password → apply path as the rest of Filters & Safety. Nothing builds a second dialog or effect warning, and the screen decides nothing: whether a change needs the password, and every count in the warning, come from the server's preview.
- **Strictness is shown from the preview** (`Assessment.strictness`, in the dialog's `extra` slot) before *Connect* applies. A catalog list comes out as the server reports it (BT_BlockLists: severe).
- **Nothing is subscribed by default.** The screen sends nothing on load. `subscribe` is sent without `floor`, so it never needs the password.
- **Wizard: an optional step after Rule packs**, in the feature's `WizardStep.svelte`, loaded by one line in `WizardPage` (plus the step list and the Done step's name map). Nothing is pre-selected; Skip or no choice connects nothing. The strictness warning is `POST /filters/strictness` for the chosen lists plus the lists you already have enabled, shown before Continue, and Continue is refused if that assessment hasn't loaded. Lists connect headless through the same flow (`connect.svelte.ts`, as `packs.svelte.ts` does for packs) and only if the preview is plain: a warning or a password stops it and points to Settings.
- **Not built:** adding your own list by URL. The brief and §2.9 ask for the catalog; a URL form is a later slice. `refusalText` already maps the URL and name errors (`list_url_refused`, `bad_list_url`, `bad_list_name`, …) to plain messages, used where this feature words an error (the wizard). In the dialog a refused change shows the server's own sentence, which #362 wrote in plain terms.

## A change outside this feature (approved by the runner)
- **Accepting a held version with the password sends `held`.** `forApply` in `filters/flows/machine.ts` bound only a floor removal's `floor_entries`. For `resolve_held {accept: true}` it now sends back the preview's `held_version` as `held` (the API refuses with 400 `held_needs_version` otherwise), and `assessmentDiffers` compares `held_version`, so a changed held version asks again. Unit tests in `machine.test.ts`: the field is sent, a changed version re-asks on a 428, and a 409 re-previews. Keeping the previous version sends no `held`.

## Sizes
- The screen loads on demand through `body.ts` (the Jobs/Security/Logging pattern). First view measured 29.47 kB; limit 30.4 kB. No existing limit is raised.
- The wizard's step has its own entry (`Blocklist wizard step (on demand)`, 7.51 kB, limit 8.3 kB) and is excluded from "Wizard steps (on demand)", whose limit is untouched.
