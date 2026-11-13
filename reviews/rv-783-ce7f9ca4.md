VERDICT: CHANGES (one cheap hygiene fix; everything else is mergeable)

Head ce7f9ca4. Checked: fmt --check clean; clippy +1.99.0 -p lodestar-bench --all-targets -D warnings clean; relevance_sets 6/6 and bin unit tests 19/19 pass. Committed set byte-identical: `--dir bench/relevance`, `--gate` and `--all` outputs from main's relevance.rs and from the PR's, built from the same tree, compare equal (cmp).

## Blocking

B1. bench/src/bin/relevance/judgments.rs:220-221. The hygiene grep fails: H1 and H2 hold 20-digit decimal runs (the `0011…99` and `99…00` stretches). They're test hex hashes, not account ids, but only the two allow-listed numbers pass. Fix: use constants with no 12-digit run, e.g. `"0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d"` and `"f0e1d2c3b4a5968778695a4b3c2d1e0ff0e1d2c3"`.

## Important

I1. docs/decisions/2026-11-12-M8-REL-1.md:39 says "`bench/README.md` isn't this slice's file, so the format is documented in the module docs", but the PR adds a bench/README.md section that the runner approved. Fix: "bench/README.md has a short section; the module docs hold the full format."

## Optional

O1. bench/src/bin/relevance.rs:704: a query that loses all its positive grades is skipped whole, so its `x` torrents that are still in the store are no longer checked for forbidden results. Keep the forbidden check for skipped queries, or count them in a separate "forbidden-only" bucket.

O2. bench/tests/relevance_sets.rs:245: directories are recorded by presence only, so a file created and then removed during a run goes unseen. On non-Windows, also record each directory's mtime (`#[cfg(not(windows))]`); that keeps the Windows reasoning.

O3. data_dir.rs:74-78: a run holds `.tantivy-writer.lock` and `index/.lock` shared, so a server started during a run fails with Locked (after 10 s on Windows). Correct; mention it in the bench/README read-only bullet.

## Checks against the brief

1. Read-only: yes. The store opens with `mode=ro&immutable=1` and the read-only flag, so no -wal/-shm is created. The index opens only if meta.json and .tantivy-meta.lock already exist in both folders, so `SearchIndex::open` creates nothing, and it takes no writer. Existing lock files are opened read-only and locked shared, and missing ones aren't created. The snapshot test (every path, plus each file's length and mtime) is unchanged after `--write-baseline` and `--gate`, and also after the missing-meta-lock refusal. A server that starts mid-run is caught by `check_unchanged` after the last store read.
2. Refusals: a newer store schema ("never migrates"), an index at another version (lists what it found), a held `index/.lock` or a held writer (refused, then succeeds once released), -wal/-shm of either size, a missing directory. All are tested and exit non-zero.
3. Held-out: aggregates only. `--worst`, `--all`, `--gate` and `--write-baseline` are refused (exit 2). Judgment errors are redacted, a failing query reports "judgments.txt:N: the query failed", and a panic hook replaces the message. The leak test covers names, words of names, hashes, queries and rationales, and confirms no baseline is written.
4. Metrics: the hand-computed NDCG, MRR and recall match. Judged torrents not in the store are removed before the ideal DCG and recall are computed. The clock is fixed (newest discovered_at + 1 day), and hashes are sorted before lookup. Tie order is the Lease's own, unchanged.
5. Bounds: judgments and baseline are capped at 16 MiB, only judged hashes are looked up, and each query reads one page of 50. The two full-table aggregates (count and max) are linear and done once.
6. Tests: they're real, with no sleeps or waits. The parser has 20,000 randomized cases plus golden errors.
7. Paths: bench/src/bin/relevance{.rs,/**}, bench/tests/relevance_sets.rs, the decisions file and bench/README.md (approved). Nothing shared is touched, and no Cargo or workflow edits.
8. Hygiene: no model ids or local times. No test-util or testing feature is added to `lodestar` (bench's existing tokio test-util is untouched). The 12-digit grep fails (B1).
