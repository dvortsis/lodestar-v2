# X-events-send-await

The runner's brief, 2026-11-28Z. The slice entry is in `runner/slices.yaml` (runner records r23, from #1168's review).

## Why
- `events.send` (the e2e events server) now waits up to 10 s for an open stream (X-live-banner-flake, #1168).
- Unawaited calls turn a failed send into an unhandled rejection rather than a test failure.

## What
1. Add `await` to every unawaited `events.send(...)` in `web/e2e/shell.routes.test.ts` and `web/e2e/ui-checks/states.routes.test.ts`. The known calls are at shell.routes.test.ts:194 and 198 and states.routes.test.ts:37, 42 and 51. Line numbers may have moved after #1173, so grep for them.
2. No other change.
3. Grep the rest of `web/e2e` for unawaited `events.send` and list any you find in the PR. Fix them too if each is the same one-word change; owns extends to those spec files.

## Rules
- No sleeps, and assertions unchanged.
- Run the two specs, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` and `--trace off`.
- `npm run check` must pass.

## Done
- A PR titled `X-events-send-await: <title>` from `slice/X-events-send-await`.
- Send `ready <full sha>` when CI is green.
