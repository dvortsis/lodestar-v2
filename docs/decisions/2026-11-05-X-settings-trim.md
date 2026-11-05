# X-settings-trim decisions

Brief: win back the Settings chunk's headroom under the §10.1 45 kB cap, with no change in behaviour or appearance. SRC-15a, SRC-17 and LEGAL-1 had raised two entries past the cap under §2.26 as a temporary measure.

- **Two pure moves, both from a field's first view into the edit chunk** (the chunk that already holds a field's conflict, reset and saved lines, loaded by the first edit, draft or event):
  - The conflict row (its wrapper, the warning icon and the text) moved from `SettingField.svelte` into `FieldConflict.svelte`. The warning icon was used nowhere else.
  - The staged-reset line (its paragraph and icon) moved from `SettingField.svelte` into `FieldSaved.svelte` (`part="reset"`).
  - The row's markup, classes and `data-testid="conflict"` are unchanged, and each row renders only when the edit chunk is in, as before.
- **The field menu's choice handler moved into `menu.ts`** (`selectMenu`), the lazy chunk that loads when a menu is first opened. It can only run once the menu is open, so the first view no longer carries it. Copying stays in `SettingField.svelte`: the clipboard write has to happen in the click.
- **No visible text, layout or keyboard flow changed.** No new lazy boundary, so no new loading or failure line: all three moves land in chunks that already have their own failure handling (`Edit`, `menuFailed`).
- **Tried and dropped** (each made the first view bigger, since Rolldown re-splits the shared message and icon chunks and the new chunk overhead cost more than the strings saved):
  - Moving the "could not load" and "draft unavailable" banners into lazy parts. The draft one can't live in the edit chunk anyway: it shows when that chunk failed to load.
  - A lazy restart banner in `SectionForm.svelte`.
  - Loading `focus.ts` only when a link names a field.
  - One shared snippet for the three banners in `SettingsForm.svelte`.

## Bytes (gzip, `npx size-limit`)

| Entry | Before | After | Limit before | Limit after |
|---|---|---|---|---|
| Settings section (own JS + CSS) | 45,070 B | 44,952 B | 45.07 KB | 45 KB (lowered) |
| Settings NotificationsScreen (first view) | 45,127 B | 44,988 B | 45.14 KB | 45 KB (lowered) |
| Settings overview (own JS + CSS) | 14,353 B | 14,347 B | 45 KB | 45 KB |
| Settings controls (on demand) | 33,393 B | 33,460 B | 38 KB | 38 KB |
| Settings edit chunk (on demand) | 11,011 B | 11,095 B | 11.3 KB | 11.3 KB |

Other entries also moved: controls (on demand) +67 B, edit chunk (on demand) +84 B, Wizard steps −107 B, and four other Settings screens about −110 B each, plus small moves of a few bytes elsewhere. Both the Section and NotificationsScreen limits were lowered to the §10.1 cap, so there is no residue of the §2.26 raises left. No source outside `features/settings/form` changed for this record, and no limit was raised.

Headroom: NotificationsScreen has 12 B left and Settings section 48 B under 45 KB. Each new registry entry adds about 65–85 B to both, so the next Settings screen needs a trim or a §2.26 raise.
