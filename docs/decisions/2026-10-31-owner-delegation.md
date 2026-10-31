# Owner delegation decisions (#260)

On 2026-10-31 the owner told the runner to follow its own recommendations on performance and UX decisions, now and in future (`docs/owner-decisions.md` §2.26). These are the decisions the runner took under that rule the same day. Each is flagged to the owner, who can veto it (§2.19).

- **Alert route (`/alerts/[id]`) size limit: 45.9 KB allowed as a fallback, not used** (`web/.size-limit.mjs`, gzip; not the 29.1 KB "Alerts route" entry). This supersedes the line in `2026-10-30-S-settings-reconcile.md` that a later increase to the Alert route goes back to the owner: under §2.26 the runner decides it.
  - S-search-6 (#256) adds Cards and the live banner to `/search`. Their imports of the shared number, date and label helpers made the bundler split those helpers into their own chunks, which the Alert page also loaded: +61 B, putting it over 45.8 KB.
  - The runner allowed 45.9 KB rather than drop the adult subtype label from Cards tiles, which would lose information users see, or trim characters in unrelated code.
  - The slice then fixed the cause: the tiles and the banner import the helpers through the results row, so the Alert page loads the same files as on `main`. The page measures 45,764 B, and the limit stays at 45.8 KB.
- **Crawler gate thresholds confirmed** (`docs/build-plan.md` §0.6):
  - a 24 h mean of at least 100 verified torrents per minute;
  - no hour below 70 per minute;
  - p95 discovery-to-searchable within 10 minutes.

  A strict rolling 60-minute minimum would fail on daily network cycles alone (the CR-0 soak checkpoint stored between 558 and 741 torrents per hour, hour to hour). The 70/min floor still catches a crawler that stalls for an hour.
- **The first start is suppressed for 6 h, as on every start** (`docs/decisions/2026-10-30-master-plan.md`, "CR-0d", which left the first start to the owner as a product choice). It's kept as product behaviour. A new install can't tell whether its IP address sampled the network before, and BEP 51 obligations belong to the address, not to the install, so suppression is the polite choice. Only BEP 51 sampling waits; the rest of the crawler runs. The first-run wizard and the user guide must say plainly that the first hours are slow, and when full speed starts.
- **Branch clean-up.** The owner deletes the 153 branches that descend from the old root `15620683`, along with `next` (merged into `main`) and `measure/windows-nrt`. The owner's backup of every ref (§2.25) keeps the old history recoverable. Ports of the paused slices are saved on `runner-state` under `ports/`. `release/v0.5.0` goes too: v0.5.0 was never tagged, and v0.5.1 is the first release.
