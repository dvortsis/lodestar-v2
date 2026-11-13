# X-watch-stall-retry

**Goal:** a stalled copy into the watch folder must not be failed for good. Today a file that reads as Malformed on one poll moves to `failed/` at once. On Unix, a slow SMB or NFS copy that stalls for a few seconds looks Malformed mid-copy, so it is lost to `failed/`. Track Q (code review 2026-11-12); `runner/slices.yaml` entry X-watch-stall-retry.

**Owns:** `crates/lodestar/src/watch.rs`, `crates/lodestar/tests/watch.rs`, `docs/decisions/2026-11-13-X-watch-stall-retry.md`.

**What to build**
- A file read as Malformed moves to `failed/` only if the next poll reads it Malformed again, **with the same size and modified time**. If either changed, it's still being written: keep waiting, and judge it fresh next poll.
- Bound the wait so a file that keeps changing can't stay pending forever. Reuse the existing stability or settle logic if there is one, and name the bound in the decisions file.
- Keep every other verdict path as it is (valid, duplicate, too large, unsupported). Only Malformed gets the second look.
- If the pending state lives in memory, a restart re-judges from scratch. That's fine; say so in the decisions file.

**Tests** (`crates/lodestar/tests/watch.rs`, plus unit tests if they fit better):
- A stalled copy: write a truncated `.torrent`, let one poll see it, keep it unchanged for longer than 10 s (a condition wait on the poll count, never a fixed sleep), then finish the write. The file is imported, not failed.
- A really malformed file that doesn't change goes to `failed/` on the second poll.
- A file that keeps growing past the bound is handled as the decisions file says.
- No fixed sleeps. Waits are on conditions with deadlines of at least 10 s (owner §2.24). Never add tokio `test-util` or `testing` to the `lodestar` crate.

**Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar --test watch` (5 runs, with the counts in the PR body), and the workspace tests. Run `/security-review` on the diff (filesystem input), and put its result in the PR body.

**PR:** branch `slice/X-watch-stall-retry`, title `X-watch-stall-retry: Hold a Malformed verdict for one more poll`. Follow `.claude/skills/steward/SKILL.md`. Push once per round. When CI is green, message the runner `ready <PR> <full sha>`. No cloud work, no AWS.
