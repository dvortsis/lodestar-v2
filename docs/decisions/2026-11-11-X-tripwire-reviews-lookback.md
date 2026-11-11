# X-tripwire-reviews-lookback decisions (#671)

This narrows a residual recorded in `docs/decisions/2026-11-11-X-op4-polish.md`, which now points here. The runner accepted it as a follow-up, 2026-11-11. It loosens no owner or security decision and changes no owner-set number.

- **Each review now counts only if it was submitted in the lookback.**
  - **Before:** for every PR active in the 130-minute window, the sweep read the whole reviews list under the 10-page limit. A PR with more than 1,000 old reviews hit that limit and tripped every sweep while it stayed active. An old bot review was also reported again every hour.
  - **Now:** each review is kept only if its own `submitted_at` falls in the window.
- **A PR's reviews are still read in full, up to 50 pages (5,000 reviews).** Past that, the PR's own list counts as an incomplete sweep, so as a trip: an escalation, then `operator-wake.yml` disabled. Old reviews alone now trip only past 5,000 on one PR, not past 1,000.
- **Why it reads in full and doesn't stop at an old page:**
  - GitHub lists a PR's reviews in creation order, and that order can't be changed.
  - A review created pending keeps its early place in the list, but its `submitted_at` comes later.
  - So the bot could open a pending review, let older reviews pile up behind it, and submit it later. A scan that stopped at the first page reaching past the window would miss that review: it would fail open.
  - Reading the list in full doesn't depend on any ordering. A first version of this slice used GraphQL `reviews(last:, before:)` and stopped early; the self-review found the hole above, and it was replaced before the PR.
- **A review with no time, or a time without a zone, counts as recent,** so it fails closed. That applies to every list in the sweep: an unzoned timestamp used to raise on comparison and fail the run. A full page whose last item has no zone keeps the list paging.
- **Costs:** at most 50 REST calls for one PR, and only for a PR active in the window. Every other list is unchanged.
- **Tests:**
  - 1,500 old reviews on an updated PR don't trip, and every page is read;
  - a bot review created first and submitted in the window trips, even behind 300 old reviews;
  - a bot review on an earlier page trips;
  - the window's edge counts as inside;
  - an old bot review is ignored;
  - 5,000 reviews trip, and 4,999 don't;
  - undated or unzoned times count as recent, in the reviews list and in the other lists.

  Every new guard was mutation-checked (8 mutations, all caught).
