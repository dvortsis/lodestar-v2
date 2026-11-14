# X-fetch-first-window decisions

This builds on X-fetch-room-fix (`2026-11-14-X-fetch-room-fix.md`). Architecture §6 (hostile-peer limits) and §18.4 (the fetch pool). This file says only what the code now does. The numbers are the runner's calls under `docs/owner-decisions.md` §2.26. No owner-set number or memory budget moves.

## Calls
- **An unproven claim's announced size doesn't count toward admission** (`pool.rs`, `Claim::proven`). A large dict's partial claim is proven once its first window is in, which is when it asks to grow. Until then, the room its announced size would need doesn't count in the largest need that a new first window is checked against. Its first window's bytes count as before, within its network's share.
  - **Why liveness still holds:** every admission keeps the partial claims' bytes plus the largest *proven* need beside the floor. So once grown claims and finished results drain, which the pace and the deadlines bound, the proven claim with the largest need can grow.
  - **Claims proving together:** a claim that proves with a need that doesn't fit waits at the front of the growth queue, and no new first window is let in while it does. Claims that prove at the same time with needs that can't fit together wait there too, bounded by their pace and their deadlines.
- **A large dict's first window must arrive within 4 s** (`FIRST_WINDOW_DEADLINE`, twice `PACE_FREE`). The window's four requests go out together, so a slow peer isn't held to a round trip a piece.
  - Past the bound the fetch ends with `Error::FirstWindow`, a timed-out failure of that peer; the key keeps its other peers.
  - A dict of one window or less is left to the fetch's deadline, as before.
- **Counted** as `fetch_first_window_timeouts_total{transport}` (`docs/metrics.md`), so the bound can be tuned from what honest peers do.

## Tests
- **Pool** (`pool.rs`, `mod budget`, paused clock): an unproven claim's size doesn't hold up another network's first window, and its need counts once it asks to grow, until it has grown. #886's admission and liveness tests pass unchanged.
- **Peer wire** (`fetch.rs`, paused clock): a large dict's first window that stops ends when due, as `FirstWindow`, which counts as a timeout; a slow but steady honest peer, its four pieces 900 ms apart, finishes; a first-window timeout is counted by transport and shows in the metrics.
- Each test fails without its part.
