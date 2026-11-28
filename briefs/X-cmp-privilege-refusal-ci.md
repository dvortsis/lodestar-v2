# X-cmp-privilege-refusal-ci (Opus; runner brief 2026-11-28)

**Branch:** `slice/X-cmp-privilege-refusal-ci`. **PR title:** `X-cmp-privilege-refusal-ci: CI asserts that companions never start as root or an administrator`.

## Why
`crates/companions/src/supervise/process.rs` `spawn` refuses to start a companion while Lodestar runs as root or an administrator (`SpawnError::Privileged`; `allow_privileged` is honoured only with `cfg(any(test, feature = "testing"))`). The only test of this, `root_is_refused_unless_a_test_allows_it` (`crates/companions/tests/supervise.rs`), is `cfg(unix)` and asserts only when run as root. CI's Linux runners aren't root and the Windows runners are administrators, so **no CI run asserts the refusal on either OS**. CMP-G's review (#1161) found this. The gap predates CMP-G.

## Task
Make CI prove the refusal on both OSes, without loosening it.
1. **Windows.** GitHub's `windows-latest` runners are administrators. Add a test that, when `GITHUB_ACTIONS` is `true` on Windows, asserts that `process::privileged()` is `Ok(true)` (so an environment change can't quietly turn the test into a no-op), and that `spawn` with `allow_privileged: false` returns `Err(SpawnError::Privileged)` and starts nothing. Outside CI it asserts the refusal whenever `privileged()` is true.
2. **Linux.** Make the refusal run as root in CI. The likely way is one step in the Linux test job that runs only this test's prebuilt binary under `sudo -n` with `--exact`, with the test asserting `geteuid().is_root()` when a marker env var says it's the root run. If you find a cleaner way, propose it first. **`.github/workflows/ci.yml` is not granted yet: send the runner the exact step you want before touching it.**
3. Release builds: `allow_privileged` is ignored without `testing`. Keep that, and don't add any new test-only override to `process.rs`. If you think product code must change, ask the runner first.

## Owns
- `crates/companions/tests/supervise.rs` (the privilege tests only)
- `docs/decisions/2026-11-28-X-cmp-privilege-refusal-ci.md`
- `.github/workflows/ci.yml`, one step, only after the runner grants it.

Ask the runner before touching anything else. `crates/companions/tests/gate.rs` belongs to CMP-G (#1161, open): don't edit it.

## Rules
- This is security-sensitive: run `/security-review` on the diff.
- No fixed sleeps. Condition waits have deadlines of at least 10 s.
- No network beyond localhost, and no cloud calls. Never touch the owner's machines.
- No model identifiers in commits, PRs or code. UTC `Z` times only. Never merge.

## Done
- CI green on Linux and Windows.
- The PR description shows the refusal assertion ran on both OSes: quote the CI test lines.
- Self-review per `.claude/skills/steward/SKILL.md`.
- Send the runner `ready <full sha>` with the claude-code-remote send_message tool (session_id session_01EYhgh4JX3AmHzKXPjYCoUg).
