# X-detail-attributes-ui

The runner's brief, 2026-11-28Z. The slice entry is in `runner/slices.yaml` (runner records r23).

## Why
- X-detail-attributes (#1172) made `GET /torrents/{hash}` carry the parsed release attributes (`TorrentDetail.attributes`).
- The detail view doesn't show them yet.

## What
1. A Release list in Technical details (or wherever §10.5 puts it), built from `TorrentDetail.attributes`.
2. A plain label per key, saying what the key means to a user (what `vcodec` says, for example), from the key list in `docs/decisions/2026-11-28-X-detail-attributes.md`.
3. en and zh strings (the zh strings machine-drafted, and the PR says so).
4. Nothing shown for null or `{}`.
5. Plugin-sourced title, alt and year are already dropped by the server when closed; show only what the API returns.
6. The glossary entries for any new user-facing term.
7. `.size-limit.mjs` changes only for the detail chunk: measured, and rounded up to 64 B.

## Owns
`web/src/lib/features/detail/**`, `docs/ui-glossary.md`, `web/.size-limit.mjs`, and `docs/decisions/*-X-detail-attributes-ui.md`.

## Rules
- Tokens only: no hex and no arbitrary values. Spacing stays on the §10.2 ramp.
- Use the owner's fonts and the minimalist style (CLAUDE.md UI rules).
- Unit tests and an e2e assertion for the list and for the empty case.
- No fixed sleeps; waits have a deadline of at least 10 s.
- `npm run check`, vitest, `npm run build`, `npm run size:ci`, and the detail e2e specs must pass. Use `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` and `--trace off`.
- Snapshot baselines change only through CI's `.update-snapshots` flow (web/e2e/README.md).

## Step 0
Before writing code, send the runner:
- where the list goes in §10.5;
- the label for each key;
- whether any key needs a value formatter.

## Done
- A PR titled `X-detail-attributes-ui: <title>` from `slice/X-detail-attributes-ui`.
- Send `ready <full sha>` when CI is green.
