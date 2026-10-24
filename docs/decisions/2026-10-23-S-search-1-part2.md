# S-search-1 part 2 decisions

The active-filter strip, its chips and the Syntax popover (architecture §10.4, §10.13). Part 1 (#132: the shell, the bar, the editor and the error states) has its own file, `2026-10-23-S-search-1.md`. Nothing here changes an owner decision or a number.

## The strip (§10.4)

- **Chips:** every top-level clause except free text (`word`, `phrase`, `near`, `prefix`, `fuzzy` on `any`, and an `or` of only those). A negated word (`-cam`) is not free text, so it gets a chip ("Not: cam").
- **Source:** while a parse error shows, the strip keeps the last AST that parsed. W5 clears `ast` on an error, and the results still show the last good query.
- **Labels** (patterns; values through `$lib/format` only):
  - `{key}: {value}`, `{key}: at least {v}`, `{key}: at most {v}`, `{key}: {lo} to {hi}`;
  - dates: "Discovered in the last {span}", "… more than {span} ago", "… on / since / until {date}" (UTC days), "… {from} to {to}";
  - `Not: …`, `Any of: … or …`, `All of: … and …`, `A file matching: …`, nested three deep and cut at 80 characters (the accessible name keeps it all);
  - lists use `Intl.ListFormat` in the current locale.
- **Field names:** `name:` is the glossary's "Raw name" and `title:` "Resolved title"; `path:` is "File paths".
- **#48's fields use the glossary's terms:** `seeders:` "Seeders", `leechers:` "Leechers", `trackers:` "Trackers reporting", `dht:` "DHT estimate" (all through `formatCount`), and `seen:` "Last seen" with the same date patterns as `discovered:`.
- **`time:` reads "Discovered between 22:00 and 02:00 UTC".**
  - The app sends no time zone yet, so the server reads the range in UTC (`docs/query-language.md`), and the label says so.
  - The times are 24-hour `HH:MM`, as the query writes them. `$lib/format` has no clock-time formatter, and a localized one would also need the zone.
  - When the app sends `tz`, the label should drop "UTC" and use the user's zone.
- **Values:** tag values show in mono as the server normalised them. `type` and `adult` values use labelled messages. The hash shows 12 characters and "…", with the full hash in the accessible name.
- **`cat:video` on its own** (1–100% of the files) reads "Has Video files", not "Video files: at least 1%". `bytes:` keeps its percentage.
- **The `junk:` range reads "Junk score".** The glossary's "Junk" is the over-threshold classification, and this is the 0–100 score.
- **Content-type values are this feature's own messages.** `Video`, `Software`, `Ebook` and `Other` repeat the words of the shared `video`/`software`/`ebook`/`other` keys. Those keys are file categories (used for `cat:`), not the glossary's `content_type` list, so the labels aren't copies of the same term.
- **Year, season and episode are identifiers:** plain integers, not grouped ("2019", not "2,019").
- **A `>` bound** is lowered by the server to `lo + 1` and shows as "at least" the formatted value ("Size: at least 4.00 GiB").
- **The `hidden=1` chip** reads "Including: Hidden by filters". The colon keeps the glossary term as it is, rather than lowercasing it.
- **Clear all** keeps the free text and clears `hidden` in **one history entry** (runner, 2026-10-23).
  - W5's `setAst` takes `{ hidden }` and commits it with the new query: a named hook edit in `lib/state/search.svelte.ts`, since W5's slice is finished.
  - Without it, Clear all made two entries, and Back landed on the free text with `hidden=1`, a state the user never saw.
  - Tests: W5's unit test checks one entry, and the strip's test checks Clear all pushes once.
- **Quick removals in a row all land.** Each one builds on the one in flight and finds its clause by node identity, so a press during the round trip doesn't bring the first chip back. A clause that's gone, after a new query, is left alone. One announcement names every removed filter once the last one lands.
- **If the second of two quick removals fails,** neither lands: the first was superseded by the second, which carried both. The banner's Retry repeats the second only, so the first chip stays and can be removed again. Nothing is lost and the strip always matches the URL.
- **Typing wins over a removal in flight** (runner, 2026-10-23, named hook edits in W5's `search.svelte.ts`):
  - `setText` cancels a structured edit or a parse in flight;
  - `setAst` drops its answer if the draft changed after it started;
  - `parse()` sets `ast` only for the current draft.
  - Before this, typing during a removal's round trip was overwritten by the server's text, and the chips showed the discarded draft.
- **Removing a chip rewrites the bar** with the server's text for what's left. That includes a draft that wasn't parsed yet or had a syntax error. Undo brings the draft back.
- **Removing:** a failed removal keeps the chip and shows an error banner with Retry. Focus moves to the next chip, else the editor (Chip's `fallbackFocus`), and a polite region says "Removed filter: {label}". It is a separate region from the message line, so a removal never replaces a parse error's text.
- **No animation** on change (§10.2). Chips are keyed by their AST node, not by label or position. So removing one doesn't re-create its neighbours, even one with the same label, and the focus handed to the next chip stays put.

## Loading (§10.1)

- **The strip loads only once there's a chip,** and the Syntax popover on its first open. Neither is in `/search`'s first load. The bar's lazy part now also loads at once when the URL carries `hidden=1`, so that chip shows without an interaction.
- **The Syntax button is on the first load,** as a plain button that loads the popover when pressed. `app.html`'s static bar has a matching inert button, so the editor's box stays the same from first paint (`boot-shell.routes.test.ts`, ≤ 1 px). The runner confirmed this hub edit.
  - **Other languages:** the static button says "Syntax". The boot script only paints the static shell for English (`boot-locales`), as with part 1's placeholder, so a zh page never shows it and its own "语法" button causes no shift on hydrate. If a locale joins `boot-locales`, its static strings must too, or the button's width shifts when the app takes over.
- **The label code isn't a chunk of its own** (a deviation from the brief's step 9; the runner accepted it, 2026-10-23). `clauses.ts` (the label messages and formatters) loads with the bar's lazy part on the first interaction, even with no chip, instead of with the strip as the brief's step 9 listed. The bare page's first load doesn't include it either way. Splitting it would add a request on the first chip for a few KB.
- **A late popover doesn't take focus.** The Syntax popover opens once its chunk has loaded, and only if focus is still on the button (or nowhere). If the user has started typing meanwhile, it stays closed. A chunk that fails to load leaves the button as it was.
- **Measured on `/search`:** search JS 39.56 → 40.17 kB (`main` at 21cff90), CSS 19.83 kB unchanged, and Lighthouse 94,845 B (mobile LCP about 1.5 s, CLS 0). Every class the strip and popover use was already in the global sheet.

## The Syntax popover

- **Examples:** the seven query rows of `docs/query-language.md` "Start here", plus `~bnuny`. The eighth row there is a hash paste, not a query.
- **"Try"** closes the popover, sets the example, submits it and puts the caret at the end of the bar.
- **Esc** in the bar closes the popover first (S-search-4's listbox will join it). Otherwise Esc clears the text, as in part 1.
- **No `/help/syntax` link** until a slice owns that page.

## Follow-ups

- **`/help/syntax`:** link it from the popover when it exists.
- **Backend:** per-clause source text in `/parse`, so a chip can show the user's own clause.
- **`time:`:** use the user's zone once the app sends `tz`.
