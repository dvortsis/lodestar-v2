# X-pseudo-text-gaps (runner brief, 2026-11-22Z)

Kind: helper (UI, i18n). Dep X-ui-checks-3 (#1045) is merged. Its decisions file (`docs/decisions/2026-11-22-X-ui-checks-3.md`, "Findings") lists two pieces of hard-coded English that the nightly pseudo-locale check carries as `KNOWN_GAPS` in `web/scripts/pseudo-locale.mjs`. Fix both, then remove both entries so the check guards them.

## The two gaps
1. **Relative times: "just now".** `web/src/lib/format/relative.ts` keeps a built-in `{ en: 'just now', zh: '刚刚' }` fallback ("until W4 supplies it"). No catalogue has a `just_now` message, and `RelativeTime.svelte` (`web/src/lib/ui/labels/`), the only caller, doesn't pass `opts.justNow`.
   - Add a `just_now` message (en and zh) in a shared catalogue, and pass it from RelativeTime.svelte. Name the catalogue file at step 0.
   - Decide whether relative.ts keeps its fallback for callers outside the UI (tests, plain `formatRelative`). If it stays, say why in the decisions file.
2. **Insights annotations insert raw values.** `web/src/lib/features/insights/overview/Movement.svelte` passes the raw setting value to `m.insights_overview_note_intensity({ level })` (e.g. "aggressive"), and the raw event kind to `m.insights_overview_note_unknown({ kind })`.
   - Look each value up in the catalogue: the intensity levels have Settings labels already; reuse those message keys if they exist.
   - An unknown kind must stay readable without inserting English: say "an unrecognised event" or show the kind as code. Pick the clearer one at step 0.
   - en and zh both.

## Rules
- **Size:** RelativeTime.svelte may be in Search's first view. Search route JS (initial) has no headroom (45 kB cap, §10.1; the owner wants the smallest UI). Measure with `npm run size:ci` before and after, and send every changed entry. No limit raise without the numbers first.
  - **`web/.size-limit.mjs` is NOT in your owns** while X-search-chunk-retry runs; it also measures Search. If a limit must move, send the numbers and wait.
- Never beside X-insights-story (`insights/overview/**`, the Insights messages); it isn't running now.
- No hex values, no arbitrary values. Reuse existing tokens.
- **Tests:**
  - a unit test that RelativeTime passes the catalogue's just_now, in both locales;
  - a unit test for Movement's two notes: a known level gives the translated label; an unknown kind inserts no raw text;
  - the pseudo-locale unit test still passes, with the two `KNOWN_GAPS` entries removed.
  - Run the pseudo project locally if feasible (`web/e2e/README.md`), or say why not.

## Owns
- `web/src/lib/format/relative.ts`
- `web/src/lib/format/format.test.ts`
- `web/src/lib/ui/labels/RelativeTime.svelte`, and its test if one exists
- `web/src/lib/features/insights/overview/Movement.svelte`, and its test
- `web/src/lib/features/insights/messages/en.json` and `zh.json`
- the shared catalogue's en and zh files for `just_now` (name them at step 0)
- `web/scripts/pseudo-locale.mjs` (the `KNOWN_GAPS` entries only)
- `docs/decisions/2026-11-22-X-pseudo-text-gaps.md`
- this slice's `runner/slices.yaml` entry

## Step 0 (before building)
1. The catalogue file for just_now.
2. The message keys for levels and unknown kinds.
3. Whether relative.ts keeps a fallback.
4. Current sizes of every entry that includes RelativeTime or Movement.
5. Files and an estimate.

## Branch and PR
Branch `slice/X-pseudo-text-gaps`, PR title "X-pseudo-text-gaps: <title>". Run the CLAUDE.md web gates. Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
