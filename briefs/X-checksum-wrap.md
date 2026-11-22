# X-checksum-wrap (runner brief, 2026-11-22Z)

From #1034's review. On the install dialog (`web/src/lib/features/settings/companions/InstallDialog.svelte:53-56`), the 64-character SHA-256 is a `break-all` mono caption. At the large layout its 478 px column fits 63 characters, so it wraps 63+1: one stray character on a line of its own. It predates #1034.

## Do
1. Show the checksum as two 32-character halves, with a break opportunity only between them, so it wraps evenly (32+32) wherever the full value doesn't fit on one line, and stays on one line where it does.
   - Each half doesn't break internally while it fits. At phone width (390 px, 16 px gutters) check that a half still fits; if one ever can't, let it break (never overflow sideways).
   - Selecting and copying the text gives the 64 characters with no space or newline added. Check it with a Playwright clipboard or selection read, not by eye.
   - Screen readers read one value. No `aria-label` duplicating the text.
2. No hard-coded hex values or arbitrary Tailwind values. Spacing from the §10.2 ramp.
3. Add or extend an e2e check: at 1366 px (large) and 390 px, the `data-offer-sha` box holds two lines of 32 characters or one of 64, never a line under 32. Read line boxes with `Range.getClientRects()`, or compare each half's top. Condition waits only, with deadlines of at least 10 s.
4. Measure sizes with `npm run size:ci`. If a chunk grows, send the measured numbers before raising any limit (rounded up to 0.01 kB).

## Owns
- `web/src/lib/features/settings/companions/InstallDialog.svelte`
- the e2e spec that already covers this dialog (name it at step 0), or a new `web/e2e/install-dialog-checksum.routes.test.ts`
- `web/.size-limit.mjs` (only if a measured size needs it)
- `docs/decisions/2026-11-22-X-checksum-wrap.md`
- this slice's `runner/slices.yaml` entry

## Gates
The CLAUDE.md web gates (check, vitest, build, `npm run size:ci`, the Playwright spec). Step 0 first: say which spec covers the dialog, the markup you'll use, and the sizes. Then build, push once, and send "ready <pr> <full sha>".
