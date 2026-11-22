# X-group-chip-label decisions (#1046)

Runner brief, 2026-11-22Z, with the runner's §2.26 calls on step 0. It follows X-builder-group-case (#1037) and #1033's `FacetValue.label`. UX polish: only what's displayed changes.

## What shows the label
- **Query chips** (`query/clauses.ts`, `tagValue`): a `group:` value shows the rail's label (`rarbg` → "RARBG") when the client holds one. Otherwise it shows the value, as before.
- **The chip after a facet click:** this is the same chip, built from the new query. The click's value is one the rail just listed, so its label is already held.
- **Builder rows:** the row's accessible name is the clause's label, so it picks up the label.
- **The Builder's input stays raw** (runner, §2.26). It's the query editor, so it shows exactly what Apply writes. Showing "RARBG" there would either write `group:RARBG` or need a hidden swap back to the value: a field whose shown and written text differ surprises people, and the swap costs code in an 18 B chunk.

## Where labels come from
- **A page-session map** (`query/group-labels.svelte.ts`), filled from every `/facets` response's group facet, where the response lands (`facets.svelte.ts`). That covers the rail and the staged sheet.
- **Not a separate click path.** The brief's "the label passed with the click" is a value the same response already listed, so it is already in the map.
- **The S1-20 check on entry,** matching the server's `fit_label` (`crates/search/src/facets.rs`): the same length, and each character either the value's own or an upper-case ASCII letter whose lower case is the value's. Only that direction counts. Anything else never enters, so a label can't read as another group.
  - **NFKC:** `fit_label` also requires the label to be NFKC-stable. The client doesn't recheck that. A passing label differs from the value only in ASCII letters, so it's NFKC-stable whenever the value is, and the server sends only labels that passed `fit_label`.
- **The latest response wins** (#1046 review):
  - a value it lists with no `label` loses its entry (the server leaves the label out when the winning spelling is the value), as does a value with a failing label;
  - every value it lists moves to the newest end, so the response that carries a selected group's label can never evict it.
- **At most 500 labels,** the oldest dropped. The map lasts for the page session, so a facet refresh or a query without the group facet keeps them.
- **Text only:** labels reach the page through Svelte's text binding, never HTML.
- **Nothing is derived** from names. No label means today's lower-case value.

## What doesn't change
- **The query:** the AST, and so the URL, copy and share, history, saved searches and the submitted query, keep the value. Only the chip's displayed parts change. A test checks that the AST is unchanged and that `rowOf` still gives the raw value.
- **Other tags:** only `group` values are looked up. A codec or language with the same spelling keeps its own text.

## A fresh load from a URL
- The chips render from the URL's query before `/facets` answers, so they show the value first.
- The server always lists a facet's selected values, so the first response fills the map and the chip re-renders once with the label. Nothing extra is fetched.
- **Nothing shifts:** the label is a mono part of the value's own length, which the S1-20 check guarantees and a test asserts.
- **With no rail response** (the group facet not loaded), the value shows, as before.

## Tests
- `query/group-labels.svelte.test.ts`:
  - the S1-20 cases: upper case over the value's lower case only, other spellings, lengths and characters, and non-ASCII letters;
  - a failing label never enters the map;
  - the cap evicts the oldest entry;
  - a response's listed values move to the newest end, so it never evicts them, through both a same-label re-listing and a relabel;
  - a value listed with no label, or with a failing one, loses its label;
  - the chip with a label known and unknown, the AST and `rowOf` unchanged, and other tags untouched;
  - **a chip held in `$derived` re-renders** with the label once a response brings it.
- **Mutation checks** (#1046 review): a plain `Map` in place of `$state.raw` fails the re-render test, and dropping the move to the newest end fails the eviction and latest-response tests.
- `facets/facets.test.ts`: a response's passing labels are kept, a failing one is dropped, and the labels last past a later response without the group facet.
- **No e2e assertion:** no existing spec shows a labelled group facet. The OpenAPI mock sends no group labels, and the facet spec runs only against the local fixture backend.

## Sizes
Measured with `npm run size:ci`, 6d8c26c0 → this branch.

| Entry | Before | After | Limit |
|---|---|---|---|
| Facet rail (lazy) | 18,843 B | 18,856 B | 18.85 → 18.86 kB (runner, §2.26) |
| Search cards (lazy) | 7,998 B | 8,000 B | 8.01 kB, unchanged |
| Search results (lazy) | 40,821 B | 40,822 B | 40.84 kB, unchanged |

- **The rail pays for recording the labels:** the import and one call in `facets.svelte.ts`, which sits in its chunk.
- **The check and the map** are in a shared chunk that no entry measures on its own.
