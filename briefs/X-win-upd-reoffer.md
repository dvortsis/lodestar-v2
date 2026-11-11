# X-win-upd-reoffer (Opus, core, /security-review): follow-ups from #677

Assigned by the runner on 2026-11-11, after #677 (X-win-upd-1920) merged as 74052431. The review is `reviews/rv-677-f4243e6c.md`.

## Do
1. **The re-offer loop** (#663 item 2, recorded as a residual in #677). A 0 exit while `from` (the old program) is still on disk must skip the version with a plain reason, not report `installed`. Rework the install tests to model the program on disk, so "installed" means the new version is the one on disk.
2. **A linear 1920 matcher** (`msi.rs:262-270`). Take `line.rfind(service)` once and match when `last >= at + AT.len()`, so the answer stays the same. Add a test that runs a 1 MiB adversarial line (`"a 1920. "` repeated) and asserts it finishes within a generous deadline in debug, and that its answer is unchanged.
3. **Saturating counters:** `helper.rs:716` `attempts + 1` and `rollback.rs:825/829` `j.blocked + 1` / `+= 1` use `saturating_add`. Add a test with u32::MAX on disk.
4. **The decisions file:** text injected into the log can at most turn a 1603 into 2 extra bounded retries (forward), or a halt into 2 more disabled-service reinstalls (rollback). Say why that's safe. Match the service name exactly (a word boundary), not `contains`, or record why not.

## Owns
`crates/lodestar/src/update/windows/helper.rs`, `msi.rs`, `rollback.rs`, `crates/lodestar/tests/update_windows_msi.rs`, `docs/decisions/2026-11-11-X-win-upd-reoffer.md`.
Not `check.rs` (PK-win-portable-upd #659 is open). The mode-Off/pause retry fix is a separate later slice on the service side.

## Bar
- fmt, clippy (workspace, `-D warnings`) and tests.
- Mutation-check each new guard. No fixed sleeps.
- Branch `slice/X-win-upd-reoffer`, PR `X-win-upd-reoffer: <title>`, steward skill.
- Reply "ready" with the head when CI is green, Windows included.
