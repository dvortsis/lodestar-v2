# X-wk18w-rollback decisions

A way forward after a rolled-back join or a leftover coordinator folder in the Windows installer's "Join an existing Lodestar as a worker" mode (follow-up to WK-18w, #802; brief `briefs/X-wk18w-rollback.md`). Affects architecture §11 (Windows packaging) and §19.6 (joining).

## A rolled-back join: a rollback action, not only the recovery message
- **The choice.** A join can succeed and the install still be undone (StartServices fails, a later step fails, or the user cancels). The credentials then stay in `C:\ProgramData\Lodestar`, and every retry is refused as already joined. #802's recovery message tells the user to delete the folder by hand. A rollback action makes that unnecessary in the common case, so this slice adds one. The message stays, for when the rollback couldn't run.
- **What it removes: only what this setup made.**
  - `join-worker.ps1` writes `cluster\joined-by-setup` as soon as lodestar has finished, whenever this setup created the credentials (also if lodestar then failed, say saving the address), and never when the folder had joined before.
  - The rollback (`join-finish.ps1 -Step rollback`) acts only when that marker exists. It then removes `cluster\credentials`, `cluster\coordinator`, `cluster\worker.lock` (and their `.tmp` siblings), and the marker last.
  - `secrets.key` stays: it seals nothing once the credentials are gone, and the next join reuses it. Anything a worker run may have left (the spool, the DHT state) stays too.
- **The commit removes the marker** (`join-finish.ps1 -Step commit`, `Execute="commit"`). So a later install's rollback never removes this install's join.
- **A stale marker goes before this install's rollback exists.** A marker can outlive its setup if the commit never ran: the power failed, msiexec was killed, or the commit was blocked (it's `Return="ignore"`). A deferred prepare step (`join-finish.ps1 -Step prepare`, `Return="check"`) runs before the rollback is put in the script and removes such a marker. If it can't, the install stops before anything is joined. Without it, a setup that failed early (a mistyped token, say) would roll back and remove the earlier setup's join. The join script removes a stale marker too, in case.
- **A marker file, not a property.** A deferred action can't set a property that a later rollback action reads: rollback and commit actions see only the CustomActionData fixed when the script was built. The marker in the data folder (SYSTEM, Administrators and the service only) is the record that survives to the rollback.
- **Scheduled before JoinWorker, not after it (a departure from the brief).** Windows Installer runs a rollback action only if it was already in the script before the failure. Scheduled after JoinWorker, it would miss a failure inside JoinWorker itself, such as a join that succeeded and a hand-off to the service that then failed. Before it, the rollback covers that and every later failure. With no marker (no join, or a join that failed), it does nothing.
- **Safe to run as SYSTEM.** It follows no link or junction: a data folder, `cluster` folder, marker or file that is a reparse point is left alone. It deletes only those fixed names. It never sees the token. It always exits 0 and is `Return="ignore"`, so it can't stop a rollback.
- **Repair, upgrade and reinstall.** The prepare, rollback and commit steps run under the join's own condition: a worker-mode first install only (`NOT Installed AND NOT WIX_UPGRADE_DETECTED`). A Repair or upgrade, also the updater's silent one, never joins and runs none of them, so it can never remove a join. A fresh install over a folder that has already joined (an uninstall that kept the data) finds the credentials: it writes no marker, lodestar refuses (exit 3), setup says how to start over, and the rollback removes nothing. Tests: `fail -Joined` and `ok -Joined`.
- **If commit steps can't run** (for example, rollback is disabled by policy), the marker stays in a working worker's folder. That's harmless: the next setup's prepare step removes it.
- **Left to the user: the coordinator's record.** The rollback can't remove the worker from the main Lodestar, which the token was spent on. The failure messages say to remove it in Settings, Workers before joining again.

## Messages
- **A coordinator's folder left by a plain uninstall.** Someone installs Lodestar normally, uninstalls it (the data folder is kept by default), then installs again as a worker. lodestar refuses "a coordinator's data folder … --data", which an installer user can't act on. `join-worker.ps1` now finds `lodestar.db` before any token file or process. It also maps lodestar's own refusal, if one gets through. Either way it says to back up `C:\ProgramData\Lodestar` if its library is wanted, delete it as an administrator, and run setup again. It is still a failure.
- **A failed hand-off to the service** now says what happens and what to do: setup is undone and removes the files, so remove the worker on the main Lodestar and run setup again with a new token. It no longer says "install again". Its join is marked before the hand-off, so the rollback does remove the files.
- **Never `REMOVE_DATA=1` when nothing is installed.** The WK-18w decisions file and its notes for WK-19 now say to delete or back up the folder in that case. Uninstalling with `REMOVE_DATA=1` stays the advice only while Lodestar is installed.

## The optional items from rv-802
- **The work folder's parent is pinned.** The script refuses work folders outside `%SystemRoot%\Temp`, the folder the custom action uses. A test passes its own folder (`-TestTempRoot`), and the MSI never does.
- **Wider test deadlines:** the custom action's 30 s deadline against the script's 90 s.
- **The pasted-token cases** use a second token-shaped value for the address. They check it appears nowhere, not even as `[join token]`, so a scrub where the address should be left out would be caught. One case runs the script directly, so its own check is tested without the custom action in front of it.
- **An `lsj1` host is refused,** by the action and the script, also after `https://`.

## Files outside the brief's owns (the runner's call)
- `packaging/windows/join-finish.ps1` is new. The rollback, commit and prepare steps can't live in `join-worker.ps1`, which needs the token on stdin and the join's arguments. It's installed with the join scripts (`join.wxs`) and never sees the token.
- `packaging/windows/README.md`: its layout line names the new script.

## Tests
`test-join-worker.ps1` covers:
- the leftover coordinator folder (refused before lodestar runs, with no `--data` or `REMOVE_DATA` advice);
- the marker: written for this setup's join and before a failed hand-off, never for a folder that had already joined, also when lodestar succeeds there;
- a stale marker: removed by the join and by the prepare step, the earlier credentials untouched, and a rollback then removes nothing;
- junctions: a `cluster` folder or a data folder that is a junction is never followed by any step;
- the rollback: credentials and address gone, the key kept;
- the commit: the marker gone, and a rollback afterwards removes nothing;
- a rollback on an already-joined folder: nothing removed;
- the hand-off message, and the rollback removing that join's files;
- the pasted-token cases, including the script's own check.

There is still no MSI-level test of a rollback: that needs a join against a coordinator on the runner.
