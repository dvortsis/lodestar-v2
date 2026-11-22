# X-fuzz-advisories (runner brief, 2026-11-22Z)

Kind: helper (CI and a lockfile). Dep X-workflow-tests-filter (#1062) is merged. Recorded in `docs/decisions/2026-11-22-runner-records-r12.md` (#1068).

## The gap
- CI's `advisories` job (ci.yml:504-517, "RustSec advisories") runs `cargo deny --locked check advisories` on the root workspace only.
- `fuzz/` is its own workspace with its own `fuzz/Cargo.lock`, and nothing checks it. On main, `cargo deny` in `fuzz/` flags RUSTSEC-2026-0385 against ctutils 0.4.2 (fuzz/Cargo.lock:391), reached through `digest`.
- `ci_changes.py` `ADVISORIES` (:142) matches `Cargo.lock`, `deny.toml`, ci.yml and ci_changes.py, but not `fuzz/Cargo.lock`.

## Step 0 (send me, then wait for "go")
1. The advisory: what it is, whether a fixed ctutils (or a `digest` that drops it) exists, and the exact lock update that resolves it. Prefer `cargo update -p <crate> --precise <v>` in `fuzz/`, the smallest move. Say what else the update moves.
2. How `cargo deny` finds its config for `fuzz/`: which `deny.toml` applies, and whether the root's ignores and sources also apply there. If a fuzz-only config is needed, say so. `deny.toml` is the runner's to edit.
3. The CI change: one more step in the same job (e.g. `cargo deny --manifest-path fuzz/Cargo.toml --locked check advisories`, or whatever form step 0 shows works). The job's name, `if:`, permissions and triggers stay as they are. Plus `ADVISORIES` matching `fuzz/Cargo.lock`, with tests in `test_ci_changes.py`.
4. Files and an estimate.

## Rules
- No ignore of the advisory unless no fixed version exists. Then it goes in deny config with a reason and a review date, and I make that call.
- No change to any other job, permission, secret or trigger. Nothing that weakens the root check.
- Run the new step locally first. cargo-deny 0.20.2 is the version CI pins: `cargo install cargo-deny --version 0.20.2 --locked`.
- Never beside another slice that owns `ci_changes.py` or ci.yml's `advisories` job. The open, idle #632 also touches ci.yml; leave it alone.

## Owns
- `.github/workflows/ci.yml` (the `advisories` job only)
- `.github/scripts/ci_changes.py`
- `.github/scripts/test_ci_changes.py`
- `fuzz/Cargo.lock`
- `docs/decisions/2026-11-22-X-fuzz-advisories.md`

## Branch and PR
- Branch `slice/X-fuzz-advisories`. PR title "X-fuzz-advisories: CI checks the fuzz workspace's lockfile against RustSec too".
- Gates: `python3 -m pytest -q .github/scripts`, `cargo deny --locked check advisories` at the root and in fuzz/ (both clean), and `cd fuzz && cargo check --locked` (or the fuzz build CI uses) after the lock update.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
