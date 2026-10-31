# Branch clean-up (owner-approved 2026-10-31)

This deletes 155 remote branches:
- the 153 that descend from the old root `15620683`, the history replaced on 2026-10-30;
- `next`, which is merged into `main`;
- `measure/windows-nrt`.

The list is in `branch-cleanup-list.txt`. Nothing on the new `main` lineage is touched except those last two.

**Before you start:** your bundle backup of every ref from 2026-10-30 keeps the old history recoverable. The paused slices are saved under `ports/` on `runner-state`.

Run this from any clone of the repo where you can push. It deletes 25 branches per push. Every name is checked first, so `main`, `runner-state` and `release/v0.5.1` can never be deleted.

## PowerShell
```powershell
git fetch origin runner-state
$names = git show origin/runner-state:notes/branch-cleanup-list.txt | Where-Object { $_ -and $_ -notmatch '^(main|runner-state|release/v0\.5\.1)$' }
for ($i = 0; $i -lt $names.Count; $i += 25) {
  git push origin --delete @($names[$i..([Math]::Min($i + 24, $names.Count - 1))])
}
```

## bash
```bash
git fetch origin runner-state
git show origin/runner-state:notes/branch-cleanup-list.txt \
  | grep -vxE 'main|runner-state|release/v0\.5\.1' \
  | xargs -n 25 git push origin --delete
```

**Result:** `git ls-remote --heads origin | wc -l` drops from 252 to 97 (fewer if some were already gone). Tell the runner when it's done. It checks for leftovers and records the clean-up.
