# Review: PR #796 (X-soak-screen-calibrate), head 9bcfc50c

VERDICT: CHANGES

Checked: the screen core (byte-identical in both copies), thresholds and their arithmetic, the tests, owned paths (the extras match the runner-added list in the PR body), privacy. Both suites pass under `python3 -I` (docs/soak/tools: 208 OK; docs/soak/tools/linux: 124 OK). The thresholds file's SHA-256 matches the decisions file. The diff has no 12-digit numbers, model ids, hosts, IPs, local times or fixed sleeps. Every fixture is invented.

The logic is sound. A trip always wins. `ok` needs at least one judged `ok` and nothing `unusable`. The info counter is filtered out before any decision. Every memory input absent gives `unjudged`. Nothing usable gives `not_available`, with `unusable` named first. Overall reads `ok` only when every reached checkpoint is `ok`. I found no input that yields `ok` without a judged reading.

## Blocking
- **B1** `docs/decisions/2026-11-12-X-soak-screen-calibrate.md:76`: "So does the r3-era run, which has only the pool's key count" says the r3-era run expects `ok(without …)`. That contradicts D5 (line 29: "r3-era runs read `unjudged`") and the test (`test_screen.py::test_older_run_directories_still_analyze` asserts `unjudged`). The record the owner is flagged on misstates the behaviour. Fix: "The r3-era run, which has only the pool's key count, now expects `unjudged(…)` (D5)."

## Important
- **I1** `docs/decisions/2026-11-12-X-soak-screen-calibrate.md:57,59`: line 57 says 8 "trips r4's shape at hours 3, 6 and 12". But the early-fill lag the PR itself models (`screen_r5_early_fill`: RssAnon trailing committed by up to 15 MiB to hour 4) masks r4's leak at hour 3. I reproduced it locally on the r4 fixture: hour 3 reads `ok(without …)` at 0.84, hour 6 trips at 12.31 and hour 12 at 27.78. Line 59 says the fill "pushes this input down" but doesn't say this can hide a real leak at the first checkpoint. Fix: state it on line 57 ("hours 3, 6 and 12 without an early fill; with a fill lag like the fixture's, hour 3 can miss it and hours 6 and 12 still trip"). Ideally add a test pinning that r4 plus the fill still trips at hours 6 and 12.
- **I2** The PR description (not the repo) is stale at this head, and CLAUDE.md requires the decisions to be named there for the runner to flag:
  - "Not done" says r4's replay "is expected to read `ok(without …)`" and that "the runner decides" the all-memory-absent case. D5 now says `unjudged`.
  - It says "What it watches" (README) is stale and left alone, but this head rewrites it.
  - Fix: update the body to match D5 and the README change.

## Optional
- **O1** `docs/decisions/...-calibrate.md:43,45` and `screen-thresholds.json:64`: r4's "about 680 stored an hour" is cited to no committed aggregate. The listed sources give r4's 0.2 mappings per stored torrent (about 640 an hour over 1.9 h, or about 24.5 per 1,000). Cite where 680 comes from, or say it's assumed equal to r5's rate. The conclusion is unaffected.
- **O2** `test_analyze_cr0.py`, `test_each_absent_input_is_named_and_the_rest_still_decide`: r4's shape without RssAnon reads `ok(without …)`, with only mimalloc's 1,536 MiB level left as a memory input. Windows runs are always in this state. That is within D5 as accepted, but the README's `ok(without …)` bullet could add: "when `anon_outside_mimalloc…` is named, r4's failure mode wasn't watched."
