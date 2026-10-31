# Owner delegation decisions (2026-10-31)

On 2026-10-31 the owner told the runner to follow its own recommendations on performance and UX decisions, now and in future (`docs/owner-decisions.md` §2.26). These are the decisions the runner took under that rule the same day. Each is flagged to the owner, who can veto it (§2.19).

- **Alerts route size limit: 45.8 KB → 45.9 KB** (`web/.size-limit.mjs`, gzip). S-search-6 (#256) adds Cards and the live banner to `/search`. Their imports of the shared number, date and label helpers made the bundler split those helpers into their own chunks, and the Alerts page loaded them too: +61 B. The slice fixed that, so the Alerts page loads the same 44 files as on `main` again. 3 B remain, from five names re-exported in a chunk the Alerts page shares. The other ways to close the gap were dropping the adult subtype label from Cards tiles, which loses information users see, or trimming characters in unrelated code. The new limit is 0.2% higher. #256 sets it.
- **Crawler gate thresholds confirmed** (`docs/build-plan.md` §0.6):
  - a 24 h mean of at least 100 verified torrents per minute;
  - no hour below 70 per minute;
  - p95 discovery-to-searchable within 10 minutes.

  A strict rolling 60-minute minimum would fail on daily network cycles alone (the CR-0 soak saw hourly rates from 558 to 741). The 70/min floor still catches a crawler that stalls for an hour.
- **The first start is suppressed for 6 h, as on every start** (`docs/decisions/2026-10-30-master-plan.md`, Part F9). It's kept as product behaviour. A new install can't tell whether its IP address sampled the network before, and BEP 51 obligations belong to the address, not to the install, so suppression is the polite choice. Only BEP 51 sampling waits; the rest of the crawler runs. The first-run wizard and the user guide must say plainly that the first hours are slow, and when full speed starts.
- **Branch clean-up.** The 153 branches that descend from the old root `15620683` are deleted, along with `next` (merged into `main`) and `measure/windows-nrt`. The owner's backup of every ref (§2.25) keeps the old history recoverable. Ports of the paused slices are saved on `runner-state` under `ports/`. `release/v0.5.0` goes too: v0.5.0 was never tagged, and v0.5.1 is the first release.
