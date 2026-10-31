# B-held-precommit: no window between a new hold's commit and intake refusing it
**Model:** Opus: content safety (§20), the pipeline writer and `HeldSet` (concurrency). Runner brief, 2026-10-31. Follow-up to B-held-exact (#287), from its review's optional 4. `/security-review` is required. **This slice must never loosen content safety.** If any path would let a held hash be fetched, stored, indexed or shown, that's a blocking defect.

## Why
After #287, `HeldSet` follows the store exactly *once each reconcile finishes*. One window is left. Between a batch's store commit (C) and its locked query and insert (Q+I), the store holds a hash that `HeldSet` doesn't, so intake's `contains` can let it through. The same window exists on the paths #287 left alone:
- the writer's ingest commit (`commit`'s blocked arm);
- the API's `correct` endpoint.

Both insert after the store commit, without a pending guard.

## Read first
- `docs/decisions/2026-10-31-B-held-exact.md`: the proof, the 11 interleavings, and "Not changed".
- `docs/architecture.md` §20.3, §20.9 and §20.11.
- `crates/filter/src/held.rs`: `HeldSet`, `insert_held`, `remove_released`, `CHECK_CHUNK`.
- `crates/pipeline/src/writer.rs`: every `HeldSet` call, the sweep and backfill arms, `commit`'s blocked arm, `purge` and the `between` hook.
- `crates/pipeline/src/lib.rs`: intake's `held.contains` and `blocked_hash`.
- `crates/api/src/filters.rs`: the releases and `correct`.

## Do
1. **Pick the mechanism and prove it in the decisions file.** The obvious move is to insert before the commit and reconcile after it. That alone is **unsafe**: a release's M step running between the pre-insert and C reads the store as "not held" and removes the hash, which reopens the hole.

   An acceptable shape is a **pending guard**: the writer marks the batch's hashes pending in `HeldSet` before C. `contains` treats a pending hash as held. `remove_released` never removes a pending hash. After C, the writer's Q+I converts pending to held exactly, using the store as #287 does, and clears pending. On a rollback or failure, the pending entries are cleared through the same reconcile. A different design is fine if you prove it.

   Either way, trace every interleaving of the pre-mark, C, Q+I, S, M, V, a crash, and the restart reload, and list them as #287 did.
2. **Cover all four paths:** the sweep, backfill, the ingest commit's blocked arm, and `correct`.
3. **Fail closed:** pending entries left by an error stay refused until the next start, never dropped. Pending must stay bounded: at most one batch per writer, plus the API's in-flight corrections.
4. **Tests:** deterministic interleavings only, through `between` or explicit steps, with no sleeps.
   - Intake's `contains` returns held at every point between the pre-mark and Q+I.
   - A release's M between the pre-mark and C doesn't remove a pending hash, and the final state equals the store's.
   - A rolled-back batch leaves no pending and no held entry for hashes the store doesn't hold.
   - After a restart, the in-memory set equals the store's.
   - Cover all four paths.
5. **Carry #287's two optional nits:**
   - in `held.rs` ~6-9, move `CHECK_CHUNK` and its one-line doc above the "Tombstoned and quarantined keys" comment, so `HeldSet` gets its doc back;
   - in #287's decisions file, change the "Errors fail closed" bullet to "that chunk and every later one".
6. **Decisions file:** `docs/decisions/<date>-B-held-precommit.md`.

## Own
- `crates/filter/src/held.rs`.
- `crates/pipeline/src/{writer.rs,lib.rs}`: the `HeldSet` call sites and `contains` only.
- `crates/api/src/filters.rs`: `correct` and the release reconciles only. X-requeue-2 is editing the re-queue functions in that file; don't touch them. Whichever PR merges second brings in main.
- Their tests, #287's decisions file (item 5 only), and your decisions file.

**Don't touch** `crates/search/**`, `web/` or the store schema.

## Size
≲ 500 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass (with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`), plus the openapi drift check and the filter budget bench (§20.8). Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally: the seven-day usage warning is escalating. Push once per round of fixes.
