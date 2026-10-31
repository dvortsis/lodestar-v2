# X-flake-shutdown-percolator decisions, r2 (corrects #193)

This file supersedes two bullets of `2026-10-30-X-flake-shutdown-percolator.md`: the cause in "Measured on Linux", and "Open, and ungated". The runner asked for a measured Windows run, and this records it. The test change in #193 stands. No owner decision or number changes.

- **Measured on Windows.** The throwaway workflow on `measure/windows-nrt` (run 41024502148, head `53298818`, not merged) ran the percolator test 5 times with `--test-threads=1 --nocapture`, then `search-bench --torrents 200000 --gate` once, on `windows-latest` and `ubuntu-latest`:

  |                                                | windows-latest                      | ubuntu-latest                       |
  | ---------------------------------------------- | ----------------------------------- | ----------------------------------- |
  | Store-to-commit (5 runs)                       | 2.099, 2.086, 2.087, 2.075, 2.111 s | 2.000, 2.001, 1.984, 2.000, 2.001 s |
  | Commit-to-alert (5 runs)                       | 33.5, 30.2, 30.5, 31.8, 42.5 ms     | 26.7, 26.7, 28.6, 28.7, 31.6 ms     |
  | New torrent → searchable, NRT gate (10 rounds) | p50 2.14 s, max 2.19 s, PASS        | p50 2.02 s, max 2.02 s, PASS        |

  Every other class of the gate passed on both systems too.

- **The cause, corrected: contention in the full test run, not the commit.** Run on its own, a Windows commit costs about 0.1 s more than a Linux one, not 3.5 s. The 5.74 s on `land/L5` came from `cargo test --workspace`, where this test shares its binary with 426 others on a 4-core runner. The span still held the same 2 s commit interval, plus the CPU and disk time the test waited for. Timing from the commit (#193) still answers it: commit-to-alert is about 30 ms on both systems, far inside 5 s.
- **Windows "new torrent → searchable" is measured, not open.** It's 2.19 s at most against the owner's 5 s (§1.6.3). It's measured once here, not gated: the NRT gate in CI still runs only on Linux.
