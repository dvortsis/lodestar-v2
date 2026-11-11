# Brief: PK-win-soak-ci (Opus, core, concurrency), runner 2026-11-11

**Slice:** "Windows merge/GC retry stress in CI: segment deletes while readers hold maps, on windows-latest" (`runner/slices.yaml`; spec architecture §11; plan `docs/plan/2026-11-06-m6-gap-audit.md` row 57 and 119; `docs/decisions/2026-11-06-P-m6-audit.md:22`). On the G-m6 path.
**Branch:** `slice/PK-win-soak-ci`. **PR title:** `PK-win-soak-ci: <title>`. Follow `.claude/skills/steward/SKILL.md`; the runner merges. Auto mode.
**Owns:** `crates/search/tests/windows_gc_stress.rs` (new), `.github/workflows/ci.yml` (one new job and its `ci-ok` needs/applicability entry only), `docs/decisions/2026-11-11-PK-win-soak-ci.md`. If wiring the job needs `.github/scripts/ci_changes.py`, ask the runner first.

## What to build
1. **The stress test** (`crates/search/tests/windows_gc_stress.rs`), through the public `SearchIndex` API on a real `MmapDirectory` (the production path, with `retry_dir.rs`'s delete retries): a writer that commits small batches with merges on, while several reader threads keep searchers (and so mmaps of old segments) open across commits and merges, then drop them. On Windows, deleting a mapped file fails until the map is gone; the test proves:
   - no commit, merge or GC returns an error or panics while readers hold maps;
   - deletes that failed are retried, and once all readers drop and one more commit runs, the index folder holds only live segment files plus meta (no leaked files), checked against the index's own segment list;
   - search results stay correct throughout (every committed doc findable once its reader reloads);
   - the mmap cache gauge (X-mmap-cache-prune) matches live files at the end.
   Bounded: a fixed amount of work (e.g. N commits) with a wall-clock budget of a few minutes on windows-latest; no fixed sleeps; every wait is a condition with a deadline of at least 10 s. Deterministic enough not to flake: no assertion on timing.
2. It runs on every OS (so Linux CI exercises the logic too), but the **CI job** that matters runs it on `windows-latest` in release or the test profile, `--test windows_gc_stress -- --test-threads=1` if needed. Add the job to `ci-ok`'s `needs` and applicability the way `mmap-retention` is (path filter on `crates/search/**` and the test), so it's required where it applies and reports on every PR.
3. **Decisions file:** what the stress covers and what it doesn't (hot columns are RAM arrays, not mmap, so only merge/GC applies, per the plan); job time measured on the PR's CI run.

## Before ready
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-search --test windows_gc_stress` locally; run it a few times to check it doesn't flake. Self-review with a concurrency eye. Reply to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with "ready", the PR number and full head SHA when CI is green, with the Windows job's duration. Never put model identifiers in commits or the PR.
