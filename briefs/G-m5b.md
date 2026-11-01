# G-m5b: M5 exit, part 2: the design-review pass
**Model:** Opus (the review). Fixes go to follow-up slices the runner launches. Runner brief, 2026-11-01. It's the second half of the M5 exit gate (`runner/slices.yaml` → `G-m5`; part 1 is G-m5a).

## Why
`docs/architecture.md` §10.17 and §14 (M5 row): the milestone exits only when a `design-review` pass over the M5 UI has **no blocking findings**. The design decided with the owner comes first (CLAUDE.md "UI rules", §10, `docs/design-tokens.md`, `docs/ui-glossary.md`). `ui-design-skills` fills only what those leave open.

## Read first
- CLAUDE.md "UI rules" and "Order of authority"; `docs/architecture.md` §10 (all of it, especially §10.1 budgets, §10.2 spacing ramp, §10.18 tokens and contrast, §10.19 brand), §10.17; `docs/design-tokens.md`; `docs/ui-glossary.md`.
- The design-review skill (`anthropic-skills:design-review`). Apply it by concept and spirit for Svelte; it never overrides the owner's choices.
- `web/e2e/` (the smoke matrix and its baselines under `web/e2e/**/__screenshots__` or the repo's snapshot folder): reuse them as the visual source rather than generating thousands of new images.

## Do
1. **Review every M5 surface** at the §10 breakpoints (at least 360, 1024 and 1920), in Riverstone light and dark plus two other themes, in comfortable and compact density, and with reduced motion:
   - Search (bar, builder, chips, facets, results, table, bulk, states);
   - the detail drawer and `/t`;
   - Compare, Discover and Alerts;
   - the Insights overview, the 5 stage pages, the digest and print;
   - Settings (each group), the first-run wizard, Filters & Safety;
   - the keyboard sheet, `/logs`, About, Updates, and the shell (nav, top bar, toasts, banners, empty/error/loading states).

   Use the existing Playwright mock server and the smoke baselines. Take new screenshots only where a state has none.
2. **Findings:** a report at `docs/review/2026-11-01-m5-design-review.md`. One row per finding: surface, breakpoint/theme, a screenshot path, severity, the rule it breaks (a § in the plan or tokens doc, or a named skill rule for gaps), and a concrete fix.
   - **Blocking:** breaks an owner decision or a §10 rule; fails contrast (§10.18) or axe; broken layout or overflow at a §10 breakpoint; unusable by keyboard; text that misleads.
   - **Important:** a clear inconsistency or usability problem that doesn't break a rule.
   - **Polish:** everything else.

   Group fixes into slice-sized batches (≲ 500 lines each, disjoint files) and say which model fits each (Sonnet for layout and CSS, Opus for anything with state or safety).
3. **Fix nothing in this PR** except a one-line, obviously safe fix with no visual side effects elsewhere, and list any you make. The PR is the report plus its screenshots.
4. Keep screenshots small (PNG, cropped to the issue), and say the total size in the PR (LFS is blocked; keep it under about 5 MB).

## Own
`docs/review/2026-11-01-m5-design-review.md`, `docs/review/m5-shots/**`, and a short `docs/decisions/<date>-G-m5b.md` (how the review ran, counts per severity).

## Done when
The report covers every surface above, each blocking finding has a concrete fix and a batch, and `npm run check` passes if you touched anything in `web/`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, the counts (blocking / important / polish) and the proposed fix batches. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Don't background long jobs with `&`. Chromium is at /opt/pw-browsers; never run `playwright install`.
