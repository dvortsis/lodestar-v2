# X-gs-followups decisions (Related and grouped results follow-ups)

Spec: architecture §10.5 (detail sections) and §10.4 (results rows, table, cards); UI review 02 §5.1 (+N other versions, Show all). Follows the reviews of GS-4 (`docs/decisions/2026-11-24-GS-4.md`, #1116) and GS-5 (`docs/decisions/2026-11-24-GS-5.md`, #1108). Every item is a refinement the runner decides (§2.19, §2.26); nothing here changes an owner decision.

- **Parsed and Related share one `/siblings` request** (`features/detail/siblings.ts`, moved from `related/load.ts`). Sections asking for the same torrent join the request already in flight. The last 8 ready answers are kept for the detail cache's freshness window (`FRESH_MS`), and a settings change (the cache generation) or Retry asks again. A request nobody waits for any more is cancelled. What stays the same: an answer for another hash is a failure, a 404 means withheld, and a stale answer is dropped. Only ready answers are kept, so a failure is never served again. The collapse rows' expand keeps its own fetch: it lives in the results chunk, which never loads the detail sections.
- **The drawer hands Related its open function** through a Svelte context (`'lodestar.detail.open'`, set in `DetailHostBody`). So Related no longer imports `open.ts`, and neither /t's on-demand graph nor "Detail sections" carries it. On /t, where nothing provides the context, a row stays a plain link. The key is a plain string written in both files, because a shared module for it was its own 121 B chunk in the drawer.
- **Show all opens Related directly.** `openDetail` takes `section: 'related'` and leaves the wish in `section.svelte.ts` (a `$state.raw`, not a proxy). The drawer scrolls that section into view once the torrent has loaded, and then clears the wish. Any open without a section also clears it, so an old wish never scrolls a later torrent. It opens the group's best version, whose Related lists every other version.
- **Show all's count is `max(others, versions) + 1`, the best row included.** The server's `others` counts the versions matching the query; `/siblings` lists the work's versions. When `/siblings` says it cut the list short (`truncated`), the line reads "Show all versions" with no number, rather than a count that could be wrong.
- **A group's load is announced again for the same words.** The polite region is cleared and set on the next frame, so a second group with the same count ("3 other versions") is read too. A newer load cancels the pending frame.
- **The active row follows its torrent across a change of view.** List, Table and Cards count rows differently (an open group's rows are in the List only), so the panel looks the active torrent up by hash in the new rows. A version or a group's line that the new view doesn't show hands over to its group's row. The Table starts each mount at row 1 (`ResultsTable`, outside this slice), so the panel sets the carried row again once the Table has mounted.
- **Nits.**
  - The j/k test now opens a group whose list is cut short, so its Show all line is real (the fixture at `ResultsPanel.group.test.ts:247`).
  - j/k at either end stays on the last torrent instead of landing on a Show all or loading line.
  - A closed drawer drops a scroll wish for a torrent that never loaded, so a later Forward doesn't jump to Related.
  - The switch's duplicate `data-testid="group-versions"` is gone: the toolbar shows the switch twice, and no test used the id.
  - `GroupToggle` is now controlled. A press asks for the change and the switch moves only when the new value arrives, so a change that never lands leaves it as it was.
- **Size (runner §2.26), measured locally with `npm run size:ci`:**

  | Entry                 | Before → after    | Limit                                                    |
  | --------------------- | ----------------- | -------------------------------------------------------- |
  | Detail sections       | 47,864 → 47,433 B | lowered 47.87 → 47.44 KB, under GS-4's starting 47.49 KB |
  | Related section       | 5,352 → 5,604 B   | 5.4 → 5.61 KB                                            |
  | Detail drawer         | 28,044 → 28,256 B | unchanged at 28.31 KB                                    |
  | /t                    | 15,687 → 15,705 B | 15.69 → 15.71 KB                                         |
  | Search results (lazy) | 43,496 → 43,696 B | 43.51 → 43.70 KB                                         |
  | Search collapse rows  | 3,007 → 3,044 B   | 3.02 → 3.05 KB                                           |
  - Detail sections and Related move together. The shared loader is one chunk that Parsed loads too, but the size config counts it in Related's entry only, so part of Detail sections' −431 B is accounting, and the loader's future growth shows in Related. The two are 179 B smaller in all.
  - The drawer's +212 B is the context, the section wish and its scroll.
  - /t's +18 B is not /t code: the shared message barrel and chunk names moved (Settings +9 B, Welcome −2 B).
  - Search results' +200 B is the carried row and the re-announce (cancelled on unmount and by the panel's other announcements).
  - Collapse rows' +37 B is the count and its no-number wording.
