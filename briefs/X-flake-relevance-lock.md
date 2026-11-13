# X-flake-relevance-lock (Opus, test fix)

**Why.** On #824 (a CI-filter PR that touches no Rust), `test (windows-latest)` failed in
`bench/tests/relevance_sets.rs`: `a_skipped_query_still_checks_forbidden_results` and
`held_out_errors_name_only_the_line`, both with "Failed to acquire Lockfile ... os error 32"
on the tantivy index. os error 32 is the Windows sharing violation. The runner's reading,
which you must confirm or correct:
- the tests share one `fixture()` data directory (a `OnceLock`), and several run the
  `relevance` binary on it at the same time, as separate processes;
- opening the index takes tantivy's meta lock file. On Windows a second process opening
  that file while another holds it gets error 32, an I/O error that tantivy does not retry
  (it retries only a busy lock), so the run fails;
- Linux and macOS don't show it because their file locks don't refuse the open.

**Do.**
1. Reproduce or confirm the mechanism from the code (tantivy's lock path and the tool's
   read-only open in `bench/src/bin/relevance/`). Say in the PR which it was.
2. Fix the test so parallel tests can't race on the shared fixture: for example, serialise
   tool runs on the shared fixture, or give each such test its own copy (`copy_of` exists).
   Keep every test and every assertion. Don't skip, ignore or `cfg`-out any test, and don't
   add fixed sleeps. Any wait is a condition with a deadline of at least 10 s.
3. If the tool itself should tolerate a concurrent reader on Windows (for example, by
   retrying that error with a deadline), say so in the PR and propose it, but keep this PR to
   the test unless the change is small and clearly in the tool's read-only open.
4. A decisions file `docs/decisions/2026-11-13-X-flake-relevance-lock.md` (short: cause, fix).

**Owns:** `bench/tests/relevance_sets.rs`, `bench/src/bin/relevance/**` (only if step 3
applies), `docs/decisions/2026-11-13-X-flake-relevance-lock.md`.

**Checks.** `cargo fmt --check`, `cargo clippy -p lodestar-bench --all-targets -- -D warnings`,
`cargo test -p lodestar-bench --test relevance_sets`. CI's Windows job is the real proof; say
in the ready message whether `test (windows-latest)` passed on your head.

**PR title:** `X-flake-relevance-lock: <short title>`. Branch `slice/X-flake-relevance-lock`.
No cloud, no AWS. Follow `.claude/skills/steward/SKILL.md`; the runner merges.
