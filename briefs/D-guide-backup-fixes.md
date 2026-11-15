# D-guide-backup-fixes: backup restore steps and §3.3 wording

Runner brief, 2026-11-15. Sonnet, user docs (cloud-testing fix slice, §2.62). Use `.claude/skills/user-docs/SKILL.md` on top of `github-docs`. Branch `slice/D-guide-backup-fixes` from current main. PR title `D-guide-backup-fixes: backup restore steps and §3.3 wording`. Target ≲ 400 changed lines.

## Why (X-test-vm guide run, main aa185384)
- **N3:** `docs/user/03-keep-lodestar-safe.md` §3.7 (restore a backup) disagrees with the README.txt that the backup itself writes, in three places:
  - the README says run `lodestar reindex`, where the guide says delete `index/`;
  - the README says revoke sessions and create new API keys, where the guide says change the password, marked "(planned)";
  - the guide is missing the Compose `chown 65532:65532` step.

  The README is also still stamped 0.5.0.
- **N4:** §3.3 says the test shows "port unreachable". On a real host the reply was `host unreachable` (20 packets).

## Owns
- `docs/user/03-keep-lodestar-safe.md` §3.3 and §3.7 only.
- The backup README.txt text, wherever the backup code keeps it (find it; a text constant or template), plus any test that pins its text. Text only, no behaviour change.
- `docs/decisions/2026-11-15-D-guide-backup-fixes.md` (new) if a decision is needed.

Not: the gluetun rows and the G2 recipe in 03, or 06 (X-gluetun-restart-recover owns them).

## Steps
1. For each of the three N3 differences, read the code to find which is right for the current build: does `lodestar reindex` exist and do what the README says, and what can a user actually revoke today? Make the guide and the README agree on the correct steps, plainly, with the safe choice recommended.
   - If the README text lives in Rust code, change only the string, and its version stamp should come from the build version, not a literal. That small code change is allowed.
   - Add the Compose `chown 65532:65532` step where the guide restores into a Compose volume. Explain why in one line (the container runs as that user).
2. N4: describe the expected result as "host unreachable" or "port unreachable" (either means the block works), or whichever the code's own check accepts.
3. No other edits.

## Rules
- `npm run check` only if you touch `web/`. Rust: fmt, clippy `-D warnings`, the touched crate's tests.
- Follow `.claude/skills/steward/SKILL.md`. Message the runner "ready <PR> <sha>". The runner merges.
