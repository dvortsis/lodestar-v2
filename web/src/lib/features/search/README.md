# features/search

The search screen (architecture §10.4). The page shell is `routes/search/+page.svelte`. Decisions: `docs/decisions/2026-10-23-S-search-1.md` (the bar), `2026-10-23-S-search-1-part2.md` (the strip and the Syntax popover) and `2026-10-24-S-search-2a.md` (the results list).

## Who owns what

| Path                                | Slice      | What                                                                                               |
| ----------------------------------- | ---------- | -------------------------------------------------------------------------------------------------- |
| `query/`                            | S-search-1 | The query bar and editor, the highlighter, the error line, the active-filter strip, Syntax popover |
| `messages/{en,zh}.json`             | all search | One catalogue for the search family; each slice appends keys under its own sub-prefix              |
| `routes/search/+page.svelte`        | S-search-1 | The page shell; later slices get named mount lines in it                                           |
| `results/` (list); facet rail (2b)  | S-search-2 | Mounted in `#results` (and `#search-rail`); see `results/README.md`                                |
| results toolbar                     | S-search-3 | Mounted at the top of `#results`                                                                   |
| listbox (autocomplete)              | S-search-4 | Attaches to the editor (see "The editor" below)                                                    |
| `features/detail/DetailHost.svelte` | S-detail   | Rendered in `#search-drawer` from a lazy glob                                                      |

**Message sub-prefixes:** `search_page_*`, `search_query_*` and `search_filters_*` belong to S-search-1. Later slices use their own (`search_results_*`, `search_facets_*`, `search_toolbar_*`, …). Glossary terms come from the shared keys (`m.hidden_by_filters()`), never copies.

## Page regions

Under the heading, the `<search>` landmark holds the bar, its message line and the active-filter strip. Below it, a grid with three regions (§10.15 tracks: one column below `x-large`, rail + results at `x-large`, rail + results + drawer from `xx-large`):

| id              | Filled by                   | Notes                                                                                           |
| --------------- | --------------------------- | ----------------------------------------------------------------------------------------------- |
| `search-rail`   | S-search-2                  | No landmark role until it has content.                                                          |
| `results`       | S-search-3, then S-search-2 | Toolbar, then the list.                                                                         |
| `search-drawer` | S-detail                    | `DetailHost.svelte` renders while `'detail' in page.state`; it positions itself per breakpoint. |

The grid's rule lives in `results/results.css`, which the page awaits before it mounts the results panel: nothing paints in the regions before then (S-search-2 moved it from `query/bar.css`, which loads without waiting, so the panel could paint one column before the rail's track arrived). **The first slice that paints into a region on first load moves the rule into the route's own stylesheet** (and pays its bytes).

## Contracts for the other search slices

- **`ensureSearchPath()`** (`query/path.ts`): call it before every search write (`setText`, `submit`, `setAst`, `setHidden`, `setSort`, `setExact`, `reset`), so a facet click or a toolbar change made while the drawer is open (shallow `/t/{hash}?…`) puts the URL back on `/search` instead of navigating to the full detail page.
- **Key scope `search`:** the page pushes it and registers `search.focus` (`/`) in `query/commands.ts`, from `import('$lib/keys')` after the first interaction. S-search-2 registers j/k (`results/commands.ts`) in the same scope, the same way; `c` and `o` are S-detail's, acting on the focused row (`setCommandTarget`). Never import `$lib/keys` statically into the route.
- **The first load is full** (§10.1: 102,400 bytes in Lighthouse; about 7.3 KB left in October 2026). Other routes no longer add to it beyond about 27 B each: `npx size-limit` budgets the route table and the shared vendor chunk (`docs/decisions/2026-10-24-search-first-load-routes.md`). Anything the bare page doesn't paint loads lazily, and a lazily imported component's own `<style>` still loads with the route (kit), so put lazy styles in a stylesheet imported after mount (`query/bar.css` is the model). Writing a utility class name anywhere in `src/`, comments included, adds it to the global 20 KB sheet: use token variables for anything the sheet doesn't already have.

## The strip

The active-filter strip sits under the bar inside the `<search>` landmark. It loads with the bar's lazy part when the URL has a query or `hidden`, and renders once there's a chip. Until its chunk arrives, `Live.svelte` holds its first line with an unseen, inert chip and "Clear all" (`filter-strip-reserve`). The results panel paints nothing while a pasted link's query is still parsing, so the reserve is in place before any row: no layout shift (S-search-2).

## The editor (for S-search-4)

`QueryBar.svelte` is the static part. `Live.svelte` (lazy) attaches `query/editor.ts` to the `contenteditable="plaintext-only"` element:

- `attach(el, { onchange, onsubmit, onescape, onrender, composing })` returns `{ text, set(text, base?), focusEnd(), setError(range), undo(), redo(), destroy() }`.
- `onescape` returns true when a popup took the key. The Syntax popover closes there today, and the listbox should join it.
- The element has `role="combobox"`, `aria-autocomplete="list"`, `aria-haspopup="listbox"` and `aria-expanded="false"`. The listbox sets `aria-controls` and `aria-expanded` while it is shown, and `aria-activedescendant` for the active option.
- Token spans carry `qt-*` classes (`bar.css`); `tokenize.ts` is the highlighter only. The server parses (`/parse`).
