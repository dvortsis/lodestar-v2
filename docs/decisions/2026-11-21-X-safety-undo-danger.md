# X-safety-undo-danger decisions (#PR)

Runner brief, 2026-11-21Z, with the runner's two rescopes the same day: Undo gets a real preview (option A, in this slice), and an explicit `Assessment.loosens_child_safety` flag. It follows X-safety-toggles (#1019): a change that loosens child safety gets the danger style. Undo was the gap. Styling only, plus what the style reads: what Undo does, its requests, the password and the warning are unchanged (owner §2.39).

## Why the server changed
- **Undo's assessment couldn't tell.** `assess_tracked` set `weakening` and `estimate` and left every count at 0, so nothing could tell an Undo that loosens child safety from one that strengthens. `weakening` and `confirm` are true for every Undo, and its warning is a fixed text. The session stopped as the brief asked.
- **Counts alone weren't enough** (runner, on this slice). A CS rule Undo turns off that caught nothing in 30 days gives `core_caught_30d` 0, and its dialog would stay ordinary while the rule's own "Turn off" button is danger. So the server says it outright, with a new flag, and the counts become a real preview.

## `Assessment.loosens_child_safety`
- **What it means** (`crates/api/src/dto/filters.rs`, `Assessment`): the change turns a child-safety core rule off or weakens one, takes entries off the locked floor, or releases or marks as a false positive a hold by a child-safety rule. That's #1019's danger rule, set wherever `assess_tracked` already knows:
  - `core_switch` off, when stored on (every `CoreRule` is a CS rule, `crates/filter/src/core.rs`, `CoreRule::ALL`);
  - `add_exception` to a CS rule;
  - `add_override`, which can bypass every unlocked core rule (its own warning says so);
  - `remove_cs1`, whose entries leave the floor;
  - `release` and `false_positive` on a hold by a rule whose code starts `CS-`, quarantine or tombstone, as the screen reads it (`child_safety`);
  - any change whose preview has `removed_from_floor` > 0: a floor removal, and an unsubscribe or pause of a floor list;
  - `undo`, from the comparison below.
- **Two cases go beyond #1019's buttons:** `add_exception` to a CS rule and `add_override`. They weaken CS rules too. The client doesn't read the flag for them (below), so nothing on screen changes for them.
- **Sent only when true** (`skip_serializing_if`), so it's optional in `openapi.json` and `schema.d.ts`, and absent reads as false. The assessments the golden files hold, all false but one, are byte-for-byte unchanged. This is a call in this slice: it keeps the change to the wire and the goldens small, and a missing flag only means the ordinary style, which is never a security decision (`confirm` still decides the password).
- **It's style only.** `weakening`, `confirm` and the warning are as before for every change.

## Undo's preview and flag
- **The comparison** (`FiltersApi::undo_preview`, `loosened`, `crates/api/src/filters.rs`): the kept rule set (`Previous::parse`, which `undo()` now uses too, so apply restores exactly what it did before), with the current pack versions copied in (Undo never restores them), against the current engine and set.
- **The flag is true when Undo** switches a core rule off, puts an exception back on a `CS-` rule, or puts back an override that hasn't expired. Hits don't matter.
- **The counts:**
  - `quarantine` and `tombstones` come from `rejudged(current engine, engine with the kept set)`: what Undo would release now, from sealed names (`estimate: true`, as before).
  - `caught_30d`, `core_caught_30d`, `per_day` and `stored` come from `for_rules` over the rules Undo weakens:
    - a core or pack rule it switches off;
    - your rule it removes, disables or edits;
    - a rule it puts an exception back on;
    - a pack rule whose override it changes.
    - An override it puts back counts every unlocked rule, as `add_override` does. An allow entry it puts back counts your rules, the packs and the lists, as `add_allow` does.
  - What Undo only strengthens adds nothing. Like the override's preview, it's an upper bound.
- **`removed_from_floor` stays 0.** Undo restores only the rule set and the unlocked core switches (`undo()`): locked rules are never restored, subscriptions aren't in the kept set, and `released_by_hash` never releases a floor rule. So an Undo can't remove floor entries, and the brief's "an Undo that removes a floor entry" test can't exist. The tests check that every Undo reports 0.
- **Fallback:** with nothing kept, or a kept or current set that can't be read, it's the old all-zero `estimate: true` preview with the flag false. The preview never fails an Undo. Apply still refuses "nothing to undo" as before.

## The dialog
- **`loosensChildSafety(assessment)`** (`flows/changes.ts`) is true for the flag, and, as a supporting signal, for `core_caught_30d` > 0 or `removed_from_floor` > 0. Those two counts can only be non-zero when the server sets the flag, and they keep a server without the flag right where it can be.
- **Only Undo reads it.** `ChangeDialog` uses the danger style when the caller passes `danger` (the #1019 flows, unchanged), or when the change on screen is an `undo` whose assessment loosens child safety.
- **Before the preview arrives, and after it fails,** the dialog has no assessment and no Apply button, and nothing in it uses the danger style. The style never comes from a guess.
- **The screen's Undo button stays ordinary.** It can't know what it restores before the preview.
- **Not colour alone** (a UX call, runner §2.26, to flag). In the other danger flows, Apply's label names the loosening action ("Turn off …", "Remove from the floor"). A danger Undo's Apply now reads "Undo and loosen child safety" (撤销并放宽儿童安全保护), a new message, `filters_undo_apply_loosens`. Otherwise it's "Apply", as before. No existing message said this, and with the flag alone every count can be 0, so the preview table can't carry the cue.

## Tests
- **Server** (`crates/api/src/filters.rs`, `mod tests`):
  - an Undo that switches CS-3a back off reports its catches and the flag;
  - one that switches off a CS rule that caught nothing, or puts back an exception that matched nothing, sets the flag with zero counts;
  - one that puts back a CS exception, or an override, reports its catches (the override: `core_caught_30d` 2, `caught_30d` 3) and the flag;
  - one that switches a pack back off counts `caught_30d` 2 and `core_caught_30d` 0, with no flag;
  - one that only strengthens reports zeros and no flag;
  - nothing kept, or an unreadable kept set, gives the old estimate with no flag, and apply still refuses;
  - every Undo keeps `weakening`, `confirm`, the warning, `estimate` and `removed_from_floor` 0;
  - the flag for the other changes: a CS rule off, an exception to a CS rule, an override, `remove_cs1`, and release and false positive on a CS hold set it; a CS rule already on, a pack off, an exception to a pack rule, an allow entry, and release and false positive on a pack hold don't.
- **Client** (`screens/screen.test.ts`, "Undo takes the danger style from its assessment"):
  - an Undo with the flag, or with core catches or floor removals, gets a danger Apply that names what it does;
  - one without them, from a server without the flag, or one that only strengthens keeps "Apply" in the ordinary style;
  - the screen's Undo button is always ordinary;
  - the request is still `{op: "undo"}`, and the password comes before any apply;
  - nothing in the dialog is danger while the preview runs, or after it fails;
  - a pack switch whose assessment has the flag stays ordinary (only Undo reads it);
  - `loosensChildSafety` reads only the flag and the two counts.
- **Mutation checks:** see the PR description; each named mutation fails at least one test.
- No e2e spec or snapshot changed: the specs don't open an Undo that loosens child safety.

## Sizes
gzip, main ce43c7e7 → this branch, each new limit the measured size rounded up to 0.01 kB (runner, §2.26).

| Entry | main | branch | Limit before | Limit after |
|---|---|---|---|---|
| Filters dialogs (on demand) | 35,898 B | 35,965 B | 35.9 kB | 35.97 kB |
| Settings InsightsScreen (first view) | 45,296 B | 45,302 B | 45.3 kB | 45.31 kB |
| Settings NotificationsScreen (first view) | 45,504 B | 45,512 B | 45.51 kB | 45.52 kB |
| Filters pack panels (on demand) | 7,911 B | 7,914 B | 7.91 kB | 7.92 kB |

- **The dialogs' +67 B is the loosening derivation.** The chunk already loads only when needed.
- **The rest is chunk-hash and gzip drift.** Neither Insights nor Notifications changed in code, and the pack panels were already 1 B over on main.
