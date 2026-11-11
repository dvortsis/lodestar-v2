# CMP-9a (Sonnet, /security-review): Settings → Companions screen

Assigned by the runner on 2026-11-11. CMP-6 (#639), CMP-5g-1 (#645), CMP-5g-2 (#653) and X-cmp-egress-prelaunch (#672) are merged, so the companion install, guided-setup and egress API is on `main`. X-cmp-native-handover (in progress, another session) adds no HTTP route or response shape; if it does after all, the runner tells you.

## Read first
- The `CMP-9a` entry in `runner/slices.yaml` (its notes are part of this brief), and `docs/plan/2026-11-06-v06-companions.md`: the slice table (~line 71), the UI rules (~line 146 and ~166) and wave 7.
- `docs/architecture.md` §10.10.8 (screens), §10.10.13 (About), §10.14, §18.10 (companions); the UI rules in `CLAUDE.md`; `docs/design-tokens.md`; `docs/ui-glossary.md`.
- The decisions files for CMP-6, CMP-5g-1, CMP-5g-2 and `2026-11-11-X-cmp-egress-prelaunch.md` (lines 38–43: the SOCKS5 residuals), and `runner-state:briefs/CMP-6.md`, for the API, its states and its refusals.
- `web/src/lib/api/openapi.json` / `schema.d.ts` for the companions endpoints (read only), and an existing Settings screen (`web/src/lib/features/settings/updates/`) for the pattern; `shell/screens.ts` registers screens by glob.

## Owns
- `web/src/lib/features/settings/companions/**` (screen, components, stores, messages, unit tests) and `web/e2e/companions.spec.ts`.
- Shared edit, one key only: `settings_about_conn_companions` in `web/src/lib/features/settings/about/messages/en.json` and `zh.json`, relabelled to describe companion downloads (Q1(a)).
- `docs/decisions/2026-11-11-CMP-9a.md`.
- Nothing in `crates/`. Don't add endpoints and don't hand-edit `openapi.json` or `schema.d.ts`. If the API is missing something, stop and tell the runner.
- Not yours: the wizard (`web/src/lib/features/wizard/**`, CMP-9c, running) and the dependency chip (`web/src/lib/features/companions/**`, CMP-9b).

## Build
- **The list:** each companion with health, version, update notice, its route (egress), and Set up / Remove; detected companions and add-by-URL; paste an API key; compose / Unraid template copy where the API offers it.
- **The install confirmation:** before anything installs, show program, version, licence, size, SHA-256 and where it runs, all from the server's catalogue reply. Nothing installs until the user confirms. The server decides; the UI never builds an install request from its own guesses.
- **Credentials:** a generated credential is revealed once only, if the API offers it, and never again. Pasted secrets go in password fields, are never echoed back, and never appear in logs, URLs or error text.
- **Egress:** saving the egress setting restarts the companion; say so before saving. Say a companion's route is copied from its plugin at its first start. While Prowlarr is on SOCKS5, warn about (or refuse, if the API refuses) email notifications (MailKit connects directly) and a FlareSolverr pairing, and say per-indexer proxies bypass the route.
- **Logs:** show the API's companion log lines as text, never as HTML.
- Failures and refusals use plain words (glossary), never raw codes; an unknown code gets a generic sentence that doesn't print the code.
- Platform differences come from the API (never sniff the user agent).
- Tokens only, no hex or arbitrary values. Keyboard and screen-reader access follow the existing Settings screens.

## Checks
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` (a new budget line only if a new chunk appears; tell the runner), and `npx playwright test e2e/companions.spec.ts` with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`. Never run `playwright install`.
Tests: the confirmation shows every field and nothing installs before confirming; a credential isn't shown twice; a pasted key is never rendered back; a refusal or failure shows plain text; the egress save warns about the restart; the SOCKS5 warnings show; log text isn't rendered as HTML.

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/CMP-9a` from the latest `origin/main`; PR title `CMP-9a: <title>`.
- Mutation-check every guard you add. No fixed sleeps; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. The estimate is about 1,000 lines; if it grows past ~1,500, tell the runner before splitting.
- Anything beyond this brief (a new dependency, an API change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `CMP-9a: PR #<n> ready for review, head <sha>` with CI green. Blocked: `CMP-9a: blocked on <what>`. The runner reviews and merges; you never merge.
