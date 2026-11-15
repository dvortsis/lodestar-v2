# X-filter-split-nits decisions (#953)

The optional findings from #917's review (`2026-11-15-X-filter-split-reuse.md`), as the runner listed them. No charge, bound, verdict or test expectation changes (architecture §20.8).

## Tests
- **`a_read_costs_the_same_whatever_came_before` runs over both readings' tokenizers:** the joined one and the earlier one (`Tokenizer::splitting`, which `with_splitting` now keeps per thread). Each runs on texts with marks and without.
  - A mark goes after every `a`, so the earlier reading ends a word where the joined one reads on.
  - The test checks that the two readings differ in all 48 marked cases. Otherwise the earlier tokenizer would only be read the way the joined one is, and its own state would go untested.
- **`a_cut_earlier_reading_leaves_the_next_one_whole`** (`crates/filter/src/core.rs`) runs end to end through the core:
  - `Core::pattern_hits_split` meters with `CUT_ONLY`, reaches the token cap and is held as too large;
  - then `read_whole_by_v1`, on the same thread, reads a text with marks. That text is within version 1's limit joined and past it apart, so it still isn't read whole.
  - A meter left running would read the text as nothing and pass it for whole. With `with_splitting`'s unmeter guard removed, the test fails at "after the cut".

## The other state kept per thread
- **Earlier reads never change what a read produces or is charged.** Besides the two tokenizers, the threads keep:
  - the core's `Scratch` and `ReadBufs` (`Core::pattern_signals`), cleared before each use;
  - the matcher's compiled queries (`matcher::compiled`), at most 4,096, each used only while the query at its address is still equal to the one compiled.
- **`ReadBufs` lets go of a buffer past 65,536 entries. The joined reading's `Scratch` doesn't.** A thread keeps the largest torrent's pattern buffers until it ends: one text at the token cap holds about 12 MB of tokens. This slice doesn't change that; it's noted for the runner.

## A `Scratch` per call in `pattern_hits_split`: kept
The review suggested keeping the earlier reading's `Scratch` per thread, as the joined reading's is. Profiled with `budget_worst_case` (release) on the build VM. Both binaries were built from this branch and run alternately, six times each, to take out drift. Raw core ms, medians:

| Shape | per call (as now) | per thread | per thread, trimmed |
|---|---|---|---|
| marks | 1.54 | 1.47 | 1.62 (against 1.58 in that run) |
| invisible | 5.92 | 5.35 | 6.46 (against 6.01) |
| long folder with a mark | 2.04 | 2.10 | 2.03 (against 1.98) |

- **Kept untrimmed, it's about 10% faster on invisible, but it keeps a second set of large buffers on every thread for good.** That's the retention noted above, doubled.
- **Trimmed as `ReadBufs` is, the gain is gone:** invisible was slower in all six runs.
- **The worst shape (about 10 ms on the reference scale) holds no mark, so neither version moves the budget's worst case.** The per-call `Scratch` stays.
