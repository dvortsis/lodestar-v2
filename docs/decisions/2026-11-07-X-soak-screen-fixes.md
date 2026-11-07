# X-soak-screen-fixes decisions

Follow-up slice, 2026-11-07, launched by the runner. It covers the soak screen's cost test, which failed on every run, and two items from X-soak-screen-polish. No production code changes: the Rust change is in `crates/lodestar/src/memstats.rs`'s test module, and the rest is in `docs/soak/tools/**`. Nothing here reverses or loosens an owner decision, or changes an owner-set number.

- **D1. The cost test spawns threads until `/proc/self/maps` holds 3,000 lines, instead of spawning a fixed 750.**
  - **Why it failed:** `memstats::tests::anon_mappings_cost_at_3000_mappings` assumed about four mappings per thread: its stack, guard page and thread-local mappings. On GitHub's Ubuntu runners each thread adds about two, so 750 threads gave 1,604–1,605 lines and failed the test's `≥ 2,500` check. That happened on every soak-screen run since X-soak-screen merged. The cost itself passed, at a median of 628 µs against 100 ms.
  - **The fix:** the test now spawns parked 64 KiB-stack threads in batches of 100 and waits until every thread in a batch runs, so its stack is mapped. It recounts the lines after each batch and stops at 3,000, or at a cap of 4,000 threads. It reports how many threads it took.
  - **What stays:** the `≥ 2,500` check and the 100 ms median limit are unchanged, so the test still measures the same thing.
  - **Locally:** 800 threads gave 3,305 lines, at a median of 891 µs per read.
  - **Why not mappings made directly:** the workspace forbids `unsafe`, and `rustix` isn't built with its `mm` feature. Mapping memory directly would need a dependency change, which this slice wasn't approved for.
- **D2. `screen_slope` squares by multiplying, and a spread too wide to square is no reading.** This changes both copies, `docs/soak/tools/linux/collect.py` and `docs/soak/tools/analyze_cr0.py`.
  - **Why:** `(x - mx) ** 2` on a float raises `OverflowError` past about 1e154. The collector caught that as a screen error and turned the whole screen off, and the analyzer reported `screen_error`.
  - **The fix:** with `(x - mx) * (x - mx)` the sum becomes `inf`, and `screen_slope` then returns NaN. The checkpoint's existing non-finite check marks that one counter `unusable`, and the rest of the screen carries on.
  - **Why NaN, not the `inf` slope:** a numerator that is still finite over an infinite `sxx` gives a slope of 0.0. That would read as `ok` on input the screen can't judge.
  - **Tests:** `test_screen.py::NeverAPassTests::test_a_stored_spread_too_wide_to_square_is_unusable_not_an_error` covers the analyzer, and `test_collect.py::ScreenTests::` the test of the same name covers the collector. Both raise `OverflowError` on the old code.
- **D3. `c.screen_results == []` is checked where it means something.**
  - **The problem:** in `test_a_screen_error_never_ends_the_run`, `screen_checkpoint` raises before any result is appended, so the check passed whatever the collector did.
  - **The fix:** it moves into `test_screen_json_that_cant_be_written_never_ends_the_run`. There the checkpoint is appended before `screen.json`'s write fails, so the check proves the collector clears it and `status.json` can't show a line `screen.json` doesn't hold.
