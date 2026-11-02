# G-import-smoke: runner import smoke stages at 10k and 100k, with the full capacity list
**Model:** Opus. This is a measurement and report gate, and the numbers feed owner decisions. A fresh Opus reviewer follows. Runner brief, 2026-11-02, for build-plan Track I step I1.

## Why
Before the owner's 1M and 10M pilots on the import host, the runner proves the offline import ladder end to end at 10k and 100k, in the cloud. It measures every item on build-plan §0.6's capacity list, so the pilots start from a working procedure and known baselines, not estimates.

## Read first
- `docs/build-plan.md` §0.6 ("Import scale") and §1.6 budgets. `docs/architecture.md` §3 (about 2.3 KB per torrent all-in) and §7.
- `docs/decisions/2026-10-31-IMP-4b.md` (import speed: 2,000/s offline, measured end to end as write time plus index drain), and the IMP-0..5 decisions files.
- `docs/operations/imports/` (the IMP-0 runbook and checker) and `lodestar import --help`.
- The importers: Magnetico SQLite (IMP-2) and Bitmagnet through PostgreSQL (IMP-3), in `crates/sources-importers`.

## Do
1. **Source data:** generate a **synthetic Magnetico SQLite** source at 10k and 100k.
   - Use realistic name, file-count and size distributions, deterministic from a seed.
   - The generator script goes under `docs/operations/imports/pilots/smoke/` (Python or a Rust example; say which).
   - Never use real torrent names or hashes from any external dataset.
   - If PostgreSQL 16 is installable in the sandbox, also run one 10k Bitmagnet restore through the IMP-0 checker. If it isn't, say so; it's optional.
2. **Run the ladder offline** with the server stopped, on a fresh data directory, for each stage:
   - `lodestar backup` first;
   - then `lodestar import`;
   - then start the server and let the index drain.
3. **Measure and report the full §0.6 list:**
   - import speed, as write time plus index drain against the 2,000/s line;
   - bytes per record: SQLite, indexes, provenance and search index, each separately, against about 2.3 KB all-in;
   - source copy size;
   - backup size and time, and restore time;
   - the have-filter size;
   - `filter_hits` and review-queue growth;
   - cold start;
   - RSS at idle and under a short query load;
   - search p95, including a cold file search;
   - Insights overhead;
   - the `relevance` fixture as a regression check only;
   - removal by job provenance: remove the 10k job's records and check they're gone, with no collateral damage.

   Note the CPU model and the sandbox limits next to every number.
4. **The report** goes in `docs/operations/imports/pilots/smoke/README.md`:
   - a table per stage;
   - what scales linearly, and the projections to 1M and 10M, marked as projections;
   - anything over budget, with what drives it.

   Raw logs stay out of the repo. Commit only aggregates.
5. **Findings:** a bug found on the way gets a minimal fix only if it's inside the import CLI path and small. Otherwise report it to the runner with a repro; don't widen the PR.

## Own
`docs/operations/imports/pilots/smoke/**`. Code fixes need the runner's OK first.

**Don't edit:** SRC-14a's files (`crates/api/src/ops.rs`, `crates/pipeline/**`, `crates/sources/src/http.rs`, `crates/lodestar/src/{ops,metrics,main,services}.rs`, `crates/store/src/sources.rs`), SRC-2b's (`crates/lodestar/src/percolator.rs`, `crates/store/src/saved.rs`), or `crates/sources-plugins/**` (SRC-7).

## Disk and time
The sandbox disk is limited. Clean the data directories between stages, and keep one build target. If 100k doesn't fit, report 10k plus the largest stage that does, and say why.

## Done when
The report is complete and reproducible: commands, seed and versions are listed. If code changed, fmt, clippy and tests pass.

Branch `slice/G-import-smoke`. Open the PR titled "G-import-smoke: offline import smoke at 10k and 100k". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, head and the headline numbers, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
