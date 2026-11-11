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
  - **more than 60 pages of reviews across all the sweep's active PRs** (`REVIEW_PAGE_BUDGET`):
    - **Why:** the repository's `GITHUB_TOKEN` requests share a budget of about 1,000 an hour with the other workflows (`fleet.yml`, `status-connector.yml`, `freshness.yml`, `release.yml`). Without a cap, an outsider on this public repo could fill a few PRs with old reviews and keep them active with a comment an hour. The reads would use up the token, and later sweeps, with their disable calls, would fail. An API error disables nothing.
    - **Which sweeps run (the runner's call, option a).** `operator-wake.yml`'s sweep job runs only once a session may exist, or after the wake job fails or is cancelled. The wake job writes `started` to its output just before it asks for the session, still after the ledger checks, because a lost or malformed reply can hide a session that was created. A refusal exits 0 and costs no sweep: the rate limit, a duplicate id, the $200 cap, a held lock, or an agent not yet applied. Tests check each refusal, and that `started` is written when the create call returns no ID, and when a session starts and then fails.
    - **The bound:** at most `wake.PER_HOUR` (6) sweeps an hour after started sessions, plus the hourly cron, plus one after each failed or cancelled wake job. One sweep makes at most 4 × 10 pages for its other lists, plus a post and a disable, so 42 requests, plus its review pages. For started sessions and the cron: 7 × (60 + 42) = 714, which leaves about 250 of the 1,000 for the other workflows. A test pins this.
    - **Residual, stated plainly:** sweeps after failed wake jobs aren't bounded by that count. A run of wake failures, each one a well-formed runner instruction that then fails, can add sweeps beyond 7 an hour. Each failure already escalates. If the token runs out, the hourly cron still sweeps once it recovers, and the 130-minute lookback covers the gap up to that point.
    - **Earlier versions of this file** assumed one sweep an hour (200 pages), then 7 bounded by `PER_HOUR` (90 pages). The runner's reviews corrected both.
    - **With the cap,** the sweep trips first, so availability is the only thing such a spend can cost.
  - **the sweep's own 6-minute deadline** (`SWEEP_DEADLINE_SECONDS` = 360). Both sweep jobs have `timeout-minutes: 10`, and a job cancelled for time can neither escalate nor disable. So once the deadline passes, the next request raises, and the unfinished run counts as a trip. The escalation and the disable still go out after the deadline. A test checks that the deadline stays at least 2 minutes under both jobs' timeouts.
  - **a page that isn't a list, or a PR whose number isn't digits.** Either one used to end a list as complete, or skip that PR's reviews; each now trips.
  - The escalation names what was hit: 10, 50 or 60 pages, the deadline, or in plain words the list with an unreadable page, or a PR without a readable number.
- **A review with no time, or a time without a zone, counts as recent,** so it fails closed. That applies to every list in the sweep: an unzoned timestamp used to raise on comparison and fail the run, which disables nothing. A full page whose last item has no zone keeps the list paging.
- **Residual, accepted by the runner (2026-11-11): an edit to an old bot review isn't seen.**
  - **The gap:** a REST review has `submitted_at` and no edit time, so editing the body of a bot review submitted before the window changes nothing the sweep reads.
  - **What the old code did:** it re-reported every bot review on an active PR each hour, so it would have caught such an edit as a side effect.
  - **Why the gap is narrow:** the bot's original review already tripped the sweep in the window it was submitted, which disabled `operator-wake.yml`. A dismissal is still caught, as a `review_dismissed` issue event with the bot as actor.
  - **Covering it would need** GraphQL `lastEditedAt` on the bot's reviews. The runner logged that as a possible follow-up.
- **Residual, accepted by the runner (2026-11-11): repeated API errors leave a gap.** Each sweep looks back 130 minutes. If sweeps keep failing for longer than that, for example while GitHub's API is down, what happened before the window isn't checked once they recover. Each failed sweep still escalates best-effort and fails its job.
- **Costs:** at most 50 REST calls for one PR and 60 across a sweep, only for PRs active in the window. Every other list is unchanged.
- **Each request's 30 s timeout limits each socket operation, not the whole request** (Python's `urlopen`). A response that trickles in can take longer than 30 s, so the sweep's own 6-minute deadline is the real bound on a run.
- **Tests:**
  - 1,500 old reviews on an updated PR don't trip, and every page is read;
  - a bot review created first and submitted in the window trips, even behind 300 old reviews;
  - a bot review behind 150 recent reviews trips;
  - the window's edge counts as inside;
  - an old bot review is ignored;
  - 1,001 reviews in the window trip, and 1,000 don't;
  - 5,000 reviews on one PR trip, and 4,999 don't;
  - exactly 60 review pages are complete, and one page more trips;
  - a sweep past its deadline trips, and the deadline stays under the job timeouts;
  - an unreadable page and an unreadable PR number trip;
  - undated or unzoned times count as recent, in the reviews list and in the other lists.

  Every new guard was mutation-checked (31 mutations across four review rounds). The one survivor was a redundant check, and it was removed.
