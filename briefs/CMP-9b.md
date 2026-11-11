# CMP-9b: Companion dependency chip (Needs X · Set up), mounted in Sources
**Model:** Sonnet, with `/security-review` (its Set up starts an install; the server decides). A fresh Opus reviewer follows.

Runner brief, 2026-11-11T14:12Z. Slot 8. Deps merged: CMP-6, CMP-10, CMP-9a (#681, 59c2d16d), SRC-15b (#552).

## Read first
- `runner/slices.yaml` CMP-9b entry; `docs/plan/2026-11-06-v06-companions.md` (CMP-9b row and wave notes); `docs/architecture.md` §10.10.8 and §18.10.
- CMP-10's decisions file and the structured Needs state in the Sources DTO (read the shape from `web/src/lib/api/schema.d.ts`; never hand-edit it).
- CMP-9a's screen: `web/src/lib/features/settings/companions/` (InstallDialog.svelte takes `offer, busy, error, onconfirm, onclose`; the store owns offer and install calls).
- `docs/design-tokens.md`, `docs/ui-glossary.md`, the existing chips for keyboard and screen-reader patterns.

## Do
1. **The chip** in `web/src/lib/features/companions/**`: "Needs X · Set up" from the structured state. Tokens only, no hex or arbitrary values; keyboard and screen-reader access like existing chips; en and zh messages.
2. **Set up opens CMP-9a's install dialog in place** (runner call, §2.26 UX: the user stays on Sources). Import `InstallDialog.svelte` and CMP-9a's store as they are. The chip builds no request of its own; it uses the store's existing offer and install calls, and the server decides what installs. If you need an extra export or prop there, a minimal additive change in CMP-9a's folder is approved: no change to the Companions screen's behaviour, and its tests stay green. Say which lines in the PR.
3. **Mount lines now:** SRC-15b has merged, so add the one mount line each in `web/src/lib/features/sources/SourceRow.svelte` and `SourcesScreen.svelte` (approved shared edits).
4. After an install, the chip reflects the new state on the next Sources refresh, with no stale "Needs X".

## Owns
- `web/src/lib/features/companions/**`; shared: the two mount lines, a minimal additive export in `web/src/lib/features/settings/companions/` if needed, message files for your keys.
- **Don't touch:** `crates/**`, `web/src/lib/api/**` (regenerated only), the Settings → Updates files (X-win-upd-window, #693).

## Done when
- vitest covers: the chip renders from the structured state; Set up opens the dialog; confirm calls the store's install once; a server refusal shows its words; keyboard path works. Mutation-check the main guards.
- `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` (any limit change goes to the runner with the measured number), and `npx playwright test` for anything with an e2e spec (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`). No fixed sleeps.
- **Process:** steward skill, push once per round, never merge. Branch `slice/CMP-9b`; PR `CMP-9b: Companion dependency chip: Needs X and Set up in Sources`. Reply "ready" with the PR number and head SHA to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via send_message when CI is green.
