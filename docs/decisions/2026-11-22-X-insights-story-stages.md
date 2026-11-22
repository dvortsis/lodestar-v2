# X-insights-story-stages decisions (PR 2 of 2)

Builds on `2026-11-22-X-insights-story.md` (PR 1, #1073) and covers finding 03-q and the stage-page items of I25 and P16. Calls marked (runner) are the runner's under §2.26.

- **KPI tiles share the row (I25).** The five stage pages have 3 to 5 tiles, and the overview's five fixed columns left one or two empty. The stage pages' row now fits as many tiles as the tile size allows (12 rem minimum) and lets them grow to fill the row, so three tiles fill the row at 1440 px and 1024 px and the row wraps on a phone. The overview keeps its five columns (§10.9.2). A Playwright test checks the last tile reaches the row's edge on every stage at both widths, and that no page scrolls sideways at 390 px.
- **Stage KPIs against the overview (03-q).** Each pair that docs/metrics.md says counts the same thing, checked by a unit test that builds each stage's response from the overview's own strip numbers and compares the displayed values, and against the server's sums (`crates/api/src/insights.rs` and `insights/stages.rs`, read only):

  | Pair                                                      | Result                                                                                                                                                                                        |
  | --------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | Discover `harvested` and strip Discover                   | agrees; both are first harvests by discovery cohort, so the Discover page now says "Counted by when each key was first harvested" (the strip says "By first harvest")                         |
  | Fetch `fetched` and strip Fetch                           | agrees (event time, null before fetches were counted by event time)                                                                                                                           |
  | Understand `stored` and strip Understand                  | agrees; both sum the stored metric over the range                                                                                                                                             |
  | Index `searchable` and strip Index and the Searchable KPI | agrees (one snapshot)                                                                                                                                                                         |
  | Serve `searches` and strip Serve                          | agrees by definition; the overview's strip is null until the search counters are read (metrics.md), so there is nothing to compare yet                                                        |
  | Funnel stage counts and the stage pages                   | differ by time base: the funnel counts a discovery cohort, the stage pages count events in the range. Each says which (the funnel subtitle, the KPI subtitle). They are not expected to match |

  No mismatch was found, so nothing was relabelled.

- **One reading is a sentence, not a plot (I25).** A chart with exactly one measured bucket now reads "Only one reading in this range. <label>: <value> <unit>." (with ≈ for approximate series), in the existing "Nothing was measured" style. No measured bucket keeps the existing sentence. The table and CSV for that chart are not offered: they would hold one row.
- **The "More measurements" list loses its box (P16).** Charts, bars and KPI tiles keep theirs.
- **Wording.** Checked against docs/ui-glossary.md: the stage pages already use torrent, searchable, harvested and the loss names the glossary gives, so no label changed. The one new sentence is the Discover time base above.
- **Not done here (X-charts):** axes, colours and histogram bins.
- **Size.** See the PR description: the stage-pages entry grows with this slice (the sentence, the row's rule, one time-base message), and the new "Insights overview (own JS + CSS)" entry from PR 1's notes is added by this PR, at its measured size rounded up to 0.01 kB.
