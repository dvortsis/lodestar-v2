# X-fetch-junk-backoff decisions

This builds on X-fetch-room-fix (`2026-11-14-X-fetch-room-fix.md`). Architecture §6 (hostile-peer limits) and §18.4 (the fetch pool). This file says only what the code now does. The numbers are the runner's calls under `docs/owner-decisions.md` §2.26. No owner-set number or memory budget moves.

## Calls
- **A network whose large dict failed its check grows nothing for a while** (`Reservation::mismatched`, `HOLD_BACK`). When a fetch whose claim had grown, a dict larger than one request window, ends with metadata that doesn't match its key, the claim's network (an IPv4 /24, an IPv6 /48, as the pool's other per-network rules count them) is held back from growing.
  - While it is held back, a growth from that network waits apart from the growth queue, as one from a network that already has a grown claim does. It holds up nothing else.
  - Small dicts from the network, first windows, and every other network are untouched.
  - Reason: a large dict's room is given to a network whose last large dict failed its check only after a pause, so the room goes to networks that deliver.
- **The back-off:** 10 minutes. Each mismatch within a day of the last doubles it, up to 2 hours (`HOLD_BACK_MAX`). A day without a mismatch starts it over (`HOLD_BACK_FORGIVEN`).
- **Only a grown claim's mismatch counts.** A small dict that fails its check, or a large one that ends any other way, holds nothing back.
- **Bounded:** at most 4,096 networks are remembered. Past that, the one whose last mismatch is oldest is forgotten. Under 100 bytes each with the map's own overhead, so under 400 KB at the bound.
- **No setting and no new metric.** `docs/metrics.md` is unchanged.

## Tests
- **Budget** (`pool.rs`, `mod budget`, paused clock): a network whose large dict failed its check is held back, waits apart while another network grows, and grows once the back-off ends; the back-off doubles to its cap and is forgiven after a day; an honest network is never held back (a small dict's failed check, a large dict's other endings, another network's mismatch); the networks held back are bounded.
- **Through the pool** (`pool.rs`, real TCP): a large dict that fails its check holds its network back, so that network's next large dict waits out its room while a small one from it still arrives.
- Each test fails without its part.
