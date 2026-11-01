# X-flows-door-2 decisions: Appearance and the wizard go through the filters flows' door

Closes the S-filters-2c (#316) review follow-up (`2026-11-01-S-filters-2c.md`, "Not done here"). Nothing in `features/filters/flows/**` changed, and the 18+ confirmation (§20.9) behaves as before.

- **Appearance loads its 18+ flow through the door.** `flows-door.ts` gains `loadAdult()`, which imports a new root, `features/filters/flows-adult.ts`. That root re-exports `AdultConfirmDialog`, `createAdultConfirm` and `setShowAdult` from `flows/**`. `AppearanceScreen.svelte` no longer imports `flows/adult.svelte` statically, and no longer imports `AdultConfirmDialog` directly. It loads the door on the first turn-on of "Show adult content" and creates the confirmation then.
  - *Why a second root and not `load()`:* `flows-root.ts` brings every change dialog (about 35 KB). The 18+ confirmation needs only its own dialog (about 20 KB), as it did before.
  - *Why one root and not two imports in the door:* two `import()` calls put two preload lists in the door. That measured 635 B, over its 0.58 KB limit. One root measures 553 B.
- **The 18+ confirmation behaves as before.** Turning the switch on asks for the confirmation. Cancel leaves it off and sends nothing. A refused save shows the server's state. Nothing is sent before Show is pressed.
  - *Turning it off loads nothing.* It asks for no confirmation, so it never waits on the door. Otherwise a tab left open across an upgrade (old chunks gone, API up) couldn't turn adult content off. `features/appearance/show-adult.ts` sends the same `{ show_adult: false }` as the flows' `setShowAdult(false, …)` and fails the same way; `show-adult.test.ts` checks both against each other. This was a self-review finding on the first draft, which loaded the door for both directions.
- **`SHOW_ADULT_ENV` is copied out of the flows.** Appearance's first view names `LODESTAR_SHOW_ADULT` when the server marks the switch read-only without naming a variable. The constant now also lives in `features/appearance/show-adult.ts`. The flows' copy can't move without editing `flows/**`, where `errors.ts` imports it. `show-adult.test.ts` keeps the two copies equal.
- **Files beyond the brief's list, both "what the importers need":** the new root `features/filters/flows-adult.ts`, and `wizard/packs.svelte.ts` (its comment and the `Flows` type).
- **The wizard's pack step loads the change flow with the door's `load()`.** Before, it imported `flows/flow.svelte` directly.
  - *Why not a separate loader:* a `loadFlow()` would add a third preload list to the door, about 30 B, and that would take the door over its limit.
  - *Cost:* the first Continue with packs checked now fetches the flows' one on-demand chunk (gated by "Filters dialogs"), not the change flow alone. The change flow is no longer a dynamic-import root, so the bundler keeps it inside that chunk, as S-filters-2c intended.
  - *Type change:* `packs.svelte.ts` now types `Flows` as `Pick<…, 'createChangeFlow'>`, which both modules satisfy.
- **The dev route is excluded, not routed.** `/dev/filters-flows` imports the flows directly. It is linked into test builds only (`scripts/dev-routes.mjs`) and never into the production build the size step measures. A comment at the guard says so.
- **The guard covers more views.** `.size-limit.mjs` now also fails if any of these carry a `filters/flows/**` module, `flows-root.ts` or `flows-adult.ts`:
  - Settings AppearanceScreen (first view);
  - the Welcome route (the wizard's first view);
  - Wizard steps (on demand), which counts each step's own imports, dynamic ones included.

  The 18+ modules left Appearance's own stop list, so a direct import of either one puts it back in that view, where the guard sees it.
  - *Checked:* with Appearance importing `SHOW_ADULT_ENV` from `flows/adult.svelte` and the pack step importing `flows/flow.svelte`, the size step failed. It named `adult.svelte.ts` in the Appearance first view, and `flow.svelte.ts` and `machine.ts` in Wizard steps.
  - *Measuring:* the door's entry now stops at both roots. "Appearance 18+ dialog (on demand)" measures from `flows-adult.ts` and the shared `adult.svelte` chunk, which has no source key and is matched by its chunk name.
- **Sizes** (gzip bytes, `npx size-limit --json`, local builds without `GITHUB_SHA`, `main` at dba7434; two runs of the same build were identical; no limit changed):

  | Entry | `main` | This branch | Change | Limit |
  |---|---|---|---|---|
  | Settings AppearanceScreen (first view) | 28,317 | 27,878 | −439 | 45,000 |
  | Appearance 18+ dialog (on demand) | 19,777 | 20,138 | +361 | 20,600 |
  | Wizard steps (on demand) | 49,395 | 47,250 | −2,145 | 53,600 |
  | Filters flows door (on demand) | 529 | 553 | +24 | 580 |
  | Filters dialogs (on demand) | 35,351 | 35,344 | −7 | 35,900 |
  | Settings FiltersScreen (first view) | 30,575 | 30,566 | −9 | 31,000 |

  - The Appearance first view fell by 439 B: `adult.svelte` and the dialog's preload list now load on demand, which accounts for the 18+ entry's +361.
  - Wizard steps fell by 2,145 B: the change flow it counted is gated in "Filters dialogs".
  - Every other entry moved by 7 B or less, from changed chunk hashes. The largest increase among them is "Settings security keys and proxy" (+2 B).
- **Not done here (follow-up):** the first-view walks treat `flows-root.ts` as on demand, as they treat the door. So a view that imports `flows-root.ts` with `import()` directly, skipping the door, isn't named by the guard. (`flows-adult.ts` isn't treated that way, so a direct import of it is caught.) The gap predates this slice and covers the Filters, drawer and /t views too. A check that only `flows-door.ts` imports the roots would close it.
