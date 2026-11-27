# X-watch-open-fstat: Watch folder: Folder::open checks the folder it opened, not the path

**Runner brief, 2026-11-27T12:29Z.** Core, Opus (filesystem handling of an operator-set path; Unix and Windows). Target < 400 changed lines. `/security-review` before you report ready.
- **Branch:** `slice/X-watch-open-fstat` from fresh `origin/main` (with #1137 merged as eda422be).
- **PR title:** `X-watch-open-fstat: Watch folder: Folder::open checks the folder it opened, not the path`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- The `runner/slices.yaml` entry already exists (r18). Don't add another one, and edit nothing else in that file or in `runner/dashboard.yaml`.

## Why
#1104's review found a time-of-check/time-of-use race in `Folder::open` (`crates/lodestar/src/watch.rs`). It checks a **path** (that it's a directory, its owner and mode, not a symlink and so on) and then opens it, or opens it and then checks the path again. Swapping a parent directory between the two makes the checks describe a different folder from the one Lodestar reads.

The watch path is set by the operator, so the finding is low and not held. Still, the checks should describe what Lodestar actually opened.

## Do
1. Read `Folder::open` and every check it makes. Open the folder **once**, then make every check against the opened handle:
   - **Unix:** `File::open` / `O_DIRECTORY | O_NOFOLLOW` where the code already avoids symlinks, then `fstat` (`metadata()` on the handle) for type, owner and mode.
   - **Windows:** open with `FILE_FLAG_BACKUP_SEMANTICS`, which is needed for a directory, and `FILE_FLAG_OPEN_REPARSE_POINT` if reparse points are refused today. Then use `GetFileInformationByHandle` or the std handle metadata.
   - Later reads and listings must go through the opened handle where the platform allows it (`openat`-style or `fdopendir`, with no new crate unless one is already in the tree). If a read must use the path, re-check the handle's identity against the path (dev/ino, or the volume serial and file index) and refuse on a mismatch.
2. Keep the behaviour the same for a normal folder. Keep the same error kinds, catalogued log events and user-facing messages. Don't loosen any check.
3. **No unsafe code** (`unsafe_code = "forbid"`). Use std, or crates already in the tree (`rustix`, `windows-sys` or whatever `watch.rs` already uses). Ask the runner before adding a dependency.
4. **Tests** in `crates/lodestar/tests/watch.rs`:
   - A parent swapped for a symlink after the open still reads the original folder, or is refused. It never reads the other one.
   - A file in place of the folder is refused.
   - A symlinked watch path follows today's rule.
   - A normal folder still works.
   - Gate the platform-specific tests with `cfg`.
   - No fixed sleeps. Use condition waits with a deadline of at least 10 s.

## Owns
`crates/lodestar/src/watch.rs`, `crates/lodestar/tests/watch.rs` and `docs/decisions/*-X-watch-open-fstat.md`.

**Not yours:**
- `crates/lodestar/src/config.rs`, `metrics.rs` and `docs/metrics.md` (CR-0w holds them);
- `crates/search/**` and `crates/api/**` (X-import-verified-no);
- `crates/store/**` and `crates/pipeline/**` (X-claims-refile-rearm).

## Done
Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p lodestar --test watch`, plus `--lib` for the watch module, and say which you ran. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Windows CI must pass. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending and no force-push. Held security detail stays off every branch.
