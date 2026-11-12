# Runner notes, 2026-11-12

Decisions the runner took while reviewing the v0.7 work, written down in one place. Corrections to other slices' files are here, not in those files, as the README asks. The one exception is the CI job-id scrub in item "Job ids" below, which is privacy hygiene and changes no decision.

## Usenet (UN-1, UN-3)

- **UN-1, L3 kept (runner, security).** A private find's `external_ref` is still never stored, usenet included. Some indexers' and Prowlarr's `guid` is the download link itself, apikey included, so plugin-side vetting must not be the only guard. UN-1 therefore stores usenet items for search with no send reference. UN-3 designs where the reference lives (an allowlisted store field, through SRC-3b or a migration) and asks the owner if that needs L3 changed.
- **UN-1, cap cost (runner, §2.26).** UN-1 measured each adding write at the 1M usenet cap: about 95–110 ms more per write, about 2.3 s of writer time per 4,096-result poll on a fast disk, and more on a NAS. Usenet writes stay off by default until X-unverified-cap-index lands. That slice adds the eviction index and a per-network count.

## Enrichment (ENR-14)

- **ENR-14, Wikimedia policy (runner, stricter reading, owner §2.28).** One request at a time, well under 5 per second, `maxlag=5`, back off on maxlag and on 429, and a descriptive User-Agent with nothing personal in it. Q12's 100k lookups a day is a ceiling, not a target. Lookups go interest-first. Open owner question: does a whole-index backfill through the API count as "bulk", for which Wikimedia points to the dumps?
- **ENR-14 does the host wiring** of IdentifySource and Wikidata, off by default (runner). ENR-10's decisions file assigned it to ENR-14.

## Folder watch (UP-1)

- **The watch setting lives in the app only (runner).** There is no `config.rs` key or env var yet, so Docker users can't set it from the environment. Follow-up: X-watch-env.
- **The watch re-validates live.** A path that was fine when saved is checked again each time the watch starts.
- **A path inside the data directory fails safe.** `Section::check` can't see the data directory, so it lets such a path through, but applying the setting disables the watch. UP-2 shows that reason next to the field.

## Corrections to `2026-11-12-X-dht-lookup-converge.md`

From the delta review of #756. The file itself is unchanged; where it disagrees, this section wins.

- **Line 31 is superseded.** It says: "With no routers configured, nothing parks. A saved table of K or more nodes doesn't park either. A saved table of fewer than K does, while routers are configured, until the bootstrap settles." Read it as: "With no routers configured, nothing parks. A saved table doesn't stop parking: its nodes seed the self-lookup, and parking ends once K of them have answered or the bootstrap settles, usually within a few round trips."
- **The worst-case sentence on line 33 is superseded.** It says: "In the worst case, a bootstrap that never answers, `PARK_DEADLINE` and `lookup_wait` run out together, and the key retries after the pool's backoff, as it did before parking." Read it as: "A bootstrap that never answers ends at `query_timeout` (4 s). The longest park is a self-lookup that runs to its 25 s deadline, which leaves a released request about 5 s of `lookup_wait`. `PARK_DEADLINE` (30 s) is a backstop. If it runs out, it does so together with `lookup_wait`, and the key retries after the pool's backoff, as it did before parking."

## Correction to `2026-11-12-D-outputs-guide.md`

- **The "not by exact text" phrase is misplaced.** The sentence beginning "So section 7.5 maps" ends "…which a bare `404` or `503` could, not by exact text." Read it as: "So section 7.5 maps by the words a message contains (credentials, `[429:TooManyRequests]`, `[503:ServiceUnavailable]`, timed out, `[404:NotFound]`, no results), not by exact text. The bracketed form can't be mistaken for a port or part of a key, which a bare `404` or `503` could." The meaning is that the chapter keys on the bracketed status forms the apps print, not on exact message text.

## Follow-up slices added

- **X-always-adult-detail-mask** (ENR-12): `always_adult` on `ops::Source`, so the detail route's "Identified as" mask works in production. Content safety, so it gets `/security-review`, and it must land before ENR-13 ships the drawer.
- **X-dht-bep42-rejoin:** one self-lookup when the BEP 42 node id changes mid-bootstrap (rv-756 N-3, pre-existing).
- **X-percolate-promoted-hold** (GS-0): a held list in Percolation for promoted clones of `sig:` searches during the backfill. Low priority; revisit with GS-2.
- **X-watch-env** (UP-1): the `LODESTAR_WATCH_DIR` env var and `config.rs` key.
- **X-unverified-cap-index** (UN-0): the eviction index and per-network count that usenet's default-on needs.
- Notes were also added to RX-1 (rv-755 items), GS-2, ENR-11 (size-report figures), UN-3, UP-2, ENR-13 and D-outputs-guide. SRC-G and ENR-G already carried theirs.

## Job ids

CI job ids in comments and decisions files are replaced with a short description of the job, since the ids mean nothing once the logs expire. No decision changes.
