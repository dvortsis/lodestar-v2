# S-search-1: Search — query bar, highlighter, active-filter strip, chips
**Model:** Sonnet (helper: a §10.4 screen composed over W5's search state; the query is parsed only by the server, and the client tokenizer only colours the user's own text). **Branch:** `slice/S-search-1`. No `/security-review`: nothing here touches auth, filters or untrusted torrent data. It renders the user's own text and the server's AST as text only. If you find you need `{@html}`, stop and say so.

**Spec:**
- `runner/standards.md` (the bar), `docs/owner-decisions.md`, and CLAUDE.md's UI rules (tokens only, no hex or arbitrary values, `ui-design-skills` only for gaps).
- Architecture:
  - §10.4: the "Layout", "Query bar" and "Active-filter strip" bullets, plus the "Error" and "Refining" rows of the states table as they apply to the bar;
  - §10.1: budgets, debounce, "tiny tokenizer for syntax highlighting";
  - §10.2: query bar geometry, type scale, motion: "filter changes never re-animate";
  - §10.3: the URL is the state;
  - §10.13: combobox pattern, focus, announcements, single-key rule;
  - §10.14;
  - §10.15: sticky query bar below `large`;
  - §10.16: `/` and `Esc`;
  - §20.3 and §20.9: the `hidden` flag, and what Adult and Hidden by filters mean.
- `docs/query-language.md` ("Start here", "Operators", "Fields") is the tokenizer's vocabulary and the Syntax popover's examples. `crates/search/src/query.rs` (`FIELDS`, line 268) is the authoritative field list.
- Contract docs:
  - `docs/design-tokens.md` §2 (`--color-accent-ink`, `--color-muted-text`), §6 (`--query-bar-height`, `--radius-field`), §7 (`--rail-width`, `--results-min-width`, breakpoints) and §8 (`--font-mono`, weights);
  - `docs/metrics.md` §1 and §3: the formatters for chip values;
  - `docs/ui-glossary.md`.
- What exists on `main` (read it, don't copy it):
  - `web/src/lib/state/search.svelte.ts` (W5). `search.text`, `ast`, `parseError {code,message,start,end}`, `parsing`, `q`, `hidden`, `setText` (200 ms debounce, then `GET /parse`, replace entry), `submit` (push), `setAst` (POSTs `/search` with `limit: 1` for the canonical text, push), `reset`, `setHidden` and `sync()`. A parse error sets `ast = null`.
  - `web/src/lib/api/client.ts` (`ApiError`, `isAbort`) and `schema.d.ts`: `AstNode` (17 ops) and `ParseResponse {ast, canonical}`. A parse error is `ErrorBody {code: "query_syntax", field: "q", start, end}`, with `start`/`end` as **UTF-8 byte offsets** (`crates/api/src/search.rs` `parse_error`).
  - `web/src/lib/keys/README.md`: `keys.register`, `pushScope`, `DEFAULT_KEYS['search.focus'] = ['/']`. `$lib/keys` is loaded lazily by the shell. **Never import it statically into the route** (W9 decisions).
  - Primitives:
    - `web/src/lib/ui/labels/Chip.svelte` (`onremove`, `removeLabel`, `fallbackFocus`, `data-chip-group`);
    - `web/src/lib/ui/labels/Mono.svelte`;
    - `web/src/lib/ui/overlays/Popover.svelte`;
    - `web/src/lib/ui/feedback/Banner.svelte`;
    - `web/src/lib/format/`.
  - **`docs/decisions/2026-10-22-W9.md`: read all of it.** It covers the first-load headroom, the static boot shell's `/search` heading, and the "Search should check LCP again" note.
- Skills (standards §3, "Any screen or primitive" row): `layout-spacing`, `layout-grids`, `responsive-layout`, `typography-ui`, `component-geometry`, `iconography-ui`, `motion-ui`, `application-interface-design`, `minimalist-visual-style`. `git clone --depth 1 https://github.com/dvortsis/skills /tmp/skills`.

## Own
- `web/src/lib/features/search/query/**` (new) and its tests:
  - the bar and its editor;
  - `tokenize.ts`, `spans.ts`, `clauses.ts`;
  - the strip and the Syntax popover;
  - `commands.ts`;
  - `path.ts` (`ensureSearchPath`, step 8).
- `web/src/lib/features/search/messages/{en,zh}.json` (new). **The message merge reads only `features/<feature>/messages/`** (`web/scripts/messages.ts` `discover`), so search messages can't live under `query/`. This file is the search feature's: keys start `search_`. Yours use `search_query_*`, `search_filters_*` and `search_page_*`. Later search slices append keys under their own sub-prefix.
- `web/src/lib/features/search/README.md` (new): a short map of the folder (who owns which subfolder, the message sub-prefixes, the page regions).
- **`web/src/routes/search/**`: the search page shell.** You own the shell. Later slices get named mount lines in it:
  - S-search-2: the facet rail and the results list;
  - S-search-3: the toolbar;
  - **the drawer mount point is yours too**. S-detail owns what renders in it (step 2).
- e2e:
  - `web/e2e/search-query.routes.test.ts` and `web/e2e/search-query.keyboard.test.ts` (new);
  - `web/e2e/search-query.fixture.test.ts` (new, local-only, step "Tests");
  - **the `search` object only** in `web/e2e/support/routes.ts` (add its `mock`). S-detail edits the `torrent` object in the same file.
  - The regenerated `search` baselines (`e2e/smoke.test.ts-snapshots/search-*`, `e2e/theme-snapshots/search--*`), through the `web/e2e/.update-snapshots` marker flow in `web/e2e/README.md`. Never commit PNGs made in the session.
- `docs/decisions/<date>-S-search-1.md`.
- **Conditional hub lines:** only if your page changes the `<h1>` box, edit the `#boot-main` contents in `web/src/app.html` and the heading expectations in `web/e2e/boot-shell.routes.test.ts` to match. Keep the owner's condition on that file (mobile LCP ≤ 2.5 s, W9 decisions "Owner permission") and say so in the PR.
- **Don't edit:**
  - `web/src/lib/state/**` (W5);
  - `web/src/lib/keys/**` (S-keys renames ids there);
  - `web/src/lib/shell/**`;
  - `web/src/lib/ui/**`, `web/src/lib/charts/**`, `web/src/lib/format/**`;
  - `web/src/lib/api/**` (generated);
  - `web/src/lib/i18n/messages/**`;
  - `web/src/lib/features/detail/**` (S-detail) and `web/src/lib/features/shortcuts/**` (S-keys);
  - `web/src/app.d.ts` (S-detail adds `PageState`);
  - `web/.size-limit.mjs`, `web/lighthouserc*.json`, `web/playwright.config.ts`, `web/package.json` and the lockfile;
  - `web/src/app.css`;
  - other routes.
- **Open PRs nearby:**
  - **#48 (M4)** adds an AST op `time_of_day` and the num fields `seeders`, `leechers`, `trackers`, `dht` and `seen` (plus a `tz` field) to the generated schema. When it merges, merge `origin/main`. Your exhaustive `switch` (step 5) then fails type-checking until you label them, which is the point.
  - **#128 B-update and #129 B-log2 part 2** regenerate `openapi.json`/`schema.d.ts`. Take them as they come and never hand-merge those files. #130 (B11b part 2) merged as `57296e6` after this brief was drafted; start from that `main` or later.
- **In-flight web slices with lines in files near yours:**
  - **S-detail:** `routes.ts` `torrent` object, `app.d.ts`, `.size-limit.mjs`.
  - **S-keys:** `lib/keys/defaults.ts`, `lib/shell/Chrome.svelte`.
  - **S-compare, S-discover, S-alerts, S-insights-1:** their own `routes.ts` objects.

## Build
1. **Measure first.** Run `npm ci && npm run build && npx size-limit` on a clean `main`. Today it reads search route JS 39.52 / 45 kB, CSS **19.82 / 20 kB** and total 81.99 / 100 kB. Then `npm run lhci` with `CHROME_PATH=/opt/pw-browsers/chromium-1194/chrome-linux/chrome`. W9 left `total-byte-weight` at about 101.9 KB of 102,400 B. Put the numbers in the PR.

   **The CSS sheet is global** (Tailwind scans all of `src/`), so every new utility class anywhere costs bytes on `/search`. Runner rule for this round: the 20 KB is the owner's number.
   - Use only utilities the build already emits (grep `build/_app/immutable/assets/*.css`).
   - Write anything new in component `<style>` blocks, with `@reference "../../../app.css"` (path relative to the component) and `@apply`.
   - If it doesn't fit, stop and ask.

2. **The page shell** (`routes/search/+page.svelte`):
   - **The heading:** keep W9's `<h1>` "Search" box (the boot-shell drift test allows ≤ 1 px), or move both together (Own, conditional).
   - **The search landmark:** under the heading, a `role="search"` landmark holding the query bar, then the filter strip. Use the `<search>` element instead if svelte-check accepts it.
   - **The grid below the strip:** a track for the rail (`--rail-width`), a results track (`minmax(--results-min-width, 1fr)`) and a trailing `auto` track for the drawer, at `xx-large` and up. Below `x-large` it's one column.
     - Give the regions ids (`search-rail`, `results`, `search-drawer`).
     - Give them no landmark role and no content until their slices fill them. An empty labelled landmark is axe noise.
     - Leave a comment per region naming its slice.
   - **The drawer mount point** is a lazy glob, so S-detail and you never edit each other's files:
     - `const host = import.meta.glob('/src/lib/features/detail/DetailHost.svelte')`. It's empty until S-detail merges.
     - When `'detail' in page.state` and the glob has the module, import it and render it in `search-drawer`.
     - When the module isn't there, render nothing.
     - S-detail decides the drawer's position at each breakpoint. Your track only gives it a place in the grid.
     - Keep it out of the route's first-load chunk (check the manifest).
   - **Sticky bar:** below `large` the query bar is sticky at the top (§10.15).
   - **Sync:** call `search.sync()` from an effect on `page.url` (W5's README).
3. **The editor**, `QueryBar.svelte` plus an editor component.
   - **Why contenteditable:** §10.4 distinguishes tokens by **weight + mono font + colour**. A native `<input>` with an overlay can't do that, because a token in another font or weight no longer lines up with the input's caret. So the editor is a single-line `contenteditable="plaintext-only"` element.
   - **ARIA:**
     - `role="combobox"`, `aria-expanded="false"`, `aria-autocomplete="list"` and `aria-haspopup="listbox"`. The listbox arrives with S-search-4; leave a documented slot for it.
     - `aria-labelledby` pointing at a label ("Search torrents", sr-only is fine).
     - `aria-describedby` pointing at the message line (step 4).
     - `aria-invalid` on a parse error.
     - `spellcheck="false"`, `autocapitalize="off"`, `autocorrect="off"`, `enterkeyhint="search"`, `inputmode="search"`.
   - **Look:** `--query-bar-height` (48 px), `text-body-lg`, `rounded-field`, `border-field-border` and the standard focus ring. The placeholder comes from a message through `data-placeholder` and CSS. It's never the accessible name.
   - **Behaviour:**
     - Every input re-renders the token spans from `tokenize()` and restores the caret by UTF-16 offset. Don't re-render while `isComposing` (IME); re-render at `compositionend`.
     - Paste is plain text, with newlines turned into spaces.
     - **Undo and redo are your own** (Ctrl/⌘+Z, Ctrl/⌘+Shift+Z, Ctrl+Y), with typing bursts coalesced. Rebuilding the DOM breaks the browser's own stack.
     - Each change calls `search.setText(text)`.
     - Enter calls `search.submit()`.
     - Esc closes an open popover first. Otherwise it clears the text (undo brings it back).
     - A long query scrolls horizontally and keeps the caret in view.
   - **Stop and say so** if this can't pass the tests below within the line budget. Don't drop mono or weight highlighting, and don't add an editor dependency.
4. **The tokenizer, the error underline and messages.**
   - **`tokenize(text): {kind, from, to}[]`:**
     - pure, linear and never throws;
     - the token texts concatenate back to the input;
     - kinds: `field` (`name:`, … from the `FIELDS` list, M4's included; an unknown `foo:` is text), `value`, `range` (a value with `>` `<` `>=` `<=` `..`), `operator` (`|`, `OR`, `AND`, `NEAR/n`, a leading `-` or `!`, `(`, `)`, a trailing `*` and a leading `~`), `phrase` (quoted, an unterminated one running to the end) and `text`.
   - **Classes,** as `@apply` rules in the editor's `<style>` block (step 1):
     - `field`: `font-mono font-semibold text-primary`;
     - `value`: `font-mono`;
     - `range`: `font-mono font-medium text-accent-ink`;
     - `operator`: `font-semibold text-accent-ink`;
     - `phrase`: `font-medium`, with its quotes in `text-muted-text`;
     - `text`: plain.

     That's the gap filled by `typography-ui`; record it. The mono font loads only when a mono token renders, so a bare `/search` never fetches it.
   - **The error underline:**
     - `spans.ts` turns the server's byte span into UTF-16 offsets over the text it was parsed for (TextEncoder).
     - Underline it with `underline decoration-wavy decoration-error` only while `!search.parsing` and the text is still the one parsed.
     - An empty span (`start === end`) marks one character, or the end.
   - **The message line under the bar always reserves one `text-small` line**, so nothing shifts (CLS < 0.02):
     - **a parse error** shows an icon plus "Can't read this query:" (`search_query_error_lead`) and then the server's `message`. Only `query_syntax` exists, in English; a localized reason code is a backend follow-up, so say so in the PR.
     - **any other error** (`network`, `http_5xx`) shows an error `Banner` "Couldn't check the query", with Retry calling `search.submit()`.
     - **an empty query never shows an error.** The bare page's own parse must not paint anything: not in lhci, not in the smoke tests, and not in W0's `web/e2e/routes.test.ts`, which loads `/search` with no mock (so `/api/v1/parse` fails) and expects no axe violations.
5. **The active-filter strip** (`clauses.ts` + strip component):
   - **Source:** the clauses come from `search.ast`. While a parse error is showing, use the last AST that parsed (W5 clears `ast` on an error, and the results still reflect the last good query).
   - **Top-level clauses** are the `of` of a root `and`, else `[root]`; `all` gives none.
   - **What gets a chip:** every clause except free text (`word`, `phrase`, `near`, `prefix`, `fuzzy` with `field: "any"`, and an `or` of only those). Free text stays in the bar.
   - **Labels:** a message per op/field, with values through W8 only:
     - `size`, `largest`, `piece`: `formatBytes` on `lo`/`hi`;
     - `files`: `formatCount`; `junk`: `formatScore`;
     - `year`, `season`, `episode`: identifiers shown as plain integers, not grouped. Record that.
     - `date`: `{ago}` through `formatSpan` ("Discovered in the last 7 days"), `{at}` through `formatDate`;
     - `share`: `formatShare(lo)` / `formatShare(hi)` with the group name;
     - `ext`: `.mkv, .mp4`;
     - `flag`: "Private: yes";
     - `tag`: the key label, with values in mono as the server normalised them. `type` and `adult` values use labelled messages (the glossary's `content_type` and `adult_kind` lists).
     - `hash`: `Mono`, shortened to 12 characters plus "…", with the full hash in the accessible name;
     - `file`, `not`, `or`: built recursively ("Not: …", "Any of: … or …", "A file matching: …"), depth-limited and truncated.
   - **The `switch (node.op)` is exhaustive** with a `never` check, and has a runtime fallback label for safety.
   - **The `hidden=1` URL flag** gets a chip labelled with the glossary term `hidden_by_filters` ("Including hidden by filters"). Its remove calls `search.setHidden(false)`.
   - **Remove:**
     - `search.setAst(rootWithout(clause))`: a single remaining conjunct is unwrapped, and none left is `{op:"all"}`.
     - A failure keeps the chip and shows an error `Banner` with Retry.
     - Focus moves per `Chip`'s `fallbackFocus` (the next chip, else the editor).
     - The polite live region says "Removed filter: {label}".
   - **Clear all** shows from one chip up. It keeps the free-text clauses, removes the rest, and clears `hidden`.
   - **No animation** on change (§10.2).
   - The strip and its labels **load lazily once there's a chip**, so the bare page doesn't pay for them (step 9).
6. **The Syntax popover:** a "Syntax" button beside the bar opens a `Popover` (lazy on first open).
   - Eight examples from `docs/query-language.md` "Start here", in mono, each with a "Try" button that sets and submits it.
   - No link to `/help/syntax`, which no slice owns yet. Name it as a follow-up.
7. **Keyboard:**
   - `pushScope('search')` while the page is mounted.
   - Register `search.focus` (its `/` comes from `DEFAULT_KEYS`), scope `search`, focusing the editor with the caret at the end.
   - Both go through `import('$lib/keys')` after mount, never a static import. The shell replays the first key once the registry loads (W9 decisions).
   - S-search-2 registers its own commands in the same scope. Say so in the folder README.
   - Don't register `dismiss`: Esc is the editor's own key handling.
8. **URL state and the drawer.**
   - All query state goes through W5 (`q`, `sort`, `hidden`, `exact`), and every other parameter is kept.
   - **S-detail opens the drawer with a shallow `pushState` to `/t/{hash}` plus the current `?search`.** The page stays mounted and `search.q` keeps reading the same query. But W5 commits with `goto(<current path>?…)`, which from `/t/…` would navigate to the full detail page.
   - So export `ensureSearchPath()` from `query/path.ts`: when `page.url.pathname !== '/search'`, `replaceState('/search' + page.url.search, {})`, which closes the drawer.
   - Call it before every `setText`, `submit`, `setAst`, `setHidden` and `reset` you make. Document it for S-search-2 and S-search-3, whose facet clicks and toolbar must do the same.
9. **Budgets (§10.1; owner numbers, never changed):**
   - **Required:** `npx size-limit` passes (search JS ≤ 45 kB, CSS ≤ 20 kB, total ≤ 100 kB), and `npm run lhci` passes (mobile LCP ≤ 2.5 s, CLS ≤ 0.02, TBT ≤ 100 ms, total-byte-weight ≤ 102,400 B; desktop LCP < 1.0 s).
   - **Lighthouse measures the bare `/search`.** So the route's first chunk holds only the shell, the bar and a minimal editor. The tokenizer can stay if it fits. Load the following lazily, in chunks the bare page doesn't fetch:
     - the strip and labels (when a chip exists);
     - the popover (on open);
     - the key registration (with the shell's lazy `$lib/keys`).
   - **If a budget still fails, stop.** Don't change `.size-limit.mjs`, `lighthouserc*.json`, `app.css`, the shell or another slice's files. Open a draft PR with the measurements per chunk and message the runner.
10. **Messages:**
    - en in full. Glossary terms come from the shared keys (`m.hidden_by_filters()`, `m.discovered()`, `m.content_type()`, `m.adult_kind()`), never copied.
    - zh as proposals for your own keys only, with the same placeholders. Missing zh only warns.
    - Wording follows §10.14 and the glossary: "torrent", never "result", in labels.
11. **Decide yourself, and record them:**
    - the token-class mapping;
    - chip wording, and which ops get chips;
    - Esc clearing the text;
    - undo coalescing;
    - the plain integers for year, season and episode;
    - the message key names;
    - the region ids.

    **Stop and ask the runner:**
    - any budget overrun;
    - a new design token;
    - a change to W5 state, `lib/keys`, `lib/ui` or the shell;
    - a new dependency;
    - dropping any part of §10.4's styling;
    - anything needing a backend change.

    The only backend changes in view are a localized parse-error reason, and per-clause text in `/parse`, which would let chips show the user's exact clause. Both are follow-ups to report, not to build.

## Tests
- **vitest:**
  - **`tokenize`:**
    - each kind;
    - `size:700MB..1.5GB`, `discovered:<7d`, `big NEAR/5 bunny`, `-cam`, `!cam`, `(a | b) c`, `comput*`, `~bnuny`;
    - an unterminated quote, an unknown field, CJK and emoji;
    - a property test on random strings: it never throws, and the texts concatenate to the input;
    - 10 KB of input in linear time.
  - **`spans`:** ASCII, CJK and emoji offsets; spans that are out of range or inside a code point clamp.
  - **`clauses`:**
    - every one of the 17 `AstNode` ops, chip or text, with the exact en label;
    - remove: unwrap, the last one becomes `all`, text is kept;
    - Clear all;
    - the `hidden` chip.
  - **The editor:**
    - typing calls `setText`, and Enter calls `submit` (a fake search state via `createSearchState(nav)`);
    - the caret stays put after a re-render;
    - IME composition doesn't re-render;
    - a multi-line paste becomes one line;
    - undo and redo;
    - Esc;
    - the ARIA attributes, and `aria-invalid` on an error;
    - the message line's height stays fixed.
  - **`ensureSearchPath`.**
- **Playwright** (mock harness; Chromium at `/opt/pw-browsers`; **on a private port**: a copied, uncommitted config with `--strictPort`, never the shared 4173):
  - **`search-query.routes.test.ts`:**
    - typed tokens get their classes;
    - a `mock.error('parseQuery', 400, 'query_syntax', …, { field: 'q', start, end })` shows the underline and the message;
    - chips render from a mocked `ParseResponse`;
    - removing one sends `searchPost` with the AST and the URL becomes the canonical text;
    - back and forward restore it;
    - the `hidden=1` chip;
    - axe passes (light and dark, 1366 and 390) with chips and with an error showing;
    - the bar is sticky at 390;
    - no layout shift on the bare page.
  - **`search-query.keyboard.test.ts`:** `/` focuses after the first key, then type, Enter, Tab to a chip, Delete, focus to the next chip and then the editor, and Esc clears.
  - **The `search` route entry's `mock`** covers the bare page's `parseQuery`.
- **Fixture backend** (`web/e2e/search-query.fixture.test.ts`, local only):
  - Imports `@playwright/test` directly, not the auto-mock `test`, and skips unless `E2E_FIXTURE_URL` is set.
  - Run it with a copied local config (`testMatch: /\.fixture\.test\.ts$/`, `--strictPort`) against `npm run build && LODESTAR_HTTP_BIND=127.0.0.1:<port> LODESTAR_FIXTURE_PASSWORD=… cargo run -p lodestar -- serve --fixture <tmp>` (`bench/README.md` "Fixture backend"). Log in with `POST /api/v1/login`.
  - `lantern type:movie` shows a "Type: Movie" chip, and removing it gives `?q=lantern`.
  - `seeders:>=1` shows the server's "not available yet" message, until #48 merges.
  - Put the results in the PR. CI doesn't run it until S-e2e wires a fixture job.
- **Baselines:** the `search` key screen changes, so use the marker flow and pull the bot's commit.

## Done when
- These pass in `web/` after merging `origin/main`:
  - `npm run check`, `npx vitest run`, `npm run build` and `npx size-limit`;
  - `npm run lhci`;
  - `npx playwright test` (your specs, `boot-shell.routes.test.ts`, and the smoke matrix with the regenerated baselines).
- Axe is clean.
- No hex, arbitrary or off-ramp values, and strings only in `features/search/messages`.
- **About 1,500 changed lines or fewer, baselines excluded.** If it's bigger, split it into two PRs on this branch in sequence, the second after the first merges:
  1. the page shell, the editor, the tokenizer and the error states;
  2. the strip, the chips and the Syntax popover.
- A PR titled `S-search-1: Search — query bar, highlighter, active-filter strip, chips`.

## Report
The PR description lists:
- **Code:** the files; the editor's API, for S-search-4's listbox; the token-class mapping; and every chip label pattern.
- **Contracts for other slices:** the page regions and their ids; the drawer glob contract and `ensureSearchPath`, for S-detail, S-search-2 and S-search-3; and the `search` key scope.
- **Measurements:** sizes before and after per chunk (size-limit and lhci byte weight, LCP, CLS); axe results; and the fixture-backend run.
- **Hub edits:** any app.html or boot-shell change, and the `routes.ts` change.
- **Follow-ups:** localized parse reasons, per-clause text, `/help/syntax`, and M4's new ops if #48 hasn't merged.
- **Owner decisions:** any refinements (`docs/owner-decisions.md` §2.19).

## Runner decisions (2026-10-23, apply over anything above)
- **CSS (owner's §10.1 limit, unchanged):** global CSS is 19.82 of 20 KB and nine web slices are in flight, so treat the headroom as zero. Don't add Tailwind utility classes that aren't already in main's built CSS. Put everything route-only in a component `<style>` block with `@reference` to `app.css`, using token variables and ramp spacing only (no px, no hex). It ships in the route chunk. Report `npx size-limit`'s CSS figure before and after. If it moves at all, say why in the PR; if you can't stay under 20 KB after merging main, stop and ask.
- **Route budgets (runner, §2.19):** each route's own JS + CSS is ≤ 15 KB gzipped (compare, discover, alerts, alerts/[id], t/[hash], drawer), and Insights is ≤ 45 KB including uPlot (§10.1). Dialogs and heavy pieces load lazily.
- **Fixture backend:** CI uses the OpenAPI mock. Do one manual check against `serve --fixture` on a private port and report it. Fixture specs stay opt-in through `E2E_FIXTURE_URL`; S-e2e will wire them into CI.
- **Shared files:** edit only the lines this brief assigns you in a shared file. On a conflict, merge `origin/main` and regenerate generated files (messages, OpenAPI, size entries) with the repo's tooling; never hand-merge them.
- **On main since this brief was drafted:** W9 (b8c884d) and B11b part 2 (#130, 57296e6: the Insights Δs, `export.csv` and the `insights` SSE event). M4 (#48, swarm fields) is still in review: use its fields only if it has merged when you start, otherwise use the null state.
- **Model: Opus** (runner decision). The first-load budget work and the contenteditable editor are correctness- and performance-critical.
- **First-load headroom:** about 0.5 KB of Lighthouse's 102,400 B is left on `/search`, and that limit is the owner's. Make room inside your scope first. Load everything the bare `/search` doesn't need on first paint lazily: chips, the highlighter's full grammar, the filter strip's dialogs. Measure each piece. If the query bar itself can't fit, stop and message the runner with the measured breakdown (fonts, CSS, JS chunks) and the options. Don't trim the shell or the fonts yourself.
- **Editor:** a contenteditable editor with its own undo stack is accepted, since §10.4 needs token weight, mono and colour. Test IME composition (zh/ja), paste of rich text (plain text only goes in), screen-reader name, role and value, and undo/redo. Never use `{@html}`: build the token spans as DOM nodes from the parser's spans.
- You own `features/search/messages/` for the whole search family. Use the `search_query_` sub-prefix for your keys.
