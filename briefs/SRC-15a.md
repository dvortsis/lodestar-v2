# SRC-15a: Settings → Sources (grouped rows, status pills, toggles, priority, yield sparkline)
**Model:** Sonnet (UI, pattern-following). A fresh Opus reviewer follows. Runner brief, 2026-11-05.

## Spec
`docs/plan/2026-11-01-v06-sources.md` §4 "SRC-15a/b"; architecture §18.8, §10.10, §10.10.2 (the Settings registry), §10.14; the API from SRC-14 (`GET /sources` with manifest, endpoints, consent state, health, 24 h yield, unique finds; `PATCH` for egress, schedule, agents). Generated from the API (`schema.d.ts`). UI rules in CLAUDE.md: tokens only, no hex, Tailwind's default scale on the §10.2 ramp, smallest UI.

## Do
- Rows grouped by source kind; each row: name, status as icon + text (never colour alone), an enable toggle, priority, and a tiny in-house SVG yield sparkline (no chart library).
- Enabling a source whose consent class needs more than a toggle shows a clear "needs setup" state and **does not** bypass the server's consent gate; the consent dialogs, credentials and purge are **SRC-15b**, not this slice.
- Register the screen through `screen.ts` with the Settings registry; no new route.
- en and zh message keys; vitest for the row states; a Playwright spec against the fixture server if the sources screen has one; keep the size budget (`npx size-limit`).

## Own
`web/src/lib/features/sources/**` (excluding anything SRC-15b will own: consent dialogs, credentials, purge) and `docs/decisions/2026-11-05-SRC-15a.md`.

## Done when
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, Playwright for any spec you touch (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`). PR "SRC-15a: …" against `main` from `slice/SRC-15a`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
