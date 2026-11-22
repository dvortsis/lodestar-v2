# CR-UTP-FIRST-WINDOW decisions (#1054)

#1008's probe found that about 1.25–1.8% of honest fetches over a 15%-loss uTP link miss the 4 s first-window deadline (`FIRST_WINDOW_DEADLINE`, `crates/peer-wire/src/fetch.rs`). This slice re-judges that deadline for lossy uTP links.

**Decision (the runner's, §2.26; flagged to the owner, §2.19): (a), keep 4 s.** `fetch.rs` is unchanged. Candidate (b) goes to the owner as a question (below), and (c) is advised against.

## What the first window bounds
- **When it applies:** a dict larger than one window (`FIRST_WINDOW`, 4 pieces of 16 KiB, 64 KiB). Once the first window's requests are out, the peer must deliver all of it within `first_window_deadline`.
- **What it caps:** how long a peer that won't deliver can hold a fetch slot, and the up-to-64 KiB reservation made for that window, before the fetch is cut. That covers a silent peer and a slow or hostile one.
- **After the window:** the peer must keep pace (`Pace`) until the fetch's own deadline.
- **A miss fails that peer, never the key** (`Error::FirstWindow`, a timeout). The fetch moves on to the key's next peer.

## Why uTP loss stretches the time to the first window
- **The window is about 55 packets of data from the peer** (64 KiB and four ut_metadata headers, at uTP's 1,200-byte `MSS`), in up to 4 bursts, plus our requests and ACKs, over one round trip.
- **Most losses inside a burst are repaired quickly:** three duplicate ACKs, or packets selectively acknowledged past the hole (`DUP_ACK_THRESHOLD`), trigger a fast retransmit within about a round trip.
- **A loss near a burst's tail has too few packets after it** to set that off. So does a lost ACK, or the resend itself being lost. These wait for the retransmit timer.
- **The timer:** RTO = max(srtt + 4·rttvar, 500 ms) (`MIN_RTO`; `INITIAL_RTO` is 1 s before the first sample), doubled on each consecutive timeout up to `MAX_RTO` (60 s), and reset by a fresh RTT sample (`crates/utp/src/ledbat.rs`).
- **The arithmetic:** on the loopback link (0–30 ms reordering) the RTO sits at its 500 ms floor. One tail loss costs about 0.5 s, and two in a row about 1.5 s (0.5 + 1). With about 55 packets and several losses each way at 15–20%, a fetch whose window meets three or four tail losses runs past 4 s, though it's making progress the whole time.

## Measured
Honest fetches of a 120 kB dict (8 pieces) over the in-process lossy link from `crates/peer-wire/tests/utp.rs`:
- loss in both directions, never two drops in a row;
- 0–30 ms reordering;
- release build, 16 fetches at a time.

Each run of 400 seeds was made with the product's 4 s window, then again with 20 s. Timing changes which packets are lost, so a seed doesn't replay its losses: compare the runs as totals. Every peer is honest by construction. "Misses" and "other failures" come from the 4 s run; the other failures of the 20 s run were also 0. Total time is the whole fetch (connect, handshakes, all 8 pieces), from the 20 s run.

| Loss | Fetches | First-window misses at 4 s | Other failures | Total time, 20 s window: p50 / p90 / p99 / max |
|---|---|---|---|---|
| 5% | 400 | 0 (0%) | 0 | 2.1 / 3.6 / 4.7 / 7.2 s |
| 10% | 400 | 0 (0%) | 0 | 2.7 / 4.6 / 6.1 / 8.9 s |
| 15% | 400 | 8 (2.0%) | 0 | 3.9 / 6.2 / 10.1 / 13.4 s |
| 20% | 400 | 32 (8.0%) | 0 | 6.0 / 9.9 / 13.0 / 19.2 s |

- **Every miss was an honest peer** (there are no others in the test). All 1,600 runs with the 20 s window succeeded.
- **15% matches #1008's probe** (1.25–1.8%).
- **To re-run it** (`first_window_miss_rates`, `#[ignore]`d as a measurement; about 32 min; `FIRST_WINDOW_FETCHES` sets the count):
  ```
  cargo test --release -p lodestar-peer-wire --test utp first_window_miss_rates -- --ignored --nocapture
  ```
- **The loss model's rate is now a parameter** (`Loss::at`). At 15% it draws exactly as before, so `fetches_over_a_lossy_link` and every other test run as they did.

## The candidates and their cost
- **(a) Keep 4 s (chosen).**
  - Cost: 2% of honest uTP fetches at 15% loss and 8% at 20% miss the window and move to the key's next peer. There are none at 10% loss or less, and ordinary paths lose far less than that.
  - The bound against slow, hostile and silent peers is unchanged.
- **(b) A transport-aware window.** uTP gets more time only while it's making progress.
  - It cuts the misses on bad links, and it keeps a silent peer's bound.
  - But it lets a slow-but-steady peer hold the slot longer on uTP: one that sends a window piece every couple of seconds earns the extension.
  - That loosens the bound against slow peers, so it's the owner's question (below).
- **(c) A longer deadline overall.** Advised against.
  - It loosens the bound for every peer on every transport, to fix a loss level only bad uTP paths reach.
  - It's the owner's by the brief.

## Owner question: a bounded uTP extension, (b)
- **The proposal:** on uTP only, one extension of 4 s, granted only if a piece of the first window arrived in the last 2 s before the 4 s mark.
- **What it changes:**
  - a slow peer's worst case on uTP goes from 4 s to 8 s;
  - a silent peer's stays 4 s;
  - TCP is unchanged.
- **The expected effect:** misses fall to fetches whose window takes more than 8 s, or that stall for 2 s right at the mark. That's rare below 20% loss in the table above, but it isn't measured with the extension in place.
- **The runner's recommendation:** not to build it unless live data show uTP paths at 15% loss or worse are common. The runner adds the owner_queue entry in its records PR.
