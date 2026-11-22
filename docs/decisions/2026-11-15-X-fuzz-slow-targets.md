# X-fuzz-slow-targets decisions

From the X-test-vm bug hunt: the `regex_plan` and `cluster_filter_snapshot` fuzz targets ran at about 100–150 inputs/s, so CI's 60 s per target gave them thin coverage. This file says only what the code now does and what was measured. It also takes in X-proptest-cases-env, which is dropped (runner's call, 2026-11-15; see the end).

## How it was measured

- Both targets were built as CI builds them: `cargo +nightly fuzz build --target x86_64-unknown-linux-gnu`.
- They were profiled with `perf record -g` over 30 s of fuzzing, run directly with `ASAN_OPTIONS=detect_odr_violation=0`, as `cargo fuzz run` sets.
- Exec rates are libFuzzer's own `stat::average_exec_per_sec` over a 5-minute run (`-max_total_time=300`). The runs were on the same 4-core machine, one at a time, each starting from its own copy of the same corpus.

## `cluster_filter_snapshot`: the harness padded chunks byte by byte

- **Cause:** for the chunk-sequence case, the harness pads each chunk that isn't the last to `MAX_CHUNK` (512 KiB) with `Vec::resize`. That generic code is monomorphised in the harness crate, which is coverage-instrumented, so each byte of padding ran a loop iteration with `__sanitizer_cov_trace_const_cmp*` calls. The profile put 87% of the time in the harness body, and 74% in those calls.
- **Fix:** the padded chunk is built as `vec![0u8; MAX_CHUNK]` (zeroed memory, no loop) with the payload copied into its start.
  - The bytes are exactly those `to_vec` then `resize` gave, so what the decoder sees, every assertion and what an input can reach are unchanged.
  - Nothing else in the target changed.

| | Execs/s | Inputs in 5 min | Coverage (edges) | Features | Peak RSS |
|---|---|---|---|---|---|
| Before | 265 | 79,851 | 2,382 | 4,122 | 326 MB |
| After | 1,348 | 405,890 | 3,538 | 7,698 | 228 MB |

- **Result:** 5.1 times the inputs in the same window, and more coverage. Neither run found a crash.

## `regex_plan`: the cost is the product's, by design

- **Cause:** the profile puts the time in `lodestar_search::regex::parse`.
  - Its translation always case-folds (`CASE_INSENSITIVE`), and `regex_syntax`'s `case_fold_simple` over wide classes dominates, mostly the coverage callbacks on its comparisons.
  - The harness itself does nothing extra. Its three parses are its oracles: the parse itself, the same parse again (deterministic), and the parse of `verify_pattern()` (spelling out `(?i)` changes nothing).
- **No harness change.** Dropping a parse would remove an oracle, and leaving `regex_syntax` uninstrumented would hide the case-folding paths from the fuzzer.
- **Finding, reported to the runner and not changed here:** in a plain release build (outside the fuzzer), `parse` averages 0.55 ms over the 921-input corpus. Its slowest inputs take 10–12 ms each, all of them wide classes case-folded across Unicode: `\P{Any}`, `\S`, negated POSIX classes such as `[:^word:]`. `Bounds` already caps the folding weight per pattern and the target's one-second assertion holds, so this is a cost to know, not a fault.

| | Execs/s | Inputs in 5 min | Coverage (edges) | Features | Peak RSS |
|---|---|---|---|---|---|
| Unchanged | 131 | 39,431 | 4,943 | 22,765 | 641 MB |

No crash.

## X-proptest-cases-env, dropped

The bug hunt reported that the parser property suites ignore `PROPTEST_CASES`, because they hard-code `ProptestConfig::with_cases(N)`. That isn't so with proptest 1.11.0 (`Cargo.lock`): the `proptest!` macro passes each suite's config through `contextualize_config`, which applies `PROPTEST_CASES` on top.
- **Measured** on `main`'s bencode `roundtrip` suite: unset 0.07 s, `PROPTEST_CASES=2048` 0.08 s, `20480` 0.72 s, `100000` 3.1 s.
- **The one exception in those suites** is `filter/tests/props.rs`'s `the_property_reads_trails`. It builds its own fixed-seed runner on purpose, so its coverage counts stay steady.
- **A value that isn't a number** gets proptest's warning on stderr, and the suite's own count runs.

The runner took the docs-only option (2026-11-15): `fuzz/README.md` now says how to deep-run the property tests. No test code changes for it.
