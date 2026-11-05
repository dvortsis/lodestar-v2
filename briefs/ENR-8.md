# ENR-8: Detail drawer, adult-enrichment section
**Model:** Sonnet (display only). A fresh Opus reviewer follows. Runner brief, 2026-11-05T19:20Z.

## Why
ENR-7 (#504, merged af4e9f7d) added the `adult_enrichment` section to `GET /torrents/{id}`. The API returns it only when "Show adult content" is on. When it's off, the field is absent, not empty. ENR-8 shows that section in the detail drawer and on `/t/[hash]`. It's on the ENR-G path.

## Read first
- `docs/plan/2026-11-04-v06-rulepacks.md` §4.3, "ENR-8". The section sits under the summary and shows studio, date, scene title, performers (per Q4) and "Matched by ThePornDB/StashDB". The scene link opens externally. The section is hidden entirely when the API omits it.
- `docs/decisions/2026-11-05-ENR-7.md`, for the field shape, the always-null scene link for now, and Public viewers.
- `web/src/lib/api/schema.d.ts`, for the generated `adult_enrichment` types.
- `web/.size-limit.mjs`. **The Detail drawer has only 24 B of headroom (28,000 B).** So the section must be an **on-demand chunk**: a lazy import that loads only when the API returned the field. List it as a new on-demand size-limit entry, next to `detailLazy`. The drawer's first-view entry must not grow by more than the one dynamic import. Don't raise any existing limit. If you can't avoid a raise, stop and tell the runner with the measured number (§2.26).
- The `docs/design-tokens.md` and §10.14 drawer patterns. Use tokens only: no hex values and no arbitrary values.

## Do (branch `slice/ENR-8` from `origin/main`)
1. Add a component in `web/src/lib/features/detail/adult/`, plus the one mount line in the drawer's section list and on `/t`.
2. **Render rules:**
   - Show nothing when the field is absent.
   - Render all API text as text, never `{@html}`. Treat every value as untrusted.
   - Show performers as a plain list of names. Note truncation when `performers_truncated` is set.
   - Name the source as "Matched by <plugin name>".
   - Today the link is always null. When it's present, open it in a new tab with `rel="noopener noreferrer"`, and only if it's `https`. Otherwise show no link.
3. Add en and zh messages in `web/src/lib/features/detail/messages/`, under keys namespaced for this section.
4. **Tests** (vitest):
   - the section is hidden when the field is absent;
   - it renders all fields;
   - script-like text shows as text;
   - there's no link when it's null or not https;
   - truncation is noted;
   - the lazy chunk isn't loaded when the field is absent.

   No fixed sleeps.

## Own
`web/src/lib/features/detail/adult/**`; the section's message keys; one mount line each in the drawer section list and on `/t`; the new size-limit entry. Name the shared lines in the PR.

**Don't touch:** `crates/**`, and the other detail sections.

## Done when
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` (nothing over and no raised limits), and `npx playwright test` for any affected spec. Chromium is at `/opt/pw-browsers`, and the path rule is in `web/e2e/README.md`.

Open the PR "ENR-8: Detail drawer adult-enrichment section" with the label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head through the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
