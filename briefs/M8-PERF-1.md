# M8-PERF-1: 10M bench mode
**Model:** Opus. A fresh Opus reviewer follows.

Runner brief, 2026-11-12T18:51Z. Epic M8 (1.0 hardening), on the path to G-v1-10m. Dep ext:v051-soak-verdict holds. **Testing first (owner):** this is the measurement tool the 1.0 gate runs on, so its numbers must be honest.

## Read first
- `docs/plan/2026-11-06-m8-hardening.md`: rows ~23–24 ("10M performance", "10M size"), the slice row (~49), ~40 (no on-disk format), M8-PERF-2/3 (what builds on you), and G-v1-10m (~85).
- `docs/architecture.md` §3 (every number) and §17; `docs/owner-decisions.md` §1.6.
- `bench/README.md`, `bench/src/bin/search-bench.rs`, `bench/src/bin/size-report.rs`, and `docs/decisions/2026-11-05-X-search-files-4.md`, `-5.md` (how 10M was run by hand).
- M8-PERF-1's `notes` in `runner/slices.yaml`.

## Do
1. **A persisted 10M dataset** (`bench/src/dataset.rs`, `bench/src/synth.rs`): generated once into a folder the operator names, keyed by the generator's version and seed; a dataset whose key doesn't match is refused, with a message naming both. Never a user's data directory. Deterministic: the same version and seed give the same bytes (a test proves it at small scale).
2. **Rows at 10M** (`search-bench.rs`): every §3 latency row, plus the regex row and the Insights rows that don't exist yet. Cold and warm where §3 asks. Steady-state RSS, new-torrent-to-searchable, cold start, and low-resource mode.
3. **Sizes at 10M** (`size-report.rs`): every §3 size figure, with every 1.0 field populated.
4. **A JSON report** with the machine (cores, memory, disk kind), the dataset key, the git revision and every row against its §3 budget, so M8-PERF-3 can gate on it. Document it in `bench/README.md`.
5. **The 200k and 50k CI gates stay exactly as they are** (same numbers, same commands). A test or a CI dry run shows they're unchanged.
6. **You can't run 10M in CI.** Prove the mode at a small scale in tests (`bench/tests/dataset.rs`), and if you run a larger scale by hand, report the machine and say it isn't the gate.
7. **Decisions file** `docs/decisions/<date>-M8-PERF-1.md`: the dataset key, each new row's method, what "new-to-searchable" and "cold start" measure exactly, and any refinement flagged (§2.19).

## Owns
- `bench/src/bin/search-bench.rs`, `bench/src/bin/size-report.rs`, `bench/src/dataset.rs`, `bench/src/synth.rs`, `bench/src/lib.rs`, `bench/README.md`, `bench/tests/dataset.rs`, your decisions file. `bench/Cargo.toml` for a dependency already in `Cargo.lock` (ask first for a new one).
- **Ask the runner first** for anything outside `bench/` (for example a hook in `crates/search` to time new-to-searchable).
- **Don't touch:** `.github/workflows/**` (M8-PERF-3 adds the 10M workflow), `crates/**` without asking, `web/**`.

## Done when
- The dataset is refused on a key mismatch; generation is deterministic; every §3 row and size figure has a 10M mode; the JSON report is documented; the CI gates are unchanged.
- **Tests:** waits on conditions with deadlines of at least 10 s, never fixed sleeps.
- **Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` (stable is 1.100; #776 fixed the one new lint, so bring `main` in once if your first CI is red only on clippy), `cargo test --workspace` (per-package is fine on limited disk; say so).
- **Size:** about 1,100 lines (plan). If it heads past ~1,500, tell the runner and propose a split before pushing.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, never merge. Branch `slice/M8-PERF-1`; PR title `M8-PERF-1: 10M bench mode`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with "ready <pr> <full sha>" once CI is green, using the Claude Code Remote MCP `send_message` tool.
