# X-hidden-count-wide-shift (runner brief, 2026-11-28)

**Slice:** X-hidden-count-wide-shift. Sonnet, UI.
**Branch:** `slice/X-hidden-count-wide-shift` from current `main`.
**PR title (exact):** `X-hidden-count-wide-shift: From 128rem the toolbar controls stay put when the hidden count arrives`
**Owns:** `web/src/lib/features/results/ResultsToolbar.svelte`, the results toolbar CSS
(`toolbar.css` or wherever the 128rem/L3 rule lives), `web/e2e/search-toolbar.routes.test.ts`,
`docs/decisions/*-X-hidden-count-wide-shift.md`, plus files refined at step 0.

## Source
X-hidden-count (#1173, merged) shows "N hidden by filters · Show" (narrow: "N filtered · Show")
after the rows land, and the note must move nothing. Below 128rem (2,048 px) that holds. From
128rem the controls follow the count (layout L3), so Sort and View move sideways when the note
arrives. The e2e at `web/e2e/search-toolbar.routes.test.ts` (the width loop near line 555) only
checks x below 2,048 px: `if (width < 2048) expect(after?.x).toBe(sort?.x);`.

## Do
1. **Step 0.** Read the toolbar layout at every breakpoint (the §10.4 plan in
   `docs/architecture.md`, `docs/design-tokens.md`, and X-hidden-count's decisions file). Send
   the runner:
   - how L3 places the count, the note and the controls today, and why the controls move;
   - your fix. For example, the note may take a slot that doesn't push the controls, or the
     controls may anchor independently of the count's width. Name what the owner-decided
     layout requires and how the fix keeps it;
   - the refined owns.

   Then wait for the reply.
2. **Test.** Drop the `width < 2048` guard, so Sort's x is held at every width in the loop.
   Add 2,560 px if L3 still applies there. Keep the height and row-y checks. Condition waits
   only, at least 10 s, and no fixed sleeps.
3. **UI rules** (CLAUDE.md): tokens only, no hex or arbitrary values, spacing on the §10.2
   ramp, and the `docs/ui-glossary.md` terms. No wording change is expected; if you need one,
   say so at step 0.
4. **Size.** Run `npx size-limit`. If a budget moves, state the measured number and the reason
   in the decisions file.
5. **Avoid:** `web/src/lib/features/detail/**` and `docs/ui-glossary.md` (X-detail-attributes-ui
   is open), and `web/src/lib/features/results/ResultsPanel*.test.ts` (X-drawer-detach-flake is
   open). If you need one of these, say so at step 0.
6. Decisions file: `docs/decisions/<date>-X-hidden-count-wide-shift.md`.
7. Follow `.claude/skills/steward/SKILL.md`, and run the CLAUDE.md pre-push checks for `web/`,
   including the toolbar e2e spec. Report `ready <PR> <sha>` to the runner session.
