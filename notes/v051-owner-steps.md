# v0.5.1: your steps (owner)

Runner, 2026-10-30. The release branch `release/v0.5.1` is cut at `c3f6341a`. The version bump (#206) and the release notes (#201) merge into it once their review and CI pass. The runner then runs every workflow on the branch. **Start at step 1 only when the runner says the branch is ready.**

## 1. Rule on the CR-0 memory gate (#189)
The soak's private memory grew by +3.24 ± 1.14 MiB/h from hour 1 on, but was flat over hours 12–24. Choose one:
- **Accept,** with the h12–24 fit as the evidence;
- **re-soak;**
- **or set a different bound.**

Without a ruling there's no rc tag.

## 2. Tag the release candidate
```sh
git fetch origin release/v0.5.1
git tag -a v0.5.1-rc.1 origin/release/v0.5.1 -m "v0.5.1-rc.1"
git push origin v0.5.1-rc.1
```
The Release workflow builds:
- the image `ghcr.io/dvortsis/lodestar-v2:0.5.1-rc.1` (`:latest` doesn't move for a pre-release);
- the Linux tarballs, the MSI and the portable zip, with checksums, on a draft pre-release.

Approve the `release` environment when GitHub asks. The runner records every artifact hash in `docs/decisions/2026-10-30-v051-cut.md`.

## 3. Unraid: the 24 h soak, as an in-place upgrade
1. **Don't restart the old pre-CR-0 container.** The rc image replaces it.
2. **Back up with Lodestar stopped:** copy the whole appdata folder somewhere off the appdata share, for example `cp -a /mnt/user/appdata/lodestar /mnt/user/backups/lodestar-pre-0.5.1`. Don't use `lodestar backup` here: it needs a running Lodestar, and the old container must stay stopped. A cold copy of a stopped data folder is consistent. This copy is your rollback if the rc's migrations go wrong.
3. Change the container's image tag to `0.5.1-rc.1` and start it on the same appdata. Note the intensity preset it runs at.
4. Start the collector, following `docs/soak/tools/linux/README.md` ("Run it on Unraid"):
   - `--commit` is the rc's 40-character commit (`git rev-parse v0.5.1-rc.1^{commit}`);
   - `--intensity` is the preset from step 3;
   - use a new `--root` run directory;
   - add `--stop` so the final clean shutdown is part of the evidence.
5. It takes 6 h of startup suppression, plus 30 min of warm-up, plus 24 h measured: **about 30.5 h in total.** Leave it alone.

## 4. Unraid: the gluetun leg (separate, at least 2 h measured)
After step 3 ends, recreate Lodestar under gluetun (`--network container:gluetun`). Then run the collector command from the README's "gluetun leg" section, with `--leg gluetun --require-gluetun --baseline-hours 2` and its own run directory. It takes about 8.5 h in total.

## 5. Windows: the MSI in service mode (at least 6 h)
Install the rc MSI and let the service run for at least 6 h. Check:
- service start and stop;
- a clean shutdown and restart;
- a non-zero stored rate in Insights.

Write down whether a Defender exclusion was active. Then smoke-test the portable zip: start it, search, stop.

## 6. Hand back
Push the evidence the same way as `soak/cr0-final`: aggregates and hourly data only, no raw logs, databases or personal paths. Use one branch per run. The runner analyzes it, records the outcome and, if everything passes, asks you to tag `v0.5.1` on the same commit as the passing rc.
