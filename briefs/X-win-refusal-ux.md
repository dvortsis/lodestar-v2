# X-win-refusal-ux: Windows service refusal (Event Log reason, exact fix in the message, correct Repair advice)

**Runner brief, 2026-11-07. Model: Opus, with /security-review.** This slice runs in the P-v052-cut session, which knows the Windows packaging and the v0.5.x release. Follow the steward skill. Use branch `slice/X-win-refusal-ux` from origin/main, and title the PR `X-win-refusal-ux: <title>`.

## Background
In the rc.4 MSI check, the owner opened the data folder in Explorer and clicked "Continue". Explorer added an Allow entry for the owner's own account. The service then refused to start three times. The messages were confusing:
- Event 7024 rendered `ServiceSpecific(3)` as "The system cannot find the path specified".
- The suggested Repair is wrong for a non-empty folder.
- The reason wasn't in the Windows Event Log.

## Read first
- The `X-win-refusal-ux` entry in `runner/slices.yaml`. Its notes are the spec.
- `docs/owner-decisions.md` §2.52, the Repair rule: "The installer's Repair may re-secure a data folder whose only problem is an extra Allow entry for the administrator's own account, as Explorer's Continue adds… every other problem still refuses."
- `docs/decisions/2026-10-23-B2c.md` and the existing ACL checks in `crates/lodestar/src/windows/acl.rs`.
- The Windows chapters of `docs/user/`, and the `user-docs` skill for the troubleshooting text.

## Build
1. **Event Log.** Write the refusal reason to the Windows Event Log. Register the event source in the MSI (`packaging/windows/main.wxs`). The entry names:
   - the folder;
   - the extra account (its name and SID);
   - the exact `icacls` command that removes it.
   **Never log secrets.**
2. **A clear exit code.** Event 7024 should not read "The system cannot find the path specified". Consider a service-specific code with the customer bit set, or another way to make SCM show a meaningful message. Record what you chose and why.
3. **Repair advice:**
   - Correct the messages, so Repair is suggested only where it helps.
   - Implement §2.52: Repair re-secures a folder whose **only** defect is an extra Allow entry for the installing administrator's own account. It restores the installer's own access list.
   - Any other defect still refuses, with the exact fix named: for example, another account, a Deny entry, a changed owner, or broken inheritance.
   - Tests for each case, including "an admin ACE plus any other defect still refuses".
4. **`settings.logging.event_log_errors`:** today no code reads it. Wire it so it controls the Event Log writes, or hide it, and record which. Update the settings golden files if they change.
5. **Guide, chapter 4 (`docs/user/06-troubleshooting.md`, as in owns):**
   - explain the refusal, the `icacls` fix and when Repair helps;
   - explain the Windows Firewall "Public network" profile, without ever suggesting the user widen it.
   Follow the `user-docs` rules.

## Owns
As listed in the `slices.yaml` entry:
- `settings/logging.rs` and the two settings goldens;
- `windows/service.rs`, `windows/acl.rs`, `windows/temp_log.rs`;
- `packaging/windows/main.wxs`;
- `docs/user/06-troubleshooting.md`;
- plus `docs/decisions/2026-11-07-X-win-refusal-ux.md`.

PK-win-restart also owns `windows/service.rs` and `main.wxs`, so it stays queued until this merges. Anything else goes to the runner first.

## Gates
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar`, including the `cfg(windows)` tests, which run on CI's windows job;
- `packaging/windows/smoke.ps1` if you touch the MSI. The Release dry run (workflow_dispatch) proves the MSI builds; dispatch it on your branch and report the run.
- No fixed sleeps.
- About 600 lines.

Message the runner with the PR number and head SHA once CI is green, including Windows. A fresh Opus reviewer runs /security-review, then the runner merges. A backport to `release/v0.5.2` is the runner's call afterwards.
