# X-wk18w-rollback decisions

A way forward after a rolled-back join or a leftover coordinator folder in the Windows installer's "Join an existing Lodestar as a worker" mode (follow-up to WK-18w, #802; brief `briefs/X-wk18w-rollback.md`). Affects architecture §11 (Windows packaging) and §19.6 (joining).

## A rolled-back join: a rollback action, not only the recovery message
- **The choice.** A join can succeed and the install still be undone (StartServices fails, a later step fails, or the user cancels). The credentials then stay in `C:\ProgramData\Lodestar`, and every retry is refused as already joined. #802's recovery message tells the user to delete the folder by hand. A rollback action makes that unnecessary in the common case, so this slice adds one. The message stays, for when the rollback couldn't run.
- **What it removes: only what this setup made.**
  - `join-worker.ps1` writes `cluster\joined-by-setup` right after a successful `--join-only`, and only when the folder hadn't joined before.
  - The rollback (`join-finish.ps1 -Step rollback`) acts only when that marker exists. It then removes `cluster\credentials`, `cluster\coordinator`, `cluster\worker.lock` (and their `.tmp` siblings), and the marker last.
  - `secrets.key` stays: it seals nothing once the credentials are gone, and the next join reuses it. Anything a worker run may have left (the spool, the DHT state) stays too.
- **The commit removes the marker** (`join-finish.ps1 -Step commit`, `Execute="commit"`). So a later install's rollback never removes this install's join. The join also deletes a stale marker it finds, one left by a setup that was cut off before its commit or rollback, so a rollback can only ever remove its own setup's join.
- **A marker file, not a property.** A deferred action can't set a property that a later rollback action reads: rollback and commit actions see only the CustomActionData fixed when the script was built. The marker in the data folder (SYSTEM, Administrators and the service only) is the record that survives to the rollback.
- **Scheduled before JoinWorker, not after it (a departure from the brief).** Windows Installer runs a rollback action only if it was already in the script before the failure. Scheduled after JoinWorker, it would miss a failure inside JoinWorker itself, such as a join that succeeded and a hand-off to the service that then failed. Before it, the rollback covers that and every later failure. With no marker (no join, or a join that failed), it does nothing.
- **Safe to run as SYSTEM.** It follows no link or junction: a data folder, `cluster` folder, marker or file that is a reparse point is left alone. It deletes only those fixed names. It never sees the token. It always exits 0 and is `Return="ignore"`, so it can't stop a rollback.
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

## Tests
`test-join-worker.ps1` covers:
- the leftover coordinator folder (refused before lodestar runs, with no `--data` or `REMOVE_DATA` advice);
- the marker: written for this setup's join and before a failed hand-off, never for a folder that had already joined;
- the rollback: credentials and address gone, the key kept;
- the commit: the marker gone, and a rollback afterwards removes nothing;
- a rollback on an already-joined folder: nothing removed;
- the hand-off message;
- the pasted-token cases, including the script's own check.

There is still no MSI-level test of a rollback: that needs a join against a coordinator on the runner.
