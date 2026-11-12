# X-dht-rejoin-nits decisions (#768)

Follow-ups from the fresh review of #768 (runner-state `reviews/rv-768-2a1f4392.md`, O-1 and O-3). The change is in `crates/dht/src/node/actor.rs` only. It adds to `2026-11-12-X-dht-bep42-rejoin.md` and doesn't replace it.

- **A re-bootstrap now clears pending rejoins (O-1).** `start_bootstrap` starts a self-lookup for the current `ids[0]` in every family, which is the lookup a pending rejoin waits to start. So it now clears every `rejoin` flag, and the rejoin isn't started a second time.
  - **The case.** A rejoin waits on a self-lookup for an old id. That lookup reaches its `LOOKUP_DEADLINE` in `scrape_tick`, which `maintain` runs after `rejoin_tick`. In the same tick the table is thin, no lookup is left and a re-bootstrap is due. `start_bootstrap` then looks up the new id, and on the next tick `rejoin_tick` found no stale lookup and started a second self-lookup for the same id.
  - **Cost before the fix:** one extra, bounded self-lookup. It's rare: at the default 1 s tick it needs a re-bootstrap due (30 s) just as a stale lookup's 25 s deadline ends.
  - **Why clear it in `start_bootstrap`** rather than skip in `rejoin_tick` when a current-id self-lookup is out: the flag means "a self-lookup for the new id is still to start", and `start_bootstrap` is exactly that start. Both of its callers (the start, and the re-bootstrap with no maintenance lookup out) have room under `MAX_LOOKUPS`, so the cleared flag never loses a lookup. A `debug_assert!` now checks that.
- **Correction to #768's self-review** (its last commit message). It dropped a "self-lookup already targets the new id" branch as unreachable, because a re-bootstrap runs after `rejoin_tick` and only with no maintenance lookups out. That missed a stale lookup ending later in the same tick. The case is reachable, and this fix covers it at the source.
- **O-3:** as the runner asked, this file's heading carries #768's number, the PR it follows up. #768's own file stays unedited.
- **Not in scope:** a minimum interval between id changes (O-2) is a runner follow-up.
- **No behaviour change without a pending rejoin.** The flags are all false except between an id change and its self-lookup. The crawler-sim goldens are unchanged.
