# S-insights-3: the Insights print layout and the digest preview
**Model:** Sonnet: UI per §10.9.6/§10.9.7 on existing APIs. Runner brief, 2026-10-31. The last Insights slice in M5 (`runner/slices.yaml`). No `/security-review`: it renders server text and charts, and prints.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md`, `docs/ui-glossary.md`, `docs/metrics.md` (formatting; never invent a series), `docs/design-tokens.md`.
- `docs/architecture.md` §10.9.3 (integrity), §10.9.6 (chart craft; **"Print layout: the overview prints as a one-page report (context bar, KPIs, trend, funnel, exceptions), with the URL and generation time in the footer"**), §10.9.7 (digest contents), and §10.14 (copy).
- `docs/decisions/2026-10-31-S-insights-2.md`, `-S-insights-imports.md`, `-S-settings-2b.md` (the digest-preview mount at `data-slot="digest-preview"` in `features/settings/screens2/InsightsScreen.svelte`), and `2026-10-23-B11c.md` (the digest API).
- `GET /digest/preview` in `web/src/lib/api/schema.d.ts`, and the overview code in `web/src/lib/features/insights/overview/**`.

## Do
1. **Print layout (`web/src/lib/features/insights/print/**`):** `@media print` styles plus any small print-only markup, so printing `/insights` gives a one-page report: the context bar, the five KPIs with Δs, the trend, the funnel and the exceptions, with the page URL and generation time in a footer.
   - No navigation, buttons, tooltips or interactive chrome in print.
   - Charts print in the light theme with legible ink (tokens only; no hex). Use the forced-light print tokens if the theme compiler has them; otherwise say what you did.
   - The print CSS must not add to first load: put it in the route's lazy CSS or a `media="print"` stylesheet loaded on demand. Measure it.
   - If the existing layout can't fit one A4/Letter page at default margins, say what you dropped and why.
2. **Digest preview** in the Insights & digest Settings screen, mounted at `data-slot="digest-preview"`:
   - Load `GET /digest/preview` on demand (when the screen opens, or behind a "Preview digest" button; pick the cheaper one and say why).
   - Render the plain-text version as **text only**, in a scrollable `pre`-like block with the mono font. Never inject the HTML version as markup. If you show the HTML version at all, use a sandboxed `iframe` with `srcdoc` and no scripts, or don't show it; say which.
   - It reflects the *saved* settings, not unsaved edits. Say so in one line of help text.
   - States: loading, error (as text), and digest off (say it's off and how to turn it on).
3. **Sizes:** `/insights` first load and the Settings first-view gates must not grow. Add new entries for the print CSS and the preview chunk at measured + 10%. Never raise an existing limit; if you can't fit, stop and report the numbers. Notifications' first view has ~0.3 kB of headroom, so don't touch shared Settings code.
4. **i18n:** `insights_print_*` and `insights_digest_preview_*` keys in en and zh in the features' own messages, regenerated with the repo's generator.
5. **Tests:**
   - vitest: the preview renders text and never markup (a `<b>` in the response shows literally); loading, error and off states.
   - Playwright: `page.emulateMedia({ media: 'print' })` on `/insights` hides chrome and shows the footer; the preview on the Settings screen with axe; keyboard-only.
   - Condition waits only, no fixed sleeps.
   - **No existing baselines should change.** If they do, stop and tell the runner.
6. **Decisions file:** `docs/decisions/<date>-S-insights-3.md`.

## Own
`web/src/lib/features/insights/print/**`, a digest-preview component under `web/src/lib/features/insights/digest/**` mounted from the existing slot (a one-line import in `InsightsScreen.svelte` is allowed), these features' message keys and regenerated maps, the Insights/Settings mock fixtures for `/digest/preview`, `web/e2e/insights-print.*`, the size-limit entries and your decisions file. **Don't edit** the Settings engine, `screens2/**` beyond that one line, `lib/state/**`, `lib/shell/**`, `lib/ui/**` or `lib/api/**`.

## Size
≲ 700 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the Insights and Settings specs, 3× for anything new. Chromium is at /opt/pw-browsers; never run `playwright install`. If the bundled headless shell doesn't match, use an untracked config with `executablePath: '/opt/pw-browsers/chromium'` and never commit it.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
