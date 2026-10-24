# S-discover decisions (Discover page)

`/discover` (§10.7) over `GET /discover` (B13). No owner decision is reversed or loosened; the refinements below are for the runner to flag (§2.19).

## What the page does

- **The URL is the state.** `/discover?type=<tab>&density=<d>`: an absent `type` is All, any other parameter is kept on every link, and a tab is a plain link (`<nav>` with `aria-current="page"`), so a tab change is a history entry. An unknown `type` is the server's `400 unknown_type`, shown as "That type doesn't exist" with a link to All; the client keeps no list of valid types beyond the tab names it draws.
- **The Adult tab follows the server.** `GET /settings/content` is read once on mount and the tab exists only for `fields.show_adult.value === true` (absent on any error, and until the answer). A `type=adult` URL with Adult hidden gets the server's empty Newest and its `adult_hidden` notice, shown as that notice with a link to `/settings/content`: not an empty state and not a count of 0. The client never filters rows.
- **Newest pages like search.** The first page asks for `type` alone; later pages send the same `type` with `cursor`. `cursor_expired` / `bad_cursor` restart from the first page with a one-line notice; any other paging error clears `hasMore` (so `VirtualList` doesn't retry in a loop) and shows a Banner with Retry above the list, keeping the rows. A failed _first_ page keeps nothing from another tab: rows of a previous tab are never left under a new tab's heading once the wait is visible.
- **Rising and Recently resolved render both paths.** `available` is a plain list of the same row; `unavailable` is a compact notice (icon, "Not available", the reason's sentence, and a link only for `search_disabled`). The reason text is an exhaustive `switch` over the generated `Unavailable` union, so a new reason fails type-checking until it has words. Today the server answers `needs_swarm_history` and `needs_resolution_history`.
- **"See all in Search"** links to `/search?q=type:<tab>&sort=-discovered` (`/search?sort=-discovered` for All), the query Newest runs, so the counts agree. There is no facet rail: `/discover` takes no query, so a rail couldn't filter it (brief step 1c).

## The interim row (`features/discover/HitRow.svelte`)

- **§10.4's Comfortable facts without badges or actions**; S-search-2 owns the shared result row and replaces this one (`// TODO(S-search-2)`).
- **Comfortable is three short lines in 64 px** (title; raw name in Mono with "resolved via files"; facts) because §10.4 puts the raw name under the title and the row height is fixed (§10.1). With no raw-name line the two remaining lines are centred. **Compact is one 36 px line** (title, size, seeders/leechers, discovered); it drops files, trackers and the composition bar, which §10.4 doesn't ask a single-line row to carry.
- **Facts order:** size, seeders/leechers, files, trackers reporting, composition, discovered. At narrow widths the line clips from the right (rows are fixed height; nothing wraps), so the facts that matter most come first. Every value is text in the row; icons are paired with screen-reader text.
- **Dominant share** is the server's largest `pct_bytes` among the groups it sent ("82% video"); the client sorts nothing and sums nothing. A row with no shares says "Not classified yet".

## Layout

- **Newest is the main region, Rising and Recently resolved sit beside it from `x-large` (two columns, 2fr / 1fr) and below it under that.** The tab strip scrolls horizontally inside its own box at narrow widths (no page scroll, no Select).
- **The list's height is the viewport minus the page chrome** (`100dvh` less a ramp step, at least six Comfortable rows), so `VirtualList` has a bounded scroller; the page itself scrolls past it to the side sections at narrow widths.
- **Loading** shows eight skeleton rows at the final row height with the section headings in place. A tab change keeps the previous rows until the answer arrives, and shows the skeleton only after 300 ms (§10.4 "Refining").

## The first-load reveal (§10.2)

- **Only the page's first data** reveals: the first eight rows, 80 ms apart (`--duration-stagger`). Tab changes, paging and retries never reveal.
- **The row's own opacity never changes.** A cover in the panel's colour fades off each row (plus a 4 px rise on the row). Rows are in the DOM, readable and focusable from the start, and contrast checks still see the real text colour. A plain opacity fade from 0 made axe report contrast failures mid-animation in the smoke tests that scan right after load.
- **Reduced motion:** the animation rules sit inside `@media (prefers-reduced-motion: no-preference)` and the page doesn't mark rows at all, so nothing animates.
- **CSS keyframes in the component's `<style>`**, not `svelte/transition` (runtime extras land in the shared chunk).

## Keyboard

- `list.next` (`j`) and `list.previous` (`k`) register in the `discover` scope from `onMount` (not `$effect`: the registry bumps its own version state, which an effect would track and loop on), with `keys.pushScope('discover')` while mounted. From outside the list `j` focuses the current row; inside it moves the active row. Enter is `VirtualList`'s `onActivate`, which opens `/t/<hash>` only for a 40-digit lowercase hex hash.

## Messages and shared keys

- **Shared keys from S-detail:** content-type labels are `content_type_*`, the composition label is `dominant_share` with `groupLabel(groupOf(g))` from `features/detail/composition/groups.ts` (an unknown group is "other"). Only `discover_type_all` stays local ("All" isn't a content type). Glossary terms use the shared keys (`seeders`, `leechers`, `discovered`, `backfill`, `never_scraped`, `trackers_reporting`, `swarm_suspect`). Plural wording is one message per form (`discover_files_one` / `_other`), as the message README asks.
- **`≈` comes from the formatter** (`{ value, approx: true }`, metrics.md §1): approximate totals and DHT-estimated seeders/leechers (`seeders_approx`).

## Errors

- **A failed tab change clears the rows** (another tab's rows and total never sit under the new tab); a failed refresh of the same tab, or a failed page, keeps its rows with a Retry. Paging uses its own request slot, never runs while a first page is loading, and a cursor is used only for the tab it came from.
- **Torrent names are untrusted:** the title and raw name go through `showBidi`; the only links built from data are `/t/<hash>` for a 40-digit lowercase hex hash and "See all in Search" from the tab.

## Budgets

- **CSS stays at 19.83 KB** (main's figure): no Tailwind class that main's CSS lacks, route-only layout in the component `<style>`. Component class names carry a `dsc-` prefix because plain names (`tab`, `tabs`, `list`, `grid`, `total`) collide with daisyUI components and Tailwind utilities and silently grow the global sheet.
- **Route own JS + CSS: 42.3 KB** (the runner's entry in `.size-limit.mjs`, raised from the proposed 15 KB to the measured value plus headroom, capped at the §10.1 45 KB route cap; runner-approved, not an owner number). It is over 15 KB because the screen is a virtualised list: about 12 KB for `VirtualList`, the virtualiser and the Svelte runtime only lazy routes use, 11 KB for the page with both languages' messages, then the key registry, API client, formatters and primitives. Importing `CategoryBar`, the chart group data and `VirtualList` directly (not through their barrels) keeps uPlot and the table and tree out of the route.
- **`/search` first load:** 41.48 → 41.65 KB JS (the route manifest), CSS unchanged, Lighthouse passes.
