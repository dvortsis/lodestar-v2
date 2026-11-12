# WK-18w: the Windows installer can join an existing Lodestar as a worker

**Runner brief, 2026-11-12 (UTC).** Opus, /security-review (it handles a join token). Split from WK-18, which found the original WiX step wrong; the WK-18 session (session_01MMjhn8ht4VBbYS74uVp94c) records why in its decisions file.
- **Branch:** `slice/WK-18w` from fresh `origin/main`.
- **PR title:** `WK-18w: Windows installer option to join an existing Lodestar as a worker`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## The facts WK-18 established (check them in the code)
- `lodestar worker --join <url> --token-file <path|->` always joins, and it never deletes the token file (`crates/lodestar/src/cli/worker.rs`, `read_join_token_file`).
- After a successful join, the worker runs from its saved credentials with plain `lodestar worker`.
- The WiX source is `packaging/windows/main.wxs`, with `.ps1` helpers. `packaging/wix/` doesn't exist.

## What to build
1. **An install-time choice in the MSI:** "This PC runs Lodestar" (the default, unchanged) or "Join an existing Lodestar as a worker". The second asks for the coordinator's address and the join token.
   - The token field is a password-style property.
   - It's `Hidden` / `MsiHiddenProperties`, so the token never appears in the MSI log.
   - It's never written to the registry and never persisted in the installed product.
2. **Joining is a one-shot deferred custom action, run as SYSTEM.** It:
   1. writes the token to a temporary file in a folder ACL'd to SYSTEM and Administrators only (reuse the `secure-datadir.ps1` pattern);
   2. runs `lodestar worker --join <url> --token-file <file>` once, with a bounded timeout;
   3. deletes the file whatever the outcome (in a `finally`), and checks it's gone;
   4. on success, installs the service as plain `lodestar worker`, with no token arguments, under the existing service account and SID rules;
   5. on failure, rolls back the install with a clear, token-free message. The coordinator's address may appear in it; the token never does.
3. **No rejoin on restart.** The service never carries `--join` or `--token-file`. A repair or upgrade keeps the saved credentials and doesn't ask for a token again.
4. **The default mode is untouched.** Diff `main.wxs` minimally. Keep the "This PC runs Lodestar" install byte-for-byte equivalent in behaviour, and prove it with the existing `smoke.ps1`.

## Tests
- Pester or PowerShell tests for the helper:
  - the file is ACL'd before the token is written;
  - it's deleted on success, on failure and on timeout;
  - the token never appears in output or logs (pass a sentinel token and grep for it).
- CI: use the repo's Windows MSI build job and the `smoke.ps1` path (find them in `.github/workflows/`). Add a join-mode smoke run against a local coordinator if the existing Windows CI can start one. If it can't, say so and stop at a build-plus-helper-tests level.
- No fixed sleeps: waits are conditions with ≥ 10 s deadlines.

## Owns
- `packaging/windows/main.wxs` (the minimal mode switch), a new `packaging/windows/join.wxs` (or a fragment), `packaging/windows/join-worker.ps1` and its tests;
- the Windows section of the worker setup guide in `docs/user/**` (use the `user-docs` skill);
- `docs/decisions/2026-11-12-WK-18w.md`.

Ask the runner before touching `crates/**` or `.github/workflows/**`.

## Never
- The token is never logged, persisted or echoed, and never put on a command line: always `--token-file`.
- Nothing runs against the owner's machines or cloud accounts.

## Done
- the repo's PowerShell and WiX checks on the changed paths;
- a /security-review of your own diff;
- send the runner `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green.

**Session window (owner, 2026-11-12):** sessions are cut back at 00:00Z on 2026-11-13. If the runner keeps this session, carry on. If not, push your work in progress, open the PR as a draft and report its state.
