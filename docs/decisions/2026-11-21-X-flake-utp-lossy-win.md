# X-flake-utp-lossy-win decisions

`fetches_over_a_lossy_link` (`crates/peer-wire/tests/utp.rs`) failed on `test (windows-latest)` on #1003's head at 2026-11-21T06:45Z: "Require: the first window didn't arrive in time". #1003 doesn't touch peer-wire. The runner chose option (b) of three (runner, 2026-11-21).

## Root cause
- **The error is the product's first-window bound, not the test's deadlines.**
  - X-fetch-first-window (2026-11-14) added `FIRST_WINDOW_DEADLINE`, a fixed 4 s: a dict larger than one request window must get its first window (64 KiB) within 4 s of asking. Past that, the fetch fails with `Error::FirstWindow`.
  - The CI failure ended about 6.4 s into the Require run, which is a handshake plus the 4 s window.
- **Why #629's fix no longer covers the test.**
  - X-flake-utp-lossy (#629, 2026-11-08) made the test's loss model drop at most one packet in a row each way, at 15% long-run. It checked uTP's limits (SYN retries, `MAX_TIMEOUTS`) against the test's 10 s connect timeout and 30 s fetch timeout.
  - It came before the 4 s bound existed. The test's 120 KB dict is larger than one window, so its first 64 KiB (about 46 data packets) now has to cross the lossy link within 4 s.
  - A couple of retransmit timeouts in that window use it up: the RTO is at least 1 s, and it backs off.
- **What the 4 s defends against** (X-fetch-first-window): a peer that announces a large `metadata_size` holds only one window of the fetch pool's memory budget until it has sent that window. The bound stops a slow or silent peer from holding that unproven claim, and the room it keeps from other fetches, for longer than 4 s.

## The probe
A temporary, uncommitted probe ran the test's link (15% loss in each direction, at most one drop in a row, up to 30 ms of reordering delay) on an unloaded Linux machine. Each fetch was a 120 KB dict, in both `Require` and `Disable` modes.

| Run | Fetches | Missed the 4 s window | Fetch time p50 / p90 (p95) / p99 / max | First window p50 / p95 / p99 / max |
|---|---|---|---|---|
| Default bound, seeds 1000–1199 | 400 | 5 (1.25%), at 4.03–4.57 s, in both modes | 2.4 / 4.5 / 7.7 / 11.4 s | — |
| Bound lifted, timing only, seeds 1000–1249 and 2000–2249, `Require` | 500 | 7 (1.40%) would have missed | 2.5 / (6.0) / — / 11.7 s | 0.70 / 2.29 / 4.40 / 9.75 s |
| Same, `Disable` | 500 | 9 (1.80%) would have missed | 2.4 / (5.8) / — / 12.5 s | 0.72 / 2.84 / 4.34 / 7.78 s |

In the second and third runs, the bound was raised to 60 s and the time each fetch took to get its first window was logged, in a local build only.

- **No other error appeared** in any run.
- **The fixed CI seeds (40 and 41) don't reproduce the failure on Linux:**
  - before the change: 0 of 20 runs failed pinned to one core, and 0 of 20 with four busy threads on that core;
  - after it: 0 of 20 with the busy threads.
  - The loss is seeded, so only timing varies. A slow Windows runner pushes these seeds' windows past 4 s.

## Calls
- **`FetchOptions::first_window_deadline` (runner, option (b)).**
  - The bound is now a field, defaulting to exactly `FIRST_WINDOW_DEADLINE` (4 s). It has no config key, environment variable or setting: it exists in code only.
  - Every `FetchOptions` built outside `Default` uses `..Default`, so no production caller or path changes. The fetch pool's admission bounds, and `pool_budget_model.rs`, still assume 4 s.
- **The test gives itself 20 s.** That covers the probe's worst first window (9.8 s) with margin, inside the test's unchanged 30 s fetch timeout.
  - What it proves is unchanged: a large dict's fetch completes over a link that really loses and reorders packets (`resent > 0`), in both `Require` and `Disable`.
  - The four seeds that missed 4 s in the first probe run (5 fetches, both modes where they failed) all finish under the 20 s bound.
- **The 4 s default doesn't change here.** Changing it would loosen a hostile-peer bound on the fetch pool, which is a separate question with its own review.
  - **The runner has queued a fetch review of the 4 s value for uTP:** about 1.25–1.8% of honest fetches over a 15%-loss link miss it. In production they count in `fetch_first_window_timeouts_total{transport="utp"}`, as a failed peer, never a lost key.

## Tests
- `the_first_window_deadline_defaults_to_the_bound`: the default is 4 s and equals `FIRST_WINDOW_DEADLINE`.
- `the_first_window_deadline_is_the_options` (paused clock): a peer whose first window takes about 6 s fails with `FirstWindow` under the default, and finishes when the option gives it 10 s. A mutation that ignores the option and keeps the constant fails this test.
- Unchanged and still green:
  - `a_first_window_that_stops_ends_when_due` (the default bound, at 4 s);
  - `a_slow_steady_peer_has_its_first_window_in_time`;
  - `a_fetch_counts_its_first_window_timeout`.
