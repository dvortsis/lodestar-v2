# X-flake-supervise-restarts decisions

A test-only fix for `supervise::crashes_outside_the_window_never_need_attention`, which failed 5 times out of 5 in the tester's Windows lane under load (4 busy loops on the test VM; 20 out of 20 passed idle). The panic was "waited for eight restarts", with 6 or 7 restarts seen: the supervisor wasn't stuck, it ran out of time. This file says only what the code now does.

## Root cause

- **The test gave a run of restarts one wall-clock deadline.** It waited for 8 restarts within one `DEADLINE` (60 s). Each start costs a process spawn, an fsync of the pid record and, on Windows, a PowerShell identity check (`pidfile::identity`), plus the output readers' wait. On a starved host, that cost grows until 8 of them outlast 60 s while every restart still comes.
- **The supervisor isn't the cause.** It timestamps each crash when the run ends and counts crashes whose timestamps lie within `crash_window`. The window therefore measures wall-clock time between crashes, respawn and backoff included. In this test the window is 1 ms, so every crash is outside it however slow the host is, and the test can't pass for the wrong reason.
- **No product change.** The shipped numbers are 5 crashes in 10 minutes. A crash loop would only go unseen if each respawn took over 2 minutes, and then crashes that far apart aren't a crash loop the window is meant to catch.

## Fix

- **`wait_restarts`:** a test helper that waits one restart at a time, each with the full `DEADLINE`. A slow host only makes the test slower. A restart that never comes still fails within one deadline, naming which restart it was waiting for. It stops early at Needs attention, so a wrong park still fails at once.
- **Used by two tests:**
  - `crashes_outside_the_window_never_need_attention` waits for 8 restarts that way. It asserts the same as before (never Needs attention), plus `restarts >= 8` stated outright.
  - `crashes_back_off_then_need_attention_until_restarted` waits for its two runs of 5 crashes the same way. It had the same single deadline over a run of restarts. Its assertions are unchanged.
- **No fixed sleeps, skips or retries.** The 300 ms "nothing restarts it" check is an absence check and stays as it was.

## How it was checked

Windows wasn't reproduced locally: no Windows host is available here, and Linux spawns don't cost what Windows ones do. The fix doesn't depend on the platform: whatever one restart costs, each gets its own 60 s. In the tester's failing runs each restart took under 10 s (6 or 7 in 60 s), so the new wait covers that load with room to spare.

On Linux, the failure was reproduced at a smaller scale:
- The test ran pinned to one CPU (`taskset -c 0`), shared with 300 busy loops at the same priority.
- Both the old and the new test were built with `DEADLINE` set to 5 s. That build was local only and isn't committed: the committed deadline stays 60 s.
- Unloaded, the 8 restarts take 0.27 s. Under that load they take about 10 s, more than one 5 s deadline, while each restart takes about 1.5 s, well inside it. That's the tester's Windows case at a twelfth of the scale.

| Test | Runs | Passed | Failed | Failure seen |
|---|---|---|---|---|
| Before (one deadline for 8 restarts) | 10 | 0 | 10 | "waited for eight restarts; last saw Status { … restarts: 3 }" (4 in one run), as in the tester's report |
| After (one deadline per restart) | 10 | 10 | 0 | none; 11 to 14 s each |

Loads that couldn't be calibrated weren't counted as results:
- busy loops at nice 19;
- 2,000 busy loops;
- cgroup CPU quotas.

Under those, the test binary barely started before the run's outer limit. At the full 60 s, 24 nice-19 loops gave 5 out of 5 passes before the fix (about 70 s per run, the wait just under 60 s).

All 20 supervise tests pass with the fix (`cargo test -p lodestar-companions --test supervise`).
