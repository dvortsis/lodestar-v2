# ENR-10: General enrichment harness (Opus, core: untrusted input, /security-review)

**Why:** every v0.7 enrichment plugin waits on this: ENR-11, ENR-12, ENR-14, then tmdb, tvdb, anilist and the rest. The plan estimates ~1,100 lines; one execution path, reviewed whole.

## Spec, read first
1. **`docs/plan/2026-11-05-v07-m7.md`:** §4's ENR-10 row and its surrounding text; §6 Q5 and Q12, which hold the owner's decisions (§2.46/§2.47: "the plan row is the decision"); and the "may land before the cut, inactive" rule (off by default, §18.7).
2. **`docs/architecture.md`:** §6, §18.1, §18.2, §18.7 and §20.9.
3. **`runner/slices.yaml` ENR-10's notes:**
   - reuse SRC-5's egress and `source_state`;
   - the file is named `identify.rs`, so it is never confused with ENR-1's `enrich.rs`;
   - `effective.rs`: resolved via the plugin;
   - `worker.rs`: reading v3 claims;
   - never edit `adult_enrich.rs` or `writer.rs`;
   - always write a non-empty details section (at least `match_confidence`).
4. **`docs/decisions/2026-11-07-runner-followups.md`:** a judged-bytes budget at ingest, before any plugin sends sections, plus an Items-phase budget test.
5. **ENR-9 and ENR-9b's decisions files:** claim encoding v3.

## What it does
- **Query** an enrichment source through a plugin trait.
- **Eligibility** decides which items may be looked up at all:
  - never `flags::PRIVACY`, held or Adult-off items;
  - Adult only through ENR-8's own path, which this slice never touches;
  - exactly what Q5 allows to be sent.
- **A cache.**
- **Interest first, then a capped backfill** (Q12).
- **Results are judged by the core before storage**, like any claim text: stricter only, and a held verdict is never stored.
- **Everything is off by default.** Nothing sends until a plugin is enabled.
- **Tests** (`crates/pipeline/tests/identify.rs`):
  - eligibility refusals: private, held, Adult-off, consent not given;
  - the budget;
  - cache hit and miss;
  - interest before backfill, and the backfill cap;
  - judging before storage;
  - restart resume;
  - no fixed sleeps; condition waits have deadlines of at least 10 s.
  - Fuzz or property tests for any parser of plugin output.

## Owns
- `crates/sources/src/identify.rs`
- `crates/pipeline/src/identify.rs`, `crates/pipeline/tests/identify.rs`
- `crates/classify/src/effective.rs`
- `crates/pipeline/src/worker.rs`. **Open PR #737 also edits this file.** Make your `worker.rs` change last, and bring `main` in after #737 merges. If you need it sooner, message the runner.
- its decisions file `docs/decisions/2026-11-12-ENR-10.md`

**Shared one-liners allowed:** the `mod` lines; `docs/metrics.md` rows for new metrics. Ask the runner before anything else.

## Checks
- `cargo fmt --check`
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`
- `cargo test -p lodestar-pipeline -p lodestar-sources -p lodestar-classify`, or the workspace if disk allows
- `/security-review` before the PR

Branch `slice/ENR-10`; PR title "ENR-10: General enrichment harness". Follow the steward skill. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) "ready <pr> <full sha>" when CI is green. A fresh Opus review with security items follows.

**This session runs in a slot lent overnight by the cloud tester (§2.60).** If the runner asks you to pause, commit and push your work in progress to your branch, then stop.
