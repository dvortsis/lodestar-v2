# X-labels

Runner brief, 2026-11-21Z. Slice id `X-labels`, branch `slice/X-labels`, PR title `X-labels: <title>`. The plan entry is in `runner/slices.yaml`. The findings are I10, U16, U28 and the "Ebook"/"Video" wording in P10, in `docs/review/2026-11-13-ui-ux/01-review.md` and `04-plan-and-flow-review-moderate-user.md`, placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Read §10.4, §10.9, §10.12 and `docs/ui-glossary.md` first.

## Why
Internal values show as labels: sources `dht` and `bitmagnet`, "Found by: dht", languages `fr`/`zh`, codecs and sources `h264`, `dts-hd-ma`, `web-dl`, `hdr` in the facets, Compare and the detail, release groups lower-cased in facets but upper-case in names, "Current theme: riverstone". The Builder's fields read `res`, `name`, `ext`. Insights shows UTC while Language & region says "Time zone: Automatic", so people read 22:00 as their own time.

## What
1. **One label formatter per field,** in `web/src/lib/format/` (a new module, e.g. `labels.ts`, exported from `index.ts` the way the others are): source, language (from `Intl.DisplayNames` in the UI language, with the code as a fallback), video codec, audio codec, release source, HDR, the type names, release group (as written in the name, never lower-cased for display), and theme name (from the theme's own display name). The display text comes from the glossary where it names the term, and from en and zh messages otherwise. An unknown value falls back to the raw value, never to an empty label. Unit tests for each, plus the fallback.
2. **Call sites change only their label calls:** facets, Compare, the detail, the Builder (field labels from the glossary, with the code as secondary mono text, as the rule rows do since X-safety-toggles) and Insights. A query the user types or a URL keeps the codes; only what's shown changes.
3. **Dates in the chosen time zone (U28).** Insights (and any other screen you find showing UTC) formats with the zone chosen in Language & region, through the existing `format/date*.ts` helpers, and says "UTC" only when UTC is the choice. Check how "Automatic" resolves (the browser's zone) and test both.
4. **"Ebook" vs "E-books" and "100% video" vs "100% Video":** one spelling each, from the glossary.
5. **A decisions file** `docs/decisions/2026-11-21-X-labels.md`, heading with `(#<this PR>)`, listing each field's labels and any UX call (§2.26; name it so I can flag it).

## Owns (refined at launch)
- `web/src/lib/format/**`
- the call sites' label lines in the facets, Compare, detail, Builder and Insights components (send me the exact files)
- the en and zh message files those features use, for the new label texts only
- their unit tests and e2e specs; snapshots only where a screen really changed (regenerate the CI way, list each)
- `docs/ui-glossary.md` only if a term is missing, and tell me first: it's a contract doc
- the decisions file, and this slice's `slices.yaml` entry

Send me the exact file list and your estimated changed lines before you edit. If it's over about 1,500, propose a split (for example, labels first, then the time zone).

## Bars
`runner/standards.md`, CLAUDE.md's web checks (`npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npx playwright test` for the changed specs, with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`). The label tables that aren't needed on first render load on demand, and any size-limit change needs your measured numbers first. Push once per round, and send "ready <pr> <sha>" when CI is green.
