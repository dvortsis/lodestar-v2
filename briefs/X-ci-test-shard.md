# X-ci-test-shard (runner brief, 2026-11-28)

**Slice:** X-ci-test-shard. Opus, CI (the required `ci-ok` check depends on it).
**Branch:** `slice/X-ci-test-shard` from current `main`.
**PR title (exact):** `X-ci-test-shard: The Rust test jobs finish sooner and still run every test`
**Owns:** `.github/workflows/ci.yml` (the `test` job, plus `ci-ok`'s `needs` and its result check
only), `.github/scripts/ci_changes.py` and its test file if the split needs a new output,
`docs/decisions/*-X-ci-test-shard.md`. Nothing else in ci.yml. Above all, not the `relevance` job,
which M8-REL-2 edits next.

## Why
The owner asked (2026-11-28) to speed up development without lowering quality. On #1181's push
of 2026-11-28T17:33Z, every other check finished within 15 min. `test (ubuntu-latest)` and
`test (windows-latest)` were still running at 27 min. They set the pace of every Rust PR, and
every PR queues behind them.

## Do
1. **Step 0: measure, don't guess.**
   - From the job logs of at least five recent runs of both `test` jobs (main and PRs), split the
     time into setup, cache restore, Windows clippy, compile and test run.
   - List the ten slowest test binaries or tests on each OS.
   - Propose the design with its expected wall time per OS. Options: build once and share the
     test binaries across N shards (for example a `cargo nextest archive` and `--partition`), shards
     by crate group, clippy (Windows) in its own parallel job, or fixing a few slow tests. Choose by
     the numbers.
   - Send it to the runner and wait for the reply.
2. **Hard rules.**
   - Every test that runs today still runs, on the same OS, in the same cases (ci_changes.py's
     selection unchanged). The PR shows the count of tests run per OS, before and after, and they
     match.
   - `--no-fail-fast` semantics stay: one failure doesn't hide others.
   - `ci-ok` fails if any shard fails or is cancelled. A skipped shard counts exactly as a skipped
     `test` job does today.
   - The later steps in the job (the DHT's production configuration, the counted test lists, the
     privdrop, cli and worker_join binaries) all still run.
   - Mind the Linux disk limit (see the job's comments): no shard may fill it.
   - A new tool or action is a new dependency (`runner/standards.md` §2). Pin it to an exact
     version and a commit SHA, name it in the PR, and get the runner's approval in step 0.
   - Workflow `permissions` don't widen. No secrets.
3. **Prove it on the PR itself.** Give the wall time of both OS jobs over at least two runs, against
   main's, and the test counts.
4. `/security-review` on the workflow diff, a decisions file, `.claude/skills/steward/SKILL.md`,
   and report `ready <PR> <sha>`. The runner merges it at a quiet moment, since it changes every
   PR's CI.
