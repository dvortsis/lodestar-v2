# CMP-9c (Sonnet, /security-review): the wizard's "Install qBittorrent" option

Assigned by the runner on 2026-11-11. CMP-6 (#639) and CMP-5g-2 (#653) are merged, so the companion install and guided-setup API is on `main`.

## Read first
- The `CMP-9c` entry in `runner/slices.yaml`, and `docs/plan/2026-11-06-v06-companions.md`: the slice table (~line 73), the UI rules at ~line 166 and wave 6 (~line 211).
- `docs/architecture.md` §10.10.10 (the wizard's download-client step) and §18.10 (companions); the UI rules in `CLAUDE.md`; `docs/design-tokens.md`; `docs/ui-glossary.md`.
- The CMP-6 decisions file and `runner-state:briefs/CMP-6.md`, for the install API, its states and its refusals.
- The existing `web/src/lib/features/wizard/DownloadStep.svelte` and the wizard tests.

## Owns
- `web/src/lib/features/wizard/DownloadStep.svelte` and `web/src/lib/features/wizard/companion/**` (new components, stores and tests).
- A shared edit, adding keys only: `web/src/lib/features/wizard/messages/en.json` and `zh.json`.
- `docs/decisions/2026-11-11-CMP-9c.md`.
- Nothing in `crates/`. If the API is missing something you need, stop and tell the runner. Don't add endpoints and don't hand-edit `openapi.json` or `schema.d.ts`.
- CMP-5g-3 (`crates/lodestar/src/companions.rs`, `manager.rs`) and the Companions screen (CMP-9a, later) are not yours.

## Build
- On native platforms only (the API says which; never sniff the user agent), the download-client step offers **Install qBittorrent for me** beside the existing choices.
- **Before anything installs,** a confirmation shows the program, version, licence, size, SHA-256 and where it will run, all from the server's catalogue reply. Nothing installs until the user confirms. The server decides; the UI never builds an install request from its own guesses.
- **During the install,** show its progress and states from the API. Install failures and refusals use plain words (glossary), never raw codes. Retry and cancel go through the API's own actions only.
- **Afterwards,** the step continues with the companion as the download client. A generated credential is never shown again after its one reveal, if the API offers one.
- Docker and Unraid builds don't show the option. Say so in a test.
- Tokens only, no hex or arbitrary values. Keyboard and screen-reader access follow the existing wizard steps.

## Checks
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` (a new budget line only if a new chunk appears; tell the runner), plus `npx playwright test` for any wizard e2e spec you touch, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`.
Tests: native shows the option and Docker hides it; the confirmation shows every field and nothing installs before confirming; a refused or failed install shows plain text; a credential isn't shown twice.

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/CMP-9c` from the latest `origin/main`; PR title `CMP-9c: <title>`.
- Before review: mutation-check every guard you add. No fixed sleeps in tests; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. Keep the PR under ~1,500 changed lines; if it grows past that, tell the runner before splitting.
- Anything beyond this brief (a new dependency, an API change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `CMP-9c: PR #<n> ready for review, head <sha>` with CI green. Blocked: `CMP-9c: blocked on <what>`. The runner reviews and merges; you never merge.
