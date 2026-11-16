# X-filter-scratch-trim decisions (#957)

Found in X-filter-split-nits (#953): the joined reading's per-thread `Scratch` in `Core::pattern_signals` was never trimmed, unlike `ReadBufs`. A thread kept the largest torrent's pattern buffers until it ended (architecture §20.8). No verdict, budget, `BUDGET` or token cap changes.

## The bound
- **After each use, a `Scratch` buffer past 65,536 entries is let go of.** That covers its tokens, flags and position table, and each remembered folder's tokens and flags. A buffer within the bound is kept, so typical torrents reuse theirs as before.
- **The bound is `ReadBufs`' own:** one constant, `KEEP`, now serves both trims.
  - Both hold the same torrent's reading: `ReadBufs` the whole read, `Scratch` the pattern pass over one text or folder at a time. So one bound lets go of both after the same large torrent, and keeps both for the same typical one.
  - A typical torrent's texts are a few dozen tokens each, far below it.
- **What it kept before:** the largest text within the budget, about 390,000 tokens of a one-letter word (400,000 is over). At 24 bytes a token, that's about 9 MB of tokens alone, plus its flags and position table. The test's 300,000-token name left 300,001 entries kept without the trim. Only a torrent judged within the budget uses `Scratch`; one past it is held as too large before the pattern pass.
- **The trim replaces a buffer with an empty one** rather than shrinking it to the bound. A torrent that needs more than 65,536 entries is rare, and the next one starts small, as `ReadBufs` does.

## Numbers
`budget` and `budget_worst_case` (`crates/filter/tests/props.rs`, release) on the build VM: an Intel Xeon at 2.10 GHz, noisy. Both binaries were built from this branch's base and head and run alternately, six times each, with nothing else running. Medians, with the range:

| Measure | before | after |
|---|---|---|
| Gate: core, µs per torrent | 10.15 (8.90–11.40) | 9.85 (9.70–11.00) |
| Gate: engine, 10,000 mixed rules, µs per torrent | 51.85 (50.10–54.60) | 51.50 (50.80–68.90) |
| typical Latin, core ms | 5.27 (5.00–5.57) | 5.04 (4.78–5.26) |
| typical CJK, core ms | 6.22 (6.16–6.32) | 6.10 (5.92–6.41) |
| dense ASCII, core ms | 3.29 (3.21–5.89) | 3.33 (3.22–6.27) |
| tiny files, core ms | 6.19 (6.06–6.50) | 6.27 (6.11–8.61) |
| ASCII words read anew (the worst shape), core ms | 9.80 (9.23–10.20) | 9.82 (9.51–11.52) |
| worst, on the reference scale, ms | 9.52 (6.54–13.40) | 9.94 (5.99–13.43) |

Every difference is within the runs' spread. The trim is a capacity check per buffer after each torrent, and it only reallocates for a torrent past the bound.

## Tests
- `core::tests::the_joined_readings_buffers_are_trimmed_after_a_long_text`:
  - a typical torrent's buffers are kept (non-empty, within the bound);
  - after judging a 300,000-token name within the budget, every buffer is back within the bound.

  Without the trim, it fails with 300,001 entries kept.
- The filter's parity, golden and budget tests pass unchanged.
