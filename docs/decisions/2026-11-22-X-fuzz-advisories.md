# X-fuzz-advisories: the fuzz workspace's lockfile is checked against RustSec

Runner's call under `docs/owner-decisions.md` §2.26; recorded 2026-11-22Z.

- **Gap.** `fuzz/` is its own workspace with its own `fuzz/Cargo.lock`. The `advisories` job checked only the root `Cargo.lock`.
- **Advisory.** RUSTSEC-2026-0385 hit `ctutils` 0.4.2 in `fuzz/Cargo.lock` (reached through `digest`): constant-time mask generation could branch. Fixed from 0.4.4. `cargo update -p ctutils --precise 0.4.8` in `fuzz/` moved `ctutils` only (0.4.2 to 0.4.8). No ignore.
- **CI.** The `advisories` job runs one more step, `cargo deny --manifest-path fuzz/Cargo.toml --locked check advisories`. The job's name, `if:`, permissions and triggers are unchanged. `ADVISORIES` in `ci_changes.py` now matches `fuzz/Cargo.lock`, so a fuzz-lockfile-only PR runs the job.
- **Config.** cargo-deny searches upward for `deny.toml`, so the root file applies to `fuzz/` (targets, unmaintained, yanked and the empty ignore list). No fuzz-specific config.
- **Root check.** Unchanged and still clean.
- **Supersedes** the bullet "`fuzz/Cargo.lock` isn't checked" in `2026-11-13-X-ci-advisories.md`. The reason: the fuzz lockfile carried a live vulnerability (RUSTSEC-2026-0385) that nothing flagged.
