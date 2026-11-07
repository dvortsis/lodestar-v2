# X-refetch-private: never send a private or remote-unverified key to public peer finding from the start's refetch queue
**Model:** Opus, with `/security-review` (§18.7, BEP 27). A fresh Opus reviewer follows.

Runner brief, 2026-11-07T12:35Z. Slot 3.

## The finding
Found in the WK-9b (#571) delta review, and confirmed by the runner on `main` 8979a9b7:
- `crates/pipeline/src/lib.rs`, at the start (around line 1374 on main): imports with incomplete file lists are queued for refetch through the first enabled peer finder, with **`private_source: false`**.
- `Store::partial_hashes` (`crates/store/src/lib.rs:1026`) selects `flags & FILES_PARTIAL != 0`, with **no privacy filter**.
- `FILES_PARTIAL | PRIVATE_SOURCE` and `FILES_PARTIAL | PRIVATE` are real states. The torrent cache's tests build both (`crates/lodestar/src/torrent_cache.rs:443-449`), and client imports are `finds_private`.
- So a private import with a partial file list goes to the DHT peer finder as public after every restart. That leaks a private infohash to public DHT/PEX, which §18.7 and BEP 27 forbid.
- `release/v0.5.1` has the same line (`lib.rs:913-922`). There its imports come from the DHT, so the practical risk is low. Say in the decisions file whether a v0.5.1 backport is needed; the runner and the owner decide on releases.

## Do
1. **Fix the start's partial refetch** so no key whose stored flags intersect `flags::PRIVACY` (`PRIVATE | PRIVATE_SOURCE | REMOTE_UNVERIFIED`) is queued for public peer finding.
   - Prefer excluding them in the query (`partial_hashes` takes a mask, or a new `partial_public_hashes`), so the cap of 50,000 counts only keys that can actually be refetched.
   - If you keep them and carry `private_source: true` instead, show that the pool and every peer finder then refuse public lookups for them, and that they don't fill the cap.
   - Pick the simpler of the two that's provably safe.
2. **Audit every other construction** of `QueuedKey`, `Found` and pool entries, and every other literal `private_source: false`, in `crates/pipeline/src/**` and `crates/sources/src/**`. That includes `writer.rs`'s refetch and recovery paths, the restore of the saved fetch queue (`load_fetch_queue`), feeds and imports. For each one, either take privacy from the store's flags or the finding sources, or explain in a comment why it can't be private. List them all in the decisions file.
3. **Tests**, each failing without its fix:
   - a store with a public partial, a `PRIVATE_SOURCE` partial, a `PRIVATE` partial and a `REMOTE_UNVERIFIED` partial: only the public one is queued for the peer finder, and the private ones never reach a public lookup;
   - the same through a restart of the real pipeline, if there's a harness for it (the `crates/pipeline/tests` start tests);
   - any other path the audit fixes.
4. **The decisions file:** `docs/decisions/2026-11-07-X-refetch-private.md`. Cover the finding, the fix, the audit list, and v0.5.1 exposure with a backport recommendation.

## Owns
- `crates/pipeline/src/lib.rs`, **only** the start's partial-refetch block and any audit fixes, as small hunks. WK-9b (#571, slot 1) also edits `lib.rs` (the saver, shutdown and restore check) and `store/src/lib.rs` (`measure_files`). Keep your hunks apart from theirs; whichever merges later brings `main` in. Don't touch their functions.
- `crates/store/src/lib.rs`: `partial_hashes` (or a sibling) and its tests.
- New test files under `crates/pipeline/tests/` or `crates/store/tests/`.
- `docs/decisions/2026-11-07-X-refetch-private.md`.
- **Ask the runner first** before any other edit, naming the function, for example `writer.rs` if the audit finds something.
- **Don't touch:** `crates/pipeline/src/remote.rs` and `rollups.rs` (WK-9b's), `crates/pipeline/src/wire/**` and `crates/lodestar/src/cluster/filter.rs` (WK-8, #572), and `crates/sources/src/pool.rs` (X-fetch-meta-reserve, slot 4; ask first if the fix needs it).

## Done when
- No private, private-source or remote-unverified key reaches public peer finding from any queue path, and each case is tested.
- Single-machine parity for public torrents: public partial imports are refetched exactly as before.
- No fixed sleeps; waits on conditions with deadlines ≥ 10 s. No tokio `test-util` or `testing` features on the `lodestar` package.
- Checks: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. Disk is limited, so per-package test runs are fine if you say so; CI runs everything.
- Size: small, about 150–300 lines with tests.
- Process:
  - follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge;
  - title the PR `X-refetch-private: Keep private keys out of the start's refetch`, on branch `slice/X-refetch-private`;
  - message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green.
