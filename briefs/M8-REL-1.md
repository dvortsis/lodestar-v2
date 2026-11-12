# Brief: M8-REL-1, relevance scores an existing store and index

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Model:** Opus. **Branch:** `slice/M8-REL-1` from `origin/main`. **PR title:** `M8-REL-1: relevance scores an existing store and index`.

## Read first
- `docs/plan/2026-11-06-m8-hardening.md`: line 22 (today's suite), the M8-REL-1 row and its §-notes paragraph ("`--dir` already reads…"), OQ2 and RQ2.
- `docs/decisions/2026-11-06-P-m8.md` line 10 and `docs/decisions/2026-11-06-runner-planning-wave-answers.md` (OQ2 answered: a committed set plus a held-out set, aggregates only).
- `bench/src/bin/relevance.rs` (1,025 lines) and `docs/decisions/2026-10-22-R-relevance.md`.
- architecture §8 (search, ranking) and §17 (gates); build-plan §0.6; `runner/standards.md`.

## What to build
1. **`--data-dir PATH`:** score an *existing* Lodestar data directory (store + search index) instead of building one from `corpus.txt`.
   - **Open read-only.** Never write to the store or index, never run migrations, and never take the index writer. If a running server holds the data directory (the writer lock, or whatever the server uses to refuse a second instance), refuse with a clear message and a non-zero exit. Opening must not change any file in the directory: a test checks file sizes and mtimes before and after.
   - If the store's schema or the index's schema version isn't the one this build reads, refuse with a clear message. Don't upgrade it.
   - **Judgments keyed by info hash** (40 hex chars, v1; say in the format doc whether v2 is accepted). Keep the existing judgments grammar (query line, grades 0–3 and `x`), with the hash replacing the corpus-local id. Validate strictly: a malformed hash, a duplicate judgment or an unknown grade is an error with a line number. A judged hash that isn't in the store is counted and reported ("N judged torrents not in this store"), not an error.
   - Searches go through the same path as today (a searcher lease, the first page, default visibility: filter-hidden and Adult left out), so the scores mean the same thing.
   - `--gate` and `--write-baseline` with `--data-dir` work against a baseline in the judgments' folder, like `--dir`. The baseline records a fingerprint of the judgments. Don't fingerprint a 10M store's contents; record the store's torrent count and the index schema version, and say so.
2. **`--held-out`:** for running judged sets on the owner's pilot data on the import host.
   - Prints **only aggregates:** NDCG@10, MRR, recall@50, the query count, and the not-in-store count. **Never** a torrent name, a hash, a query's text or any per-query line. No `--worst` or `--all` output (refuse the combination), and error messages for a bad judgments line give the line number only, never the line's content.
   - Writes **no file**. Refuse `--write-baseline` with `--held-out` unless the baseline path is outside the repository checkout. The simplest safe rule is to refuse it outright; your call, record it.
   - A test captures stdout and stderr of a `--held-out` run on a fixture and asserts that no name, hash or query text from the fixture appears.
3. **The committed synthetic set's output stays byte-identical** (`relevance --dir bench/relevance` and `--gate`). Prove it in the PR: run before and after, diff empty. `bench/relevance/*` is not yours to change.
4. **Structure:** the file is already 1,025 lines. Split shared code into `bench/src/bin/relevance/` modules (for example `measure.rs`, `judgments.rs`, `data_dir.rs`) if that keeps it readable. Owned paths: `bench/src/bin/relevance.rs`, `bench/src/bin/relevance/**`, `bench/tests/relevance_sets.rs`.

## Tests (bench/tests/relevance_sets.rs and unit tests)
- Build a small data directory in a temp dir through the real store and index APIs, write hash-keyed judgments, and check the scores against values computed by hand for at least one query (NDCG, MRR, recall).
- Read-only: file sizes and mtimes unchanged; refused while the writer is held (hold it in the test, then release it); a wrong schema is refused.
- The judgments parser: property tests or a fuzz-style test over random lines (it parses a file the owner writes, but treat it as untrusted: no panics, bounded memory). Golden-error tests for each malformed case.
- `--held-out` leak test as above.
- The synthetic set still gates in CI's `relevance (§8)` job unchanged.
- No fixed sleeps; waits are on conditions with ≥ 10 s deadlines.

## Boundaries
- **Don't edit** `.github/workflows/*`, `bench/Cargo.toml`, `Cargo.lock`, `bench/relevance/*`, or any crate outside `bench/`. If you need a new public read-only open in `crates/store` or `crates/search` (for example a read-only store open), **ask the runner first** with the exact signature. Prefer what already exists.
- M8-PERF-1a (#780) is editing other `bench/src/**` files (`search-bench.rs`, `dataset.rs`, `synth.rs`, `lib.rs`). Don't touch them. If you need something from `bench/src/lib.rs`, tell the runner.
- No owner data and no real torrent names in fixtures; invent them.
- Decisions file: `docs/decisions/2026-11-12-M8-REL-1.md` for your calls (the hash format, the baseline fingerprint, the `--held-out` write rule). Name the refinements in the PR description.

## Size
About 450 lines in the plan. Keep it under ~1,500 changed lines, tests included.

## Done
fmt, clippy (`-D warnings`) and `cargo test -p lodestar-bench` pass; the synthetic set is byte-identical; follow `.claude/skills/steward/SKILL.md`; push once per round; then message the runner "ready <pr> <full 40-char sha>".
