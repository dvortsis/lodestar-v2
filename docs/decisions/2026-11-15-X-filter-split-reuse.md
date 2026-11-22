# X-filter-split-reuse decisions

A follow-up from #910's review (`2026-11-15-X-filter-budget-manyfile-perf.md`, "Memory").

## Calls

- **The earlier reading keeps one tokenizer per thread** (runner). Before, two places built a new `Tokenizer::splitting()` on every call, so a read that trades allocated `kept` (2,048 × 88 B, about 180 KB) and freed it again each time:
  - `read_split`, the matcher's earlier reading of a torrent's texts;
  - `Core::pattern_hits_split`, the core's.

  Both now use `normalize::with_splitting`, which works like `with_tokenizer` does for the joined reading: the thread's tokenizer, or a new one if it's already in use. `read_whole_by_v1` used a thread-local tokenizer of its own in `core.rs`, and it now shares the same one.

- **Its meter is stopped when the call returns, a panic included.** `pattern_hits_split` starts a meter (`CUT_ONLY`) and never stopped it, which didn't matter with a throwaway tokenizer. On a shared one, a meter left running would make the next unmetered reading (`read_whole_by_v1`) cut its texts. `with_splitting` runs the call under the same guard (`Unmeter`) that every metered read already uses.
- **Nothing a read is charged or produces changes.** A metered read is charged the same whatever earlier reads left in the tokenizer (`a_read_costs_the_same_whatever_came_before`). The charge digest pinned in `a_metered_read_reads_what_an_unmetered_one_does` is unchanged. No bound, charge or test expectation moves.

## Numbers

Only a text that holds a mark character takes the earlier reading. The 50 µs gate's typical torrents hold none, so the gate's path doesn't run this code.

On the build VM (an Intel Xeon at 2.10 GHz, noisy), raw core ms, `main` against this change, run back to back:

| Shape                                       | `main` | this change |
| ------------------------------------------- | ------ | ----------- |
| long folder with a mark                     | 2.31   | 2.14        |
| marks                                       | 1.57   | 1.62        |
| invisible                                   | 6.37   | 5.78        |
| worst of all shapes, on the reference scale | 10.08  | 9.72        |

## Tests

- `the_splitting_tokenizer_is_kept_and_left_unmetered` (`crates/filter/src/normalize.rs`):
  - a split read that trades, repeated three times, matches a read on a new tokenizer: the same work, tokens and ends;
  - the thread's `kept` table is reused, at the same address, rather than reallocated;
  - the tokenizer is unmetered after each read;
  - a caller's own meter doesn't reach the next caller, whose unmetered read stays whole.

  Without the guard, the test fails.
