# X-detail-depth (runner brief, 2026-11-22Z)

Track U, from the UI review of 2026-11-13 (`docs/review/2026-11-13-ui-ux/`), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Kind: helper (Svelte), Sonnet, with a fresh Opus review. Deps merged: X-results-drawer-adaptive, X-labels.

## Findings in scope
- **I16:** full parsed metadata. Provenance is done (#474), and Related is GS-4's.
- **U13, U14.**
- **I28:** the file filter's label.
- **P11 and 02 §4e.**
- **I26's detail composition.**
- **02 §5.5:** "Not this one" and the ID links.
- **The slice title's items:** a Swarm section, the §10.5 actions menu, Open in torrent client, Technical details, back links, and Compare showing Differences only.
- **Spec:** `docs/architecture.md` §10.5 and §10.6.

## Leave alone
- `detail/related/**` (GS-4), provenance, and the adult mask.
- Never run beside GS-4 or ENR-13b.
- **"Open in torrent client"** is the magnet link or handler only. Sending to a configured download client and its settings belong to X-client-handoff, so add no client-settings UI here.
- **No server change.** If a field the detail needs isn't in the API, list it at step 0. Don't fake it.

## Step 0 (send me, then wait for "go")
1. **Per finding:** what the user sees now, what changes, and how you'll check it, at 390, 1024 and 1440 px, in the drawer and on the full page.
2. **Refined owns:** the exact files, your e2e specs, and the detail and compare en and zh message keys. Add `web/.size-limit.mjs` only if needed, by entry name. Other slices hold that file in turn now, so ask before planning an edit.
3. **Size:** measure the detail and compare entries with `npm run size:ci` on main, and give the headroom. No raise without the measured numbers sent to me first.
4. **Data:** which API fields back each new section (the Swarm section, Technical details, full parsed metadata), and anything missing.
5. Files and an estimate. If it's over about 1,500 changed lines, propose a split.

## Rules
- **The owner's design (§10) comes first,** then `docs/design-tokens.md` and `docs/ui-glossary.md`. `ui-design-skills` only fills gaps.
  - Tokens only, no hex, and spacing from the §10.2 ramp.
  - Status is always icon plus text.
- **No fixed sleeps.** Condition waits need deadlines of at least 10 s. Run keyboard and axe checks on the specs you touch.
- **Focus order follows the visual order** at every width (WCAG 2.4.3). Don't reorder with CSS `order` alone.

## Owns (to be refined at step 0)
- `web/src/lib/features/detail/**`, except `detail/related/**`
- `web/src/lib/features/compare/**`
- `docs/decisions/2026-11-22-X-detail-depth.md`

## Branch and PR
- **Branch:** `slice/X-detail-depth`. PR title "X-detail-depth: <what the user gets>", named at step 0.
- **Gates:** `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, and `npx playwright test` for the specs you touch (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never run `playwright install`).
- **Screenshots** before and after, at 390 and 1440 px, go in the PR description.
- **Pushing:** push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
