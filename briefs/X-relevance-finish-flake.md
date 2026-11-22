# X-relevance-finish-flake (runner brief, 2026-11-22Z)

Kind: core, model opus. Workstream: the bench one (place it beside X-flake-relevance-lock). Deps: none.

## Why
`bench/tests/relevance_sets.rs`, `refused_while_a_server_holds_it`, failed once in CI at its last
assertion: `assert!(run_on(...).status.success())` after `writer.finish().unwrap(); drop(index);`.
It passed on one re-run. The assertion prints nothing, so the cause can't be read from the log.

## Step 0 (send to the runner before building)
1. Find what can still hold the index when the tool runs right after `writer.finish()` and
   `drop(index)`. Candidates: tantivy's writer lock or merge threads released after `finish`
   returns; a `SearchIndex` reader, the directory watcher or mmap still open; or the Windows sharing
   rule (os error 32, see X-flake-relevance-lock's decisions file). Name the mechanism from the code
   (tantivy's IndexWriter drop/finish path, lodestar-search's SearchIndex) and say which OS it
   bites on.
2. Propose the fix:
   - Prefer one that makes the release deterministic: wait on the real condition (the writer's
     threads joined, the lock file gone), or close in the order that releases the lock.
   - Wait with a deadline of at least 10 s only if the release really is asynchronous.
   - No fixed sleeps. Keep every assertion.
   - If the tool itself should wait briefly for a just-released lock, say so; keep the change in
     the test unless it's small and in the tool's read-only open.

## Rules
- Every `refused(...)` and success assertion prints the tool's stdout and stderr on failure.
- Never skip, ignore or cfg-out a test. No fixed sleeps.
- Owns: bench/tests/relevance_sets.rs, docs/decisions/*-X-relevance-finish-flake.md, its own
  slices.yaml entry and placement; bench/src/bin/relevance/** only if step 0 says so.
- Checks: cargo fmt --check, cargo clippy -p lodestar-bench --all-targets -- -D warnings, and
  cargo test -p lodestar-bench --test relevance_sets run 20 times in a loop with 0 failures (say
  how it was run).
- Branch slice/X-relevance-finish-flake, PR title "X-relevance-finish-flake: <title>". Push once
  per round, then send "ready <pr> <sha>". Follow .claude/skills/steward/SKILL.md; never merge.
