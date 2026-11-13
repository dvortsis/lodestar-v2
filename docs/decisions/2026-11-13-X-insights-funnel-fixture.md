# X-insights-funnel-fixture decisions

The UI/UX review of 2026-11-13, finding B4, re-ranked Important (`docs/owner-decisions.md` §2.66 note): the overview's Fetch stage read 0 against the funnel's 413 on the fixture.

- **The fixture writes `FUNNEL_EVENTS` as a pipeline flush does.** It writes one row per (hour, stage, reason), summed over sources, with `dim` the stage and `key` the reason, from the same counts as its `cohort_hour` rows (`bench/src/fixture.rs`, `funnel_events`).
  - Each cohort's counts fall in its own hour. A real flush counts them in the hour it runs, which is later for slow fetches. Keeping them in one hour means a fixture window always shows the cohort and event views agree.
  - `bench/tests/fixture.rs` checks that every (hour, stage, reason) agrees, and that the manifest's new `rollups.fetched` and `rollups.fetched_24h` match the store.
  - `fixture::FORMAT` goes from 2 to 3, so `serve --fixture` rebuilds an old fixture rather than reuse it.
- **The overview's stage strip says its time base:** the selected range, by when each step happened. The exceptions are Discover, counted by first harvest (its cohorts), and Index, a count now.
  - It's a fixed sentence. The range's name lives in `Overview.svelte`, which this slice doesn't own, and the range picker sits just above.
- **The Fetch stage's loss list shows no rows when every loss count is 0.** Instead of ten rows of 0, it says "No keys were lost in this range.", or "No keys were fetched or lost in this range." when the Fetched count is also 0. Why: the list knows only about losses, so it says nothing was fetched only when the count says so.
  - A `null` list still reads "Not available yet", which is not the same as no losses.
- **Still open: a live-crawl check of the counter.** This slice fixes the fixture. That a real crawl's Fetch stage and funnel agree still needs a check against a live crawl. No code change is known to be needed: `lodestar_pipeline::rollups::funnel_events` writes both from one set of counts.
