# SRC-17: provenance in the detail drawer, Show unverified toggle and badge, Source/Verified facets
**Model:** Sonnet (UI, pattern-following). A fresh Opus reviewer follows. Runner brief, 2026-11-05.

## Spec
`docs/plan/2026-11-01-v06-sources.md` §4 "SRC-17"; architecture §18.6, §18.8; SRC-3's search fields (`verified:no` hidden unless asked for; source and network fields) and SRC-14's API. UI rules in CLAUDE.md: tokens only, no hex, the §10.2 spacing ramp, smallest UI, the UI glossary (`docs/ui-glossary.md`) for names.

## Do
- Detail drawer: "Found by: DHT · Nyaa" (the item's sources from its provenance), and the claim-vs-verified differences when an item has both (e.g. a claimed size or file list that differs from the verified metadata).
- Search: a "Show unverified" toggle (off by default, matching SRC-3: unverified hidden unless asked for) that adds the query term, and an **Unverified** badge on such results (icon + text).
- Source and Verified facets wired to SRC-3's fields.
- `private_source` items never show anything that would leak their source to a non-admin view beyond what the API already returns; don't add API fields.
- en and zh keys; vitest for each state; Playwright if the search or detail screens have specs you touch; keep the size budget.

## Own
`web/src/lib/features/detail/provenance/**`, `web/src/lib/features/search/unverified/**`, and one registration line each where the drawer and the search filter bar mount them; `docs/decisions/2026-11-05-SRC-17.md`.

## Done when
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, Playwright for touched specs (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`). PR "SRC-17: …" against `main` from `slice/SRC-17`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
