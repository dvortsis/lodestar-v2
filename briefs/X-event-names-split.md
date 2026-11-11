# X-event-names-split (Sonnet helper): split the on-demand log event-names chunk by component

Assigned by the runner on 2026-11-11.

## Read first
The `X-event-names-split` entry in `runner/slices.yaml` (binding). The size-limit config is `web/.size-limit.mjs`.

## Owns
`web/src/lib/features/settings/logging/event-names.ts`, `web/scripts/event-names.mjs`, `web/.size-limit.mjs`, `web/scripts/event-names.test.ts`, `web/src/lib/features/settings/logging/event-labels.svelte.ts`.

## Build
Generate one on-demand chunk per component (crawler, search, sources, companions, workers, update, …, whatever the event-name prefixes give) instead of one chunk for all names, and load only the chunk a shown event needs. Each new log event should then stop needing a size-limit raise (#637 raised 14.9 to about 16.6 KB). Set a per-chunk budget in `.size-limit.mjs` with modest headroom; the total first-load budget (§10.1) must not grow. Same labels and fallback as today for an unknown name; tests for the generator and for lazy loading.

## Checks
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for any e2e spec that covers the Logging screen (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never `playwright install`).

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/X-event-names-split` from the latest `origin/main`; PR title `X-event-names-split: <title>`.
- Before review: mutation-check every guard you add (the last reviews were mostly about guards with no test). No fixed sleeps in tests; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. Keep the PR under ~1,500 changed lines (generated files aside); if it grows past that, tell the runner before splitting.
- Anything beyond this brief (a new crate, a `Cargo.toml` change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `X-event-names-split: PR #<n> ready for review, head <sha>` with CI green. Blocked: `X-event-names-split: blocked on <what>`. The runner reviews and merges; you never merge.
