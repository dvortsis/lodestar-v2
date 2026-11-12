# UP-1: Folder watch

**Runner brief, 2026-11-12 (UTC).** Opus, core work, `/security-review` (it reads untrusted files from a user-writable folder).
- **Branch:** `slice/UP-1` from fresh `origin/main`, at or after 84051209 (#753 UP-0).
- **PR title:** `UP-1: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.
- **Size:** about 700 changed lines (plan estimate).

## Goal
A watch folder. Drop a `.torrent` file into it and Lodestar takes it exactly as it takes an upload: the same parse, the same judgement and the same commit. That's `pipeline::local_torrent` (`read`, `Uploads::submit`) and SRC-13b's verified-file path. The spec is plan `docs/plan/2026-11-05-v07-m7.md` §4.3 (the UP-1 line), plus §3 and §18.7 of the architecture.

## Owns
- `crates/lodestar/src/watch.rs` (new)
- `crates/lodestar/src/settings/watch.rs` (new)
- `crates/lodestar/tests/watch.rs` (new)
- `docs/decisions/2026-11-12-UP-1.md`

**Shared, and allowed small:**
- `crates/lodestar/src/services.rs`: the registration lines. ENR-14 is also editing this file; whichever lands later brings main in.
- `crates/lodestar/src/settings/mod.rs` (or wherever settings register): the registration lines.
- `crates/lodestar/src/metrics.rs` and a `docs/metrics.md` row, if you export counters.
- The three UP-0 follow-ups below (N1–N3), in UP-0's files: `crates/pipeline/src/local_torrent.rs`, `crates/lodestar/tests/upload.rs` and `crates/api/src/alerts.rs` (a test only).

Tell the runner **before** pushing if anything else needs to change. That includes `crates/pipeline/src/writer.rs`, `crates/api/**` beyond the alerts test, or `crates/store/**`.

## Read first
- `docs/decisions/2026-11-12-UP-0.md`: the upload path, its bounds, its error causes, and "nothing from the file in a log line".
- The UP-1 line of plan §4.3 and its §6 note ("Folder watch adds no network surface").
- `crates/pipeline/src/local_torrent.rs`, and how `services.rs` and `Pipeline::uploads()` wire the upload route.
- `crates/lodestar/src/privdrop.rs` and `fsdetect.rs`, for Docker, SMB and permissions.
- CLAUDE.md and `runner/standards.md`.

## Requirements
1. **Off by default, one folder.**
   - The setting is a folder path, plus "after adding: move (default) or delete".
   - Validate the path when it's saved and again at start:
     - it's absolute, it exists, and it's a directory;
     - it isn't the data directory or inside it, and the data directory isn't inside it.
   - A bad path disables the watch, with one clear status reason. Nothing is ever read outside the folder.
   - Say in the decisions file whether a change applies live or needs a restart.
2. **Polling, not filesystem events.**
   - Poll at a fixed interval, ≥ 5 s; 10 s is the suggested default. It must work on Docker bind mounts and SMB.
   - Bound each poll:
     - at most N directory entries scanned and M files submitted. Set both, as named constants, justified under §2.26;
     - the next poll continues where this one stopped, so a big folder drains in order and nothing starves.
3. **Safe file handling.** This is the core of the slice.
   - **Only regular files directly in the folder**, whose name ends in `.torrent` (ASCII case-insensitive). No recursion.
   - **Never follow a symlink, a junction or any other reparse point.** That includes the folder's own `added/` and `failed/`.
   - **Check the handle, not the name:**
     - open with no-follow (`O_NOFOLLOW` on Unix; on Windows, `FILE_FLAG_OPEN_REPARSE_POINT` and refuse a reparse point);
     - confirm from the open handle that it's a regular file and within the cap;
     - read through a reader limited to cap + 1.

     A file that was swapped for a symlink between the listing and the open is refused.
   - **Cap:** `local_torrent::MAX_BYTES`, the upload cap. An oversize file is refused **without being read** and goes to `failed/`.
   - **Wait until it's complete.** Read a file only once its size and modification time match across two polls. Skip a file another process holds open, such as Windows sharing violations or SMB locks, until a later poll. Never count it as a failure.
4. **Afterwards, it's never read again.**
   - **Move to `added/` or `failed/`** inside the folder: the outcome is accepted, malformed, refused or held. Or delete it, if the setting says so.
     - Create both subfolders without following links. If one exists as a link, or isn't a directory, disable the watch with a status reason.
     - Never overwrite: on a name clash, add a numeric suffix.
     - `held` goes to `failed/`. Decide whether it gets a distinct name or a subfolder, and record it.
   - **If the move or delete fails** (for example a read-only mount):
     - remember the file in a **bounded** in-memory set (by name, size and mtime), so it's never re-submitted;
     - report it once in the status, not once per poll;
     - when the set is full, disable the watch with a reason. Never evict and re-read.
5. **The same path as an upload.**
   - Submit through `Uploads::submit` (or the same `read` → judge path), so a held or floor-blocked file is never stored. `private=1` gets both privacy bits exactly as an upload does.
   - The provenance source: reuse UP-0's built-in `upload` source unless you find a reason for a separate one; record the choice.
   - Never bypass the judge, and never write to the store directly.
6. **Nothing from the file, and no file name, in logs or status.**
   - Names in a user's folder are often torrent titles, adult ones included. Log and show counts and outcome causes only.
   - Only the info hash may appear, as in UP-0. The `added/` and `failed/` folders are how the user finds a specific file.
7. **No network surface.** No new route in this slice; the settings API and UI are UP-2's. If you find you need a route, ask first.
8. **The UP-0 follow-ups (from rv-753).** These are in scope because the watch uses the same path:
   - **N1:** the served-binary test waits for the private alert before the RSS check (`crates/lodestar/tests/upload.rs:283` vs `:265`). Make the order explicit with a condition wait.
   - **N2:** a unit test for the `PRIVACY` mask in `crates/api/src/alerts.rs:352`.
   - **N3:** a panic while judging an upload ends the judge task, and every later upload (and now every watched file) is refused until a restart.
     - Catch it per item (`catch_unwind`, or an equivalent supervised restart), so one bad file fails alone, counted, and goes to `failed/`.
     - Nothing from the panicking file is stored.
     - No `writer.rs` edit.
9. **Tests** (`crates/lodestar/tests/watch.rs`, plus unit tests in `watch.rs`):
   - **No tokio `test-util` or `testing` feature on the `lodestar` package** (owner §2.58).
     - Make one poll a plain function you can call directly, and drive it step by step from tests.
     - Any real wait is a condition with a deadline ≥ 10 s. No fixed sleeps.
   - **Cover:**
     - an accepted file goes to `added/`;
     - a malformed one, an oversize one (never read; check that no read happened) and a held one go to `failed/`;
     - a growing file isn't read until it's stable;
     - non-`.torrent` names and subdirectories are left alone;
     - a symlink entry is refused, and so is a file swapped for a symlink between the listing and the open (Unix; Windows where CI can create one);
     - `added/` existing as a symlink disables the watch;
     - a name clash gets a suffix;
     - a read-only folder: no re-read loop, a bounded set, then disabled when full;
     - the per-poll bounds and resuming in order;
     - the data-directory rules for the path;
     - delete mode;
     - N3: a panicking judge fails one item and the next one is still accepted;
     - no file name in any log line (capture logs as the existing tests do).
   - Tests must pass on Windows CI, so mind path handling and file locks.
10. **Docs.** The user guide's Docker bind-mount text is a docs slice's job, after UP-2. Put the facts it needs in the decisions file under "For the guide": the container path, permissions under privdrop, and what `added/` and `failed/` mean.

## Not in scope
- The settings UI and API (UP-2).
- Recursion or several folders.
- Magnet-link text files.
- Filesystem-event watchers.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar -p lodestar-pipeline -p lodestar-api`, and the workspace if disk allows.

The PR description names the shared edits and the §2.26 numbers. Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus review with `/security-review` follows.
