# X-wk-fetched-lease-nits decisions: stale Fetched reports, and the third way a lease ends

Follow-up to X-wk-fetched-lease (#752), from its review rv-752. Brief: runner, 2026-11-12.

## Only a truly stale `Fetched` is counted
- **Before:** #752 counted no `Fetched` report whose key had no lease, whatever the reason. The server looked the lease up after `fetched` had released the authority's lock.
- **Now:** the authority answers `FetchedReport` under the one lock it marks with (`LeaseAuthority::fetched`):
  - **`Marked`:** the sender holds the key under that token, so the lease is marked;
  - **`Committed`:** the key has no lease, isn't waiting for a re-grant, and is among the recent commits (`RECENT_COMMITS`, the set `serve_remote` turns on). The commit beat the report, the usual order;
  - **`Stale`:** anything else, such as another token or holder, or a key this run doesn't know.
- **Only `Stale` counts** in `cluster_stale_reports_total`, and the `docs/metrics.md` row says so.
- **The limits:**
  - **The token:** the recent-commit set remembers keys, not tokens. A `Fetched` with a wrong token for a key committed lately counts as committed, not stale. That only changes a counter.
  - **Old commits:** a commit older than the set's memory makes a late `Fetched` count as stale, which only happens far past any normal lag.

## The third way a lease ends (a refinement of §19.3)
- **§19.3 names two ways:** the holder commits or reports a loss, or it goes silent past the TTL.
- **#752 added a third:** a lease marked `Fetched` whose commit hasn't come a TTL after the mark expires, its holder alive or not. Its key waits for a re-grant, as after any expiry.
- **Recorded in two places:**
  - the module docs (`crates/cluster/src/leases.rs`) now list it;
  - this file records it as a refinement of §19.3. The architecture text itself is the runner's to update.

## Tests
- `a_marked_lease_of_a_silent_holder_expires_once`: a lease that is both marked and held by a silent worker expires once, with nothing lost. Its mark goes, and nothing is left for a later expiry.
- `the_holders_claim_after_a_marked_lease_expires_is_granted_anew`: the holder's claim after the expiry is a re-grant under a new token, not a denial and not a second harvest.
- `a_fetched_report_is_marked_committed_or_stale`: the three answers.
- **`fetched_keeps_the_lease_and_results_need_a_sink`** (`crates/cluster/src/tests.rs`, unchanged): it still shows, over a real session, that a `Fetched` after the commit isn't counted. It now takes the `Committed` path.

## Not done: skipping hints for a marked lease
- **What the brief offered,** as optional: stop relaying another worker's peer hints to the holder of a marked lease.
- **Why not:**
  - the server would need a second authority lookup, under the lock, for every `Held` answer;
  - the holder already ignores hints for a key its pool is done with: they go in as a reply, which joins a key still waiting for peers and is ignored otherwise (`hints` and `granted` in `crates/lodestar/src/worker/runtime.rs`).
- **The cost of leaving it:** a few wasted hint messages while a commit is pending.
