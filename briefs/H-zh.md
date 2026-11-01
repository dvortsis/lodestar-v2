# H-zh: fill the missing zh messages
**Model:** Haiku (mechanical translation fill). The runner reviews; an Opus reviewer samples it. Runner brief, 2026-11-01. Slice entry: `runner/slices.yaml` → `H-zh`. It's an M5 exit-gate dependency (G-m5).

## Why
On main, 473 of 1,843 message keys have no zh entry, so zh users see English for them. Per feature:

| Feature | Missing |
|---|---|
| insights | 153 |
| filters | 90 |
| settings | 86 |
| i18n (shared) | 57 |
| auth | 39 |
| search | 38 |
| shortcuts | 10 |

Also, 19 zh entries are identical to the English, and some of those may be untranslated. Run the count yourself on current main; it may have moved.

## Read first
- `docs/architecture.md` §10.14 (i18n), `docs/ui-glossary.md` (the plain-words rule and any zh terms it fixes), CLAUDE.md "User documentation" tone (plain, matter-of-fact).
- `web/scripts/messages.ts` (how feature messages merge into `web/messages/`) and how keys, placeholders and plurals are written in the existing en and zh files.
- Existing zh entries in the same feature: match their terms and tone (e.g. how "torrent", "magnet", "crawler", "index" and "Settings" are already rendered).

## Do
1. For every en key without a zh entry, in `web/src/lib/features/*/messages/zh.json` and `web/src/lib/i18n/messages/zh.json`, add a zh translation:
   - Simplified Chinese, plain and short, the same meaning as the en string. No added or dropped information, and keep the en string's punctuation intent (full-width punctuation in zh).
   - **Placeholders and markup are copied exactly:** `{name}`, plural or select blocks, and any tags. The set of placeholders in zh must equal the set in en.
   - Technical terms the glossary keeps in English (e.g. BEP numbers, `magnet:`, product names, env var names) stay in English.
   - Content-safety wording (filters, 18+, holds) must keep its exact meaning; don't soften or strengthen it.
2. Review the 19 identical entries: translate the ones that are plain English words, and leave identifiers, numbers and product names as they are.
3. **Shared glossary keys** (`web/src/lib/i18n/messages/`) are listed in `docs/ui-glossary.md`. If a glossary term has no zh rendering there, add the zh term to the glossary in the same PR, one line per term.
4. Regenerate the merged catalogues with the repo's generator (`web/scripts/messages.ts`, or the npm script that runs it). Never hand-edit `web/messages/*.json`.
5. **Add a check**, a small vitest or a step in the existing messages test: every en key has a zh key, and the placeholder sets match. Then the gap can't silently come back.

## Own
- `web/src/lib/features/*/messages/zh.json`, `web/src/lib/i18n/messages/zh.json`, the regenerated `web/messages/zh.json`
- zh lines in `docs/ui-glossary.md`
- the new completeness test

Don't edit en files or any code.

## Size
Large in lines but mechanical; one PR is fine. Say the line count in the PR.

## Done when
These pass in `web/`:
- `npm run check`
- `npx vitest run` (including the new completeness test)
- `npm run build`

Sizes: the zh message maps load per locale; report any size-limit entry that moves, and none may go over.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the counts (keys filled per feature). Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
