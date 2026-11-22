# X-msi-upgrade-env-value (runner brief, 2026-11-22Z)

Kind: helper (packaging and docs). No deps. This comes from the Windows re-test on 2026-11-20: a major upgrade by MSI may drop the Lodestar service's `Environment` registry value (`HKLM\SYSTEM\CurrentControlSet\Services\<service>\Environment`), which holds per-service variables.

## Why it matters now
`docs/user/04-install-on-windows-and-linux.md:367` says "A setting locked by a `LODESTAR_UPDATES*` variable shows 'Set by …'". Nothing in the Windows guide says how such a variable is set on Windows, or whether an upgrade keeps it. A user who sets one on the service, the usual Windows way, could silently lose it on upgrade.

## Step 0 (before building): answer these, with file:line evidence
1. Does `packaging/windows/main.wxs` install the service so that a major upgrade removes and recreates it? Look at the ServiceInstall element, MajorUpgrade scheduling and RemoveExistingProducts. Would the `Environment` value survive?
2. Which is the safer, simpler fix for the user?
   - (a) The MSI keeps the value across an upgrade: WiX only, no custom action that writes secrets.
   - (b) The Windows guide says how variables are set for the service, that an upgrade resets them, and how to set them again. Or it recommends the in-app setting instead, if every such variable has one.
3. If (a), how is it tested? The MSI and portable job in CI builds the MSI. Can `smoke.ps1` or a WiX validation step check it? X-test-vm can test a real upgrade later, if you say what to check.
Then wait for my answer.

## Rules
- No change to the service's account, its ACLs or the data directory's security. If a fix seems to need one, stop and tell me.
- Docs follow `.claude/skills/user-docs/SKILL.md`.

## Owns
- `packaging/windows/**`
- `docs/user/04-install-on-windows-and-linux.md`, the Windows variables paragraph only
- `docs/decisions/2026-11-22-X-msi-upgrade-env-value.md`

## Branch and PR
- Branch `slice/X-msi-upgrade-env-value`, cut from `origin/main`. PR title "X-msi-upgrade-env-value: MSI upgrade keeps the service's Environment registry value, or the docs never point there".
- Gates: whatever the PR touches (`npm run check` in `web/` doesn't apply), plus the user-docs gates.
- Push once, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
