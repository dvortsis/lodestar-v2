# X-wk18w-rollback

Runner brief, 2026-11-13. Sonnet, helper: the Windows installer. A follow-up to #802 (WK-18w, merged 665935cf). Run a security self-review before ready: this is the join-token path.

## Why
The join path has two cases left where the user hits a dead end, plus the optional items from rv-802 rounds 1 and 2.

## Must have
1. **A coordinator's folder left by a plain uninstall.** Someone installs Lodestar normally, uninstalls it (the data folder is kept by default), then installs again as a worker. lodestar refuses with "is a coordinator's data folder… --data" (`crates/lodestar/src/cli/worker.rs`), which an installer user can't act on.
   - In `join-worker.ps1`, map that refusal to an installer message: back up or delete `C:\ProgramData\Lodestar` as an administrator (it holds the old library), then run setup again. It stays a failure.
   - Test it with the stub.
   - Fix the decisions file wording (`docs/decisions/2026-11-12-WK-18w.md`, lines near 33 and 50) and the WK-19 note so they say to delete or back up the folder, never "uninstall with REMOVE_DATA=1" when nothing is installed.
2. **A rolled-back join.** The join succeeds, then StartServices fails or the user cancels, so the install rolls back and the credentials stay behind.
   - Decide, and record in a new decisions file, whether a rollback custom action should remove `cluster\credentials` (and `secrets.key`) when this install created them, or whether the recovery message (#802 I2) is enough.
   - If you add the rollback action: Execute="rollback", Impersonate="no", scheduled right after JoinWorker, and it removes only files this install created. Record which in a property set by the deferred action. Test it.
3. **The failed hand-off message** (`test-join-worker.ps1` around :257 and the script's message) should give the recovery steps directly, not "install again".

## Optional (from rv-802 round 2)
- Pin the work folder's parent to `%SystemRoot%\Temp` (or the folder the action uses) inside the script.
- Give the action deadline in tests more margin: 30 s action against a 90 s script deadline.
- The pasted-token test: use a different `lsj1` value for the address than for the token, so it catches a scrub-instead-of-omit regression. Add a case that reaches the script's own `lsj1` check.
- Refuse an address whose host part starts with `lsj1`, including `https://lsj1…`.

## Owns
`packaging/windows/join.wxs`, `packaging/windows/join-ca/**`, `packaging/windows/join-worker.ps1`, `packaging/windows/test-join-worker.ps1`, `packaging/windows/main.wxs` (only if a rollback action needs scheduling), `docs/decisions/2026-11-12-WK-18w.md`, `docs/decisions/*-X-wk18w-rollback.md`.

## Process
Branch `slice/X-wk18w-rollback`; PR title `X-wk18w-rollback: Windows installer: a way forward after a rolled-back join or a leftover coordinator folder`. Follow `.claude/skills/steward/SKILL.md`. The MSI job in CI runs the join tests. When CI is green, including that job, message the runner `ready <PR> <full sha>`. Tests use condition waits with deadlines of at least 10 s, never fixed sleeps. No cloud or AWS calls.
