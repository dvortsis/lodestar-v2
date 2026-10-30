# F-metainfo: metainfo parser fixes (A1-1..4, release blocker; untrusted input)
**Model:** Opus (untrusted-input parser).

## Base and PR timing (read first)
- Branch `slice/F-metainfo` from `origin/next` (the rewritten history that replaces `main`; owner-decisions §2.25 on `origin/claude/busy-allen-aptli2`). Never base on, merge or pull the old `origin/main`: its history is being replaced, and a PR head containing the old root `15620683` or old tip `2d5e761d` will be refused.
- Until the owner force-pushes `main` (the runner tells you), GitHub cannot open a PR between the two histories. Build, test, self-review and push your branch, then message the runner "ready, waiting for the swap". Open the PR to `main` only when the runner says the swap is done.
- The review report is on another branch: read it with `git show origin/fix/dht-cr0:<path>`.

**Spec:** `docs/code-review/codex-review-v0.5-area-1-results.md` findings 1–4 (on fix/dht-cr0), architecture §6, BEP 47, BEP 52. Finding 5 (empty BEP 51 samples) is already fixed by CR-0; don't touch `crates/dht`.

## Fix (`crates/metainfo/src/info.rs`)
1. ~343: choose each file's valid `path.utf-8` independently, not all-or-nothing; keep the encoding info for remaining legacy paths.
2. ~537: read attributes before length; an absent symlink length is 0.
3. ~509: padding entries may omit `path`; assign a deterministic internal path that satisfies the non-empty-path invariant and stays excluded from counts and search like other padding.
4. ~704: parse v2 symlink targets with the existing bounded path parser.

## Tests
Golden-corpus cases for each (the report's inputs), property tests round-tripping mixed overrides, symlinks and padding through parse → store serialization, and extend the fuzz targets (`fuzz/fuzz_targets/metainfo.rs`, `filetree.rs`) with seeds for these shapes. Run each fuzz target at least 10 minutes locally; report execs and any crash.

## Own
`crates/metainfo/**`, `fuzz/**`, `docs/decisions/<date>-F-metainfo.md`.

## Done when
fmt, clippy `-D warnings`, `cargo test --workspace` pass; fuzz runs clean; fresh-reviewer self-review done.
