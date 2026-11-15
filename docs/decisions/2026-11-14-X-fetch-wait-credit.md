# X-fetch-wait-credit decisions

This builds on X-fetch-first-window (`2026-11-14-X-fetch-first-window.md`). Architecture §6 (hostile-peer limits) and §18.4 (the fetch pool). This file says only what the code now does. The numbers are the runner's calls under `docs/owner-decisions.md` §2.26.
- **Unchanged:** the 8 s fetch deadline, A's pace rule, the first-window bound, and every owner-set number and memory budget.

## Calls
- **A timeout after a late room isn't charged to the peer** (`pool.rs`). A large dict's fetch is *late* when both hold:
  - its room took half the fetch's timeout or more in all (4 s at the default 8 s; `LATE_ROOM`, `Reservation::late_after`), counting both its first window's wait and its growth's;
  - it then timed out from a peer that was still sending at the deadline.

  Such a timeout is the wait's, not the peer's:
  - the peer isn't charged;
  - the key is tried again, its peer last among its untried ones;
  - the retry's growth goes ahead of growths without that credit, behind any credited one already waiting, and never ahead of the front of the growth queue, which keeps its turn (X-fetch-first-window). The credit is used once, by that retry.
  - **Why:** a peer whose room came that late had too little of its deadline left, so an honest peer timed out and the key could be lost for want of room alone. In the seeded wire model (below), that lost honest keys.
  - **Why both steps count:** neither wait alone may reach the threshold while together they leave the peer too little time.
  - **Why half the timeout** (runner decision): the threshold follows `fetch.timeout_secs`, which has no upper bound, so it means the same share of a fetch's time at any setting.
- **The limit:** a key has four late timeouts (`LATE_WAITS_FREE`), apart from its reservation wait-outs (`RESERVE_WAITS_FREE`). The fifth is charged, so no key cycles for ever.
- **Falling behind is always the peer's** (`peer-wire`, `Error::Behind`). A pace failure now has its own error. On `/metrics` it is still a `timed_out` failure, and the pool always charges it, however late the room came.
- **At a paced fetch's deadline, a peer that had stopped ends as behind** (`Pace::at_deadline`). The exchange now ends itself at the deadline, so it can say how the peer was doing:
  - a peer that is behind, or sent nothing for the last second (half `PACE_FREE`, the same at any timeout: runner decision), or that we were still waiting to write to, ends as `Behind`;
  - a peer still sending ends as a timeout, which may be credited.
- **Counted** as `fetch_late_room_timeouts_total` (`docs/metrics.md`).
- **What's left:** a narrow case at the deadline can still earn a credit, capped at four per key and inside the 8 s bound.

## Evidence
The combined wire model (`crates/sources/tests/pool_wire_model.rs`) runs the real pool and the real fetch loop over in-memory pipes. Each run has scripted honest, slow, stalling, junk and oversize peers, a worker's 4 MiB budget and the 8 s deadline. Over 1,000 seeds:

| Version | Honest keys lost | Reservation wait-outs | Late credits | Credits to stalling peers |
|---|---|---|---|---|
| Before (X-fetch-first-window alone) | 36, over 32 seeds | 48 | 0 | 0 |
| Credit, no deadline rule | 0 | 67 | 320 | 71 |
| Credit, stopped = silent for one part | 1 | 56 | 219 | 2 |
| **Credit, stopped = silent for 1 s (this)** | **0** | **60** | **271** | **25** |
| Credit, stopped = silent for 2 s | 0 | 67 | 303 | 54 |

## Tests
- **Budget** (`pool.rs`, `mod budget`, paused clock):
  - a claim is late once its room took 4 s in all, counting both steps, and neither alone; falling behind cancels it;
  - at a 30 s timeout the threshold is 15 s;
  - a credited growth goes behind the front and earlier credited growths, and ahead of the rest; it doesn't displace a front its own network passed.
- **Pool** (`pool.rs`):
  - of two fetches whose room came late, the one still sending isn't charged and is counted as late, and the one that fell behind is charged;
  - a key's four late timeouts go uncharged, the fifth is charged, and the key gets no more attempts.
- **Peer wire** (`fetch.rs`, paused clock): at the deadline, these end as `Behind`:
  - a peer that keeps its checks but stopped sending;
  - a peer we're still waiting to write to.

  A peer still sending ends as a timeout. At a 30 s timeout, silence is still measured in seconds, not parts.
- **Wire model:**
  - a key that a late room lost before now arrives;
  - a peer that keeps half an even pace and then stops, with a late room, earns no credit and is charged;
  - one peer saying a dict is too large doesn't end its key (the size rule);
  - 300 seeds lose no honest key.
- **Each test fails without its part:** the deadline rule, the allowance's limit, the credit's place in the queue (removed, or ahead of the front), the timeout-derived threshold, the size rule, and with falling behind ignored.
