# X-win-data-dir-followups (Opus; same session as #983)

Runner brief, 2026-11-20. Follows #983 (merged b8e2a3b0); its review said MERGE and left these items.

## 1. Write access (the main item, UX)
A data folder the service account can list but **not write to** still stops with error 1 and no Event Log entry. That's the original symptom, with a third cause.
- Add `Problem::Unwritable`: probe by creating and then deleting a uniquely named temp file in the folder, `create_new` with no follow of reparse points, after the existing checks.
- Refuse with Event Log ID 6 and fix text along the lines of: grant the service account write access, or point the setting at a folder it can write to.
- On Windows, a probe file left behind by a crash must not break the next start: give it a unique name and ignore an existing one.
- Tests: a write-denied folder refuses (icacls on Windows; mode bits on Unix, skipped under root as now); a writable folder passes and leaves no file behind.

## 2. Review optionals from #983
- a. `unusable_fix` for `Problem::Uncreatable` (service.rs ~448): when a parent is a file, "create that folder" is advice that can't work. Mention moving any file in its path away, or say which part is a file.
- b. The test at ~2179: assert that the service's own folder is not created by `unusable(default_dir, false)`, e.g. `assert!(!svc.exists())` inside the `secure` closure.
- c. Make `load_guarded` `#[cfg(test)]`, move its doc to `load_checked`, and update the intra-doc links. Leave cli/mod.rs:390 alone; it's outside your owns.

## 3. User guide
`docs/user/06-troubleshooting.md` §6.2: describe Event Log ID 6 (what it means, the four problems, the fix for each). Use the project `user-docs` skill (`.claude/skills/user-docs/SKILL.md`) and the owner's `github-docs` rules: plain, short, defined terms, the safe choice.

## Owns
- crates/lodestar/src/windows/service.rs
- docs/user/06-troubleshooting.md
- docs/decisions/2026-11-20-X-win-data-dir-unusable-log.md (append a short "Follow-up" section) or a new docs/decisions/2026-11-20-X-win-data-dir-followups.md

## Size and checks
≲ 400 lines. cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test -p lodestar; the docs checks the user-docs skill names. No fixed sleeps. Windows code compiles first on CI's test (windows-latest).
