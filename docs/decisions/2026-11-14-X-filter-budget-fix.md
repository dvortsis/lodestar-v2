# X-filter-budget-fix decisions (#PRNUM)

FILTER-13 and FILTER-14: the filter budget charges. The detail stays held under `docs/security/v1/README.md` §5.

## Calls
- **Each word or character is charged as new once per torrent, whatever the cache's size** (owner, 2026-11-14). The tokenizer's caches stay the same size.
  - When a word or character finds its cache slot held by another the same read has used, the two trade places. The one displaced is kept for the rest of that read in an overflow store, and the store is emptied when the next read starts.
  - The store is a hash map with std's randomly keyed hash, so no choice of text makes its look-ups slow. Each trade is charged a fixed amount, so a read always costs the same whatever earlier reads left behind.
  - At most 1,024 buffers are kept between reads; a larger read's are freed when the next starts.
  - Its size is bounded by charges already paid (runner): every entry was charged as new in the read, so a read within the budget keeps at most about `BUDGET / PLAN` words and `BUDGET / CHAR_PLAN` characters. Debug builds assert it. It is emptied when every read starts and again when it ends, a panic included.
- **The budget charges what it didn't before.**
  - A word new to the read is now charged for each of its tokens, as well as its bytes. New tokens cost more downstream than repeated ones, and that charge was missing.
  - Judging the signals is charged against the same budget as the read: each text the rules keep, and each rule's search for a pair, counted as the Adult reading does it. It is counted twice where the earlier reading is read too. A torrent whose read and judging together pass the budget is held as too large to judge (CS-0a). A CSX-5 Override entry still releases it, as it does for the other pattern rules. Without an override, it is held. A torrent whose read alone passes the budget is held before overrides, as before.
- **`BUDGET` is unchanged** (owner's number), and so is the 15 ms bound on the reference runner. Measured times supersede the "2–12 ms" in `2026-11-13-X-filter01-fix.md` and `docs/architecture.md` §20.4.
- **`Core::read_cost` and `Core::too_large`** include judging, which depends on the core's term lists. The free `read_cost` and `too_large` still give the read's part, with no core.
- **The "ten thousand files in any script" claim is corrected** (`docs/architecture.md` §20.4's "Read whole" bullet, now updated, and the test's doc). Paths built from a few repeated words, each file numbered, now fit 9,344 to 10,880 files depending on the script, against an estimated 11,000 to 13,000 before: each file's new number is now charged in full. The test now holds 9,000. A collection whose file names vary is judged whole up to a number of files that depends on the script, pinned by a test at a floor under each measured number; there, fits rose in every script (below).

## Numbers
Fits are the most files of about 60 bytes each, with names drawn from a 2,000-word vocabulary per script, that the core judges whole.

| Script | Before | After |
|---|---|---|
| Latin | 4,032 | 5,312 |
| Cyrillic | 4,928 | 5,888 |
| Arabic | 4,736 | 5,760 |
| Kana | 5,568 | 6,656 |
| CJK | 1,680 | 2,944 |
| Hangul | 1,152 | 2,432 |

Measured on the build VM: an Intel Xeon at 2.10 GHz with 4 vCPUs, a noisy host.

Worst-case core judge time at the budget, scaled to the reference runner by the calibration workload: 44.2 ms before (one run), 11.7 ms after (the slowest input's best of five runs on a noisy host). Every input measured is under 15 ms. The core's time for a typical torrent is unchanged within the host's noise.

## Tests
- `budget_worst_case` (`crates/filter/tests/props.rs`, ignored in debug, run by CI's `filter budget (§20.8)` job) adds generated inputs for the charges above. It fails on the code before this change and passes after. It now sizes each input with the core-aware `Core::read_cost`.
- `generated_inputs_reach_the_budget` (`crates/filter/tests/props.rs`, runs in CI with no clock) covers the same generated inputs in work units, plus a sixth that pins the overflow store's charge. Each must reach the budget, and be held as CS-0a, at no more than a cap set about a quarter above today's fit. Every one fitted past its cap before these charges. It also checks that a torrent the read alone fits is held when judging its signals passes the budget.
- `varied_collections_in_each_script_are_judged` (`crates/filter/tests/judged_whole.rs`) pins each script's fit with the deterministic budget meter, so no timing is involved.
- The CS-0a property in `every_reading_reaches_the_end_or_holds` uses `Core::too_large`.
