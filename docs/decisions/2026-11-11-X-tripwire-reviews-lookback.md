# X-tripwire-reviews-lookback decisions (#671)

This narrows a residual recorded in `docs/decisions/2026-11-11-X-op4-polish.md`, whose line on reviews now points here. The runner accepted it as a follow-up, 2026-11-11. It loosens no owner or security decision and changes no owner-set number.

- **Each review now counts only if it was submitted in the lookback.**
  - **Before:** for every PR active in the 130-minute window, the sweep read the whole reviews list under the 10-page limit. A PR with more than 1,000 old reviews hit that limit and tripped every sweep while it stayed active. An old bot review was also reported again every hour.
  - **Now:** each review is kept only if its own `submitted_at` falls in the window.
- **Why it reads every page and doesn't stop at an old one:**
  - GitHub lists a PR's reviews in creation order, and that order can't be changed.
  - A review created pending keeps its early place in the list, but its `submitted_at` comes later.
  - So the bot could open a pending review, let older reviews pile up behind it, and submit it later. A scan that stopped at the first page reaching past the window would miss that review: it would fail open.
  - Reading every page doesn't depend on any ordering. A first version of this slice used GraphQL `reviews(last:, before:)` and stopped early; the self-review found the hole above, and it was replaced before the PR.
- **What still trips, so fails closed:** an escalation, then `operator-wake.yml` disabled, whenever any of these holds:
  - **more than 1,000 reviews on one PR inside the window.** This is the same 10-page limit as every other list, as the brief asks;
  - **5,000 or more reviews on one PR, from any time** (`REVIEW_MAX_PAGES` = 50). Reading the list in full has to stop somewhere, and this is far above any PR here;
  - **more than 200 pages of reviews across all the sweep's active PRs** (`REVIEW_PAGE_BUDGET`):
    - **Why:** the job's `GITHUB_TOKEN` has a per-hour request budget of about 1,000 requests per repository. Without a sweep-wide cap, an outsider on this public repo could fill about 20 PRs with old reviews and keep them active with a comment an hour. The reads would then use up the token, and the sweep would end in an API error. An API error disables nothing.
    - **With the cap,** the sweep trips first, so availability is the only thing such a spend can cost.
  - The escalation names the limit that was hit: 10, 50 or 200 pages.
- **A review with no time, or a time without a zone, counts as recent,** so it fails closed. That applies to every list in the sweep: an unzoned timestamp used to raise on comparison and fail the run, which disables nothing. A full page whose last item has no zone keeps the list paging.
- **Residual: an edit to an old bot review isn't seen.**
  - **The gap:** a REST review has `submitted_at` and no edit time, so editing the body of a bot review submitted before the window changes nothing the sweep reads.
  - **What the old code did:** it re-reported every bot review on an active PR each hour, so it would have caught such an edit as a side effect.
  - **Why the gap is narrow:** the bot's original review already tripped the sweep in the window it was submitted, which disabled `operator-wake.yml`. A dismissal is still caught, as a `review_dismissed` issue event with the bot as actor.
  - **Covering it would need** GraphQL `lastEditedAt` on the bot's reviews. That is a separate change, for the runner to decide.
- **Costs:** at most 50 REST calls for one PR and 200 across a sweep, only for PRs active in the window. Every other list is unchanged.
- **Tests:**
  - 1,500 old reviews on an updated PR don't trip, and every page is read;
  - a bot review created first and submitted in the window trips, even behind 300 old reviews;
  - a bot review behind 150 recent reviews trips;
  - the window's edge counts as inside;
  - an old bot review is ignored;
  - 1,001 reviews in the window trip, and 1,000 don't;
  - 5,000 reviews on one PR trip, and 4,999 don't;
  - many PRs past the 200-page budget trip, and 164 pages don't;
  - undated or unzoned times count as recent, in the reviews list and in the other lists.

  Every new guard was mutation-checked (18 mutations across both review rounds). The one survivor was a redundant check, and it was removed.
