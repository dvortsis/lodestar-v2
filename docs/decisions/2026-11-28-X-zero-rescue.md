# X-zero-rescue decisions

Builds U8 and U9 and the query-bar items of P10 from the 2026-11-13 UI review (`docs/review/2026-11-13-ui-ux/`), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Spec: §10.4 "No results" and "Natural phrasing, the bridge".

## What it does
- **"Did you mean" (U8).** After a zero-result answer, a filter typed as words (`seeders>5`, `size > 4gb`, `age>7d`) is rewritten into the clause it means (`seeders:>5`) and offered as one button. It only offers: applying is one explicit click, which puts the text in the bar and searches it.
  - The rewrite is pattern matching over the **server's** field list (`GET /search/fields`, names and aliases, and the operators each field accepts). The client holds no field names, so with no list there is no suggestion. A quoted phrase is never rewritten.
  - A suggestion is shown only after the server's own parser accepts it (`/parse`).
  - While it shows, the panel no longer says "Try fewer or different words" (§10.4: the advice never says that when the user typed a filter).
- **Natural-phrasing chips (U9).** On a zero-result answer, when `/parse?natural=true` returns chips and the proposal differs from the text, the chips show unasked with **Apply filters**. The visible "Natural phrasing" button still lets anyone ask at any time.
- **Each server question is asked once per query text.** An aborted effect drops an answer for older text, so a stale answer cannot land on a newer query.
- **Query-bar copy (P10).** The error line's lead was "Can't read this query:", and the server's own messages often begin "…can't read…". The lead is now "Check your query:". Autocomplete value rows no longer carry the "Value" prefix (a kind label stays where it tells the user something: Field, Title, Filter, Did you mean, Recent, Saved search).
- **No error on an unfinished token.** While the caret is in the token being typed and the suggestions are open, a parse error in that token is not shown (no underline, no `aria-invalid`, no message). Pressing Enter shows it at once, and so does moving the caret out of the token or closing the suggestions. The bar's Assist chunk reports the caret (`onsuggest`).

## Calls the runner made at step 0
- **§10.4 wins over the slice title.** The title says "natural-phrasing chips only when asked"; §10.4 and the r5 decision say the chips are offered unasked on a zero or small answer. This slice follows §10.4.
- **"Zero or few results" is built as zero only.** The panel's empty block is the one mount point. A few results is not rescued here.
- **Owns widened** to `results/Rescue.svelte` and a small mount edit in `ResultsPanel.svelte` (the lazy import, `rescuing` and the hint it hides).
- **zh strings are machine-drafted.** A native review comes later.

## Files
- New: `results/Rescue.svelte`, `assist/rescue.ts`, `query/unfinished.ts`, their tests and `e2e/search-rescue.routes.test.ts`.
- Changed: `query/Live.svelte`, `assist/Assist.svelte`, `results/ResultsPanel.svelte`, `messages/en.json` and `zh.json` (`search_rescue_*`, `search_query_error_lead`), the three e2e/unit tests that matched the old lead.

## Sizes (`npm run size:ci`)
- New entry **Search zero-result rescue (on demand)**: 1,633 B, limit 1.64 KB. The chunk loads only for an empty answer.
- **Search route JS (initial)**: 45,000 → 45,056 B (measured 45,002 B, +2 B, rounded up to 64 B).
- **Search results (lazy)**: 44,224 → 44,352 B (measured 44,300 B, +76 B, rounded up to 64 B).
- `rescue.ts` imports no code from `fields.svelte.ts`, and the chips use their own markup and message key, so the Builder and Natural phrasing chunks keep their sizes.
