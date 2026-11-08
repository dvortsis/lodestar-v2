# CMP-2: Companion archive extraction and install layout (zip, tar.gz, bounded, fuzzed; versioned dirs, atomic swap, rollback, removal)

**Model:** Opus. **Review:** a fresh Opus reviewer, plus `/security-review` and an unanchored second read (2R). This slice handles untrusted input. **Branch:** `slice/CMP-2`, cut from `origin/main`. **PR title:** `CMP-2: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`:
  - §3 (store impact, no migration);
  - §4's CMP-2 bullet;
  - §5 Q13 (dependencies) and Q16 (caps: 20,000 entries, 2 GB total, a 100:1 compression ratio; a hard 512 MB per download is CMP-1's).
- `docs/architecture.md` §12 and §18.10.
- `crates/companions/` as CMP-0 left it: `catalogue.rs` (the `archive: zip|tar.gz|installer` field and the executable path inside the archive), `inputs.rs` (`CompanionId`) and `adapter.rs`.
- `crates/lodestar/src/update/download.rs`, for the file-handling discipline (`O_NOFOLLOW`, 0600, fsync, rename).
- `runner/standards.md` is the bar.

## Owns
- `crates/companions/src/extract.rs`
- `crates/companions/src/layout.rs`
- `crates/companions/tests/extract.rs`
- `fuzz/fuzz_targets/companion_archive.rs`

**Shared edits the runner allows:**
- one `mod` line each in `crates/companions/src/lib.rs`, plus `pub use` if needed, and new events in `crates/companions/src/log_events.rs`. Events carry no paths from the archive beyond a bounded, sanitised entry name;
- `crates/companions/Cargo.toml`: `zip` with `default-features = false` and deflate only, `tar`, and `flate2` (already in the lockfile). Check each crate's licence and maintenance, record the result in the decisions file, and pin them like the crate's other dependencies;
- `Cargo.lock`;
- `web/licenses/rust.json`, if the licence generator lists the new crates;
- `fuzz/Cargo.toml`, one `[[bin]]`, plus a seed corpus under `fuzz/seeds/companion_archive/` (small crafted archives only);
- one `docs/decisions/2026-11-08-CMP-2.md`.

CMP-3 (#598) is also adding lines to `crates/companions/Cargo.toml`, `lib.rs` and `Cargo.lock`. Whichever PR merges later brings `main` in. That is a trivial conflict, not an owns overlap.

Anything else goes to the runner first.

## What to build
1. **Extraction (`extract.rs`), zip and tar.gz, streaming and bounded.**
   - Extract into a fresh `<data>/companions/<id>/<version>.staging/`. It is created new (refuse if it exists; clean a stale one only after checking it is a plain directory under `<data>/companions/<id>/`). It is renamed into place only when extraction is complete and fsynced.
   - **Refuse, for the whole archive (never skip-and-continue):**
     - absolute paths, a drive letter or UNC prefix, `..` components and empty components;
     - backslashes treated as separators;
     - names Windows can't hold safely: `:` (alternate data streams), reserved device names (`CON`, `NUL`, `COM1` and so on, with any extension), and trailing dots or spaces;
     - symlinks and hard links;
     - device files, FIFOs and sockets;
     - setuid, setgid and sticky bits;
     - duplicate names, compared after normalisation and case-folding, so a name that collides on a case-insensitive filesystem is refused too;
     - more than 20,000 entries;
     - more than 2 GB in total. Count the bytes actually written, never the header's claimed size: one byte past the cap stops it;
     - a compression ratio above 100:1, checked as bytes are written per entry and for the whole archive.
   - Permissions: files 0644 and directories 0755. The catalogue's executable path is the only file that is 0755. On Windows, inherit the companion folder's ACL; no new ACL code.
   - Open every output with no-follow (create-new, `O_NOFOLLOW`/`O_EXCL` on Unix). An entry whose path traverses something that isn't a directory we just created is refused. Never follow a symlink that already exists on disk.
   - The executable path the catalogue names must exist in the archive as a regular file, or the archive is refused.
   - **Failure leaves nothing behind:** the staging directory is removed on any error, and the error is typed and carries no unbounded attacker text.
   - No `unsafe`, and no `unwrap` or `expect` outside tests.
2. **Install layout (`layout.rs`).**
   - Versioned directories `<data>/companions/<id>/<version>/`.
   - An atomic "current" pointer that works on Windows without symlinks, for example a small `current` file written by temp, fsync and rename. Record the design in the decisions file. The state row itself is CMP-5's.
   - Switching to a new version is atomic. The previous version is kept for rollback until the caller (CMP-5) says the new one passed health; rollback restores the pointer.
   - **Removal deletes only paths under `<data>/companions/<id>/`.** Walk with no-follow; a symlink or reparse point found inside is removed as a link, never followed. Canonicalise and check the root before deleting.
   - Expose an API that CMP-5 calls: stage, commit, rollback, prune the previous version, and remove. Build no state rows, CLI or endpoints here.
3. **Fuzzing.** `fuzz/fuzz_targets/companion_archive.rs` covers both formats through the same entry point with a temp root. It asserts that nothing is ever written outside the staging directory and that a failure leaves no staging directory. Seed it with the crafted test archives.

## Tests and gates
- **Crafted archives, one test per refusal above, for both formats where the format allows it:**
  - zip-slip and tar `..`;
  - absolute paths and drive letters;
  - symlink and hard-link entries;
  - a symlink planted on disk before extraction;
  - devices;
  - setuid;
  - case-colliding duplicates;
  - the entry cap and the size cap (a header that lies about the size);
  - a zip bomb over the ratio;
  - a missing executable path;
  - a truncated or corrupt stream.

  Build the archives in the test with the `zip` and `tar` writers, or with a hand-built header where a writer won't produce the malformed case. Only small seeds are committed.
- Property tests: arbitrary entry-name lists are either refused or extract strictly under the staging directory.
- Layout tests: commit, rollback, prune and remove, including a crash between the steps (simulated by stopping after each one). Removal refuses a root outside `<data>/companions/<id>/`.
- No fixed sleeps. Tests run on Windows CI too.
- Before pushing:
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace`, at minimum `-p lodestar-companions`;
  - the fuzz target builds, and a short `cargo fuzz run companion_archive -- -max_total_time=60` if nightly is available; say so if it isn't.
- **Size:** about 800 lines per the plan; stay well under 1,500.

## Decisions you may make (§2.33; record each in the decisions file)
- the "current" pointer design;
- how normalisation and case-folding are done;
- the error type shape;
- the API that CMP-5 calls.

Anything that loosens a refusal or a Q16 cap goes to the runner.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA.
