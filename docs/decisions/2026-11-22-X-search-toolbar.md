# X-search-toolbar decisions

Slice `X-search-toolbar`, from the UI review of 2026-11-13 (`docs/review/2026-11-13-ui-ux/`: I2, L4, I15, P7, L3). The runner decided the points marked "runner" under `docs/owner-decisions.md` §2.26 and flagged them to the owner under §2.19.

- **One view switcher (runner, refines §10.2 "density control" and §10.4's toolbar line).** List, Compact, Table, Cards in one control replace Density (Comfortable, Compact, Cards) and List/Table, whose combinations didn't all mean something. "List" is §10.2's Comfortable: a moderate user reads "List" against "Table" more easily than "Comfortable" (it names what they see, not a spacing grade). The URL keeps `density=` and `view=`, so old links work: `view=table` wins over any density, then `density=cards`, then `density=compact`, else List. Every combination is tested (`toolbar.test.ts`). Writing a view clears both parameters and sets the one it needs.
- **The view control is a native `<select>` at every width.** It costs less than the segmented control with four options, and gives type-ahead and the platform picker on phones for free.
- **Phones (below 640 px): Select and Save sit in a "More" disclosure** (a native `popover`: Tab moves through it, Escape closes it and returns focus), replacing the ARIA menu pattern first asked for. Bits UI's menu would have put about 35 kB in the toolbar chunk. Save keeps its label (X-alert-noun renames that noun later). Filters shares the toolbar's row (the rail's host is placed over the results track's top-left, below x-large), and the count moves to a line under the row. The count stays visible, so the control row is one line and the count is still read once.
- **"Still growing" stays visible (runner).** It says the results are incomplete. Below 640 px it becomes one compact line, in CSS only: no icon, caption size, tight padding.
- **Raw name only, no Resolution (runner).** `SearchHit` has no `resolution` field and names aren't parsed client-side. The table gets a Raw name column, shown by default; a column choice saved before this change is kept as it was (Raw name starts hidden for it). Cards show the raw name beneath the title, reusing `.rr-raw`. The API field is a follow-up for the runner.
- **P7: one count line only.** Results expose no hidden count, so there is nothing to add to "N hidden · Show". The toolbar already has one count. The other figures come from `results/ResultsPanel.svelte:431` (the "No results" title, `searchable_torrents`, shown with the empty state) and `discover/DiscoverPage.svelte:82` (the compact `≈ 2.2K`, via `formatCompact`); the follow-up can reconcile them there.
- **L3.** At 2,048 px and up the toolbar's controls follow the count instead of the track's far edge (`toolbar.css`, CSS only).
- **CSS placement.** The rail placement and compact-banner rules live in `toolbar/toolbar.css`, not `results/results.css`: `results.css` is in the Results chunk, which has about 10 B of headroom. They load in the same `Promise.all` as the panel, and the pasted-link layout-shift test passes at 390 and 1366 px (CLS < 0.01).
- **Size (`npm run size:ci`, gzip).**

  | Entry | main | this branch | limit |
  | --- | --- | --- | --- |
  | Search route JS (initial) | 44.95 kB | 44.95 kB | 45 kB |
  | Search results (lazy) | 40.84 kB | 40.84 kB | 40.85 kB |
  | Results toolbar (lazy) | 4.28 kB | 3.89 kB | 4.6 kB |
  | Search cards (lazy) | 8.00 kB | 8.11 kB | 8.01 kB, raise to 8.11 kB approved by the runner (see the PR) |
  | Search table mode (on demand) | 5.92 kB | 6.05 kB | 6.5 kB |
