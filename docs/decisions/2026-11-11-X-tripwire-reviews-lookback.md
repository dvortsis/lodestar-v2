# X-tripwire-reviews-lookback decisions

This narrows a residual recorded in `docs/decisions/2026-11-11-X-op4-polish.md`, which now points here. The runner accepted it as a follow-up, 2026-11-11. It loosens no owner or security decision and changes no owner-set number.

- **The tripwire's per-PR reviews list now has the same lookback as the other lists.**
  - **Before:** for every PR active in the 130-minute window, the sweep read all its reviews from the REST endpoint. That endpoint lists oldest first and can't be sorted. So a PR with more than 1,000 old reviews hit the page limit and tripped every sweep while it stayed active, and an old bot review was reported again every hour.
  - **Now:** the sweep reads reviews through GraphQL, `reviews(last: 100, before: cursor)`. It starts from the newest page, pages backwards, and stops at the first page that reaches past the window. Only reviews submitted in the window are checked.
- **Failing closed is unchanged inside the window.** More than 1,000 reviews within the window on one PR still hits the 10-page limit and counts as a trip: an escalation, then `operator-wake.yml` disabled.
- **A review with no time, or a time without a zone, counts as recent,** so it fails closed. That applies to every list in the sweep: an unzoned timestamp used to raise on comparison and fail the run.
- **A GraphQL error fails like an API error.** That includes errors returned beside partial data: the sweep posts a best-effort escalation and fails the job, and never treats a partial answer as clean.
- **The query needs nothing new.** The job's `GITHUB_TOKEN` already reads pull requests, and `api.github.com/graphql` is the same host.
- **Tests:**
  - 1,500 old reviews on an updated PR don't trip, and the sweep reads one page;
  - 1,001 reviews inside the window trip it;
  - exactly 1,000 doesn't;
  - an old bot review is ignored, and a recent one trips;
  - undated or unzoned reviews count as recent;
  - errors with partial data fail the job.

  Every new guard was mutation-checked (7 mutations, all caught).
