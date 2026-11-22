# CR-UTP-FIRST-WINDOW decisions (#1054)

#1008's probe found that about 1.25–1.8% of honest fetches over a 15%-loss uTP link miss the 4 s first-window deadline (`FIRST_WINDOW_DEADLINE`, `crates/peer-wire/src/fetch.rs`). This slice re-judges that deadline for lossy uTP links.

**Decision (the runner's, §2.26; flagged to the owner, §2.19): (d). The 4 s deadline stays, and the first request asks for one more piece than the window.**
- No time bound changes, for any peer.
- (b) goes to the owner as a question (below), and (c) is advised against.

## What the first window bounds
- **When it applies:** a dict larger than one window (`FIRST_WINDOW`, 4 pieces of 16 KiB, 64 KiB).
- **What it bounds:** once the first request is out, the peer must deliver the window, pieces 0–3, within `first_window_deadline`. That caps how long a peer that won't deliver can hold a fetch slot and the reservation made before it has sent anything. It covers a silent peer and a slow or hostile one.
- **After the window:** the peer must keep pace (`Pace`) until the fetch's own deadline.
- **A miss fails that peer, never the key** (`Error::FirstWindow`, a timeout). The fetch moves on to the key's next peer.

## Why uTP loss stretches the time to the first window
- **The window is about 55 packets of data from the peer:** 64 KiB and four ut_metadata headers, at uTP's 1,200-byte `MSS`.
- **Most losses are repaired quickly.** Three duplicate ACKs, or packets selectively acknowledged past the hole (`DUP_ACK_THRESHOLD`), trigger a fast retransmit within about a round trip.
- **Losses near the tail wait for the timer.** A loss near the end of what the peer is sending has too few packets after it to trigger a fast retransmit. So does a lost ACK, or a lost resend.
- **The timer:** RTO = max(srtt + 4·rttvar, 500 ms) (`MIN_RTO`; `INITIAL_RTO` is 1 s before the first sample), doubled on each consecutive timeout up to `MAX_RTO` (60 s), and reset by a fresh RTT sample (`crates/utp/src/ledbat.rs`).
- **The arithmetic:**
  - on the loopback test link (0–30 ms reordering) the RTO sits at its 500 ms floor;
  - one tail loss costs about 0.5 s, and two in a row about 1.5 s;
  - a window that meets three or four of these runs past 4 s, while making progress the whole time.
- **The request pattern decides how often that happens.** The product always fetches with a reserve (`pool.rs` calls `fetch_reserving`), so it asks for only the first request until the window is in. The window's last piece is then the tail of everything the peer has to send.

## Measured
Honest fetches of a 120 kB dict (8 pieces) over the in-process lossy link from `crates/peer-wire/tests/utp.rs`. Loss is in both directions, never two drops in a row, with 0–30 ms reordering. Release build, 400 fetches per cell, 16 at a time.
- Every peer is honest by construction.
- Other failures were 0 in every cell.
- Timing changes which packets are lost, so a seed doesn't replay its losses: compare cells as totals.

**First-window misses at 4 s**

| Loss | Product today (window alone) | (d), the window and one more piece | #1008's pattern (all 8 pieces at once, not the product's) |
|---|---|---|---|
| 5% | 0 (0%) | 0 (0%) | 0 (0%) |
| 10% | 8 (2.0%) | DPLUS10 | 0 (0%) |
| 15% | 25 (6.25%) | DPLUS15 | 8 (2.0%) |
| 20% | 75 (18.75%) | DPLUS20 | 32 (8.0%) |

**With a 20 s window (the product's pattern today):** all 1,600 fetches succeeded. Whole-fetch time, p50 / p99 / max:

| Loss | p50 | p99 | max |
|---|---|---|---|
| 5% | 2.1 s | 5.0 s | 7.3 s |
| 10% | 3.3 s | 10.6 s | 13.4 s |
| 15% | 4.7 s | 11.0 s | 13.7 s |
| 20% | 7.4 s | 18.7 s | 29.1 s |

**To re-run it** (`first_window_miss_rates`, `#[ignore]`d as a measurement, about 32 min):
```
cargo test --release -p lodestar-peer-wire --test utp first_window_miss_rates -- --ignored --nocapture
```
- `FIRST_WINDOW_ONLY_4S=1` skips the 20 s runs.
- `FIRST_WINDOW_ALL_PIECES=1` uses #1008's pattern.
- `FIRST_WINDOW_FETCHES` sets the count.
- The loss model's rate is now a parameter (`Loss::at`). At 15% it draws exactly as before, so `fetches_over_a_lossy_link` and every other test run as they did.

## (d): the first request is the window and one more piece
- **The change** (`FIRST_REQUEST`, `fetch.rs`). For a dict larger than five pieces, the first request asks for pieces 0–4 and reserves 80 KiB up front, instead of pieces 0–3 and 64 KiB.
- **What stays as it was:**
  - the window is still pieces 0–3;
  - the 4 s deadline and `Error::FirstWindow` count only those four.
- **Why it helps:** piece 3's last packets are followed by piece 4's, so a loss at the window's tail gets duplicate and selective ACKs and is fast-retransmitted.
- **Past the window:**
  - once pieces 0–3 are in, the whole result is reserved and the rest is asked for, as before;
  - the pace starts then, without waiting for piece 4, which is already reserved;
  - a peer that sends the window but withholds piece 4 is held to the pace like any other (`the_window_is_still_its_four_pieces`).
  - **Changed while building:** the step-0 version waited for every requested piece before moving on, and that would have let such a peer skip every pace check until the fetch deadline.
- **What it costs:**
  - **The hostile-peer worst case** is 80 KiB pinned for 4 s, against 64 KiB, by a peer that announces a large dict and sends nothing.
  - **The global reserve budget is unchanged.** About 20% fewer first requests fit in it at once when it binds.
  - **Every time bound is unchanged** for every peer, slow, silent or hostile.
  - **What the reserve protects is unchanged:** every requested byte is still reserved before it's requested.
- **Smaller dicts:**
  - **four pieces or fewer:** unchanged;
  - **exactly five pieces:** now fits the first request, so it's reserved whole (80 KiB) and asked for in full. Today it reserves 64 KiB and asks for piece 4 once the window is in.
- **Its limit:** the gain depends on the remote peer's uTP stack fast-retransmitting on duplicate or selective ACKs, as libtorrent does. The test's fake peer uses ours. TCP has no first-window misses at these loss levels and gains nothing.
- **Tests:**
  - `the_first_request_is_the_window_and_one_more_piece` (6 and 8 pieces);
  - `a_five_piece_dict_is_reserved_and_asked_for_whole`;
  - `the_window_is_still_its_four_pieces`;
  - every existing first-window and pace test is unchanged;
  - `tests/fetch.rs`'s up-front reservation is 5 pieces.
  - Mutation-checked: dropping the extra piece fails all three new tests. Waiting for every requested piece fails `the_window_is_still_its_four_pieces` and two existing pace tests.

## The candidates and their cost
- **(a) Keep 4 s and today's request.**
  - 2% of honest uTP fetches miss at 10% loss, 6% at 15% and 19% at 20%; each moves on to the key's next peer.
  - Every bound is unchanged.
- **(d) The window and one more piece (chosen).** Misses as in the table. 16 KiB more pinned per waiting first request, and no time bound changes.
- **(b) A transport-aware window.** uTP gets more time only while it's making progress.
  - It keeps a silent peer's bound, but lets a slow-but-steady peer hold the slot longer on uTP.
  - That loosens the bound against slow peers, so it's the owner's question (below).
- **(c) A longer deadline overall.** Advised against: it loosens the bound for every peer on every transport, and it's the owner's by the brief.

## Owner question: a bounded uTP extension, (b), on top of (d)
- **The proposal:** on uTP only, one extension of 4 s, granted only if a piece of the first window arrived in the last 2 s before the 4 s mark.
- **What it changes:**
  - a slow peer's worst case on uTP goes from 4 s to 8 s;
  - a silent peer's stays 4 s;
  - TCP is unchanged.
- **The gain left for it after (d):** the misses in the (d) column above, at most DPLUS20PCT at 20% loss and DPLUS15PCT at 15%. It would remove those that need under 8 s. That isn't measured with the extension in place.
- **The runner's recommendation:** not to build it unless live data show uTP paths at 15% loss or worse are common. The runner adds the owner_queue entry in its records PR.
