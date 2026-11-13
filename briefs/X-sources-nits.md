# X-sources-nits (runner brief, 2026-11-13)

Follow-up to #816 (X-sources-restructure), from the runner's fresh review rv-816-3d77669e (MERGE, 4 optional findings).

**Owns:**
- `web/src/lib/features/sources/**`;
- `web/e2e/settings-sources*.test.ts`;
- `web/e2e/docs/shots.docs.ts` (the Sources shot only);
- the regenerated Sources screenshot under `docs/user/`;
- `docs/decisions/2026-11-13-X-sources-restructure.md`.

**Do:**
1. **O1.** Add "(#816)" to the decisions file heading, as `docs/decisions/README.md` asks. Record the Account `setupNeed` change there: a disabled Account row whose password is required now shows "Off" with no password note, where before it showed "Needs setup". The Account switch opens the Account dialog, so nothing is lost.
2. **O2.** Repair the header comments split mid-sentence in `SourcesScreen.svelte` (lines 2–8) and `SourceRow.svelte` (lines 2–9).
3. **O3.** Add two tests:
   - Enter and Space on a row's expand button leave the switch alone and send no request;
   - an Overlay case in the Account-password `setupNeed` test.
4. **O4.** Pluralise the "Showing N of M sources" count properly (ICU plural or the existing helper), en and zh.
5. **Docs screenshot.** Regenerate the Sources screenshot that `e2e/docs/shots.docs.ts` produces, so the user guide shows the new layout.

**Hard limits, as in X-sources-restructure:**
- no change to the consent dialogs or the switches' behaviour;
- the §2.49 strings test stays.

Steward skill; fresh self-review; PR title "X-sources-nits: …".
