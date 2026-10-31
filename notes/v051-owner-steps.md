# v0.5.1: your steps (owner)

Runner, 2026-10-30. The release branch `release/v0.5.1` is cut at `c3f6341a`. The version bump (#206) and the release notes (#201) merge into it once their review and CI pass. The runner then runs every workflow on the branch. **Start at step 1 only when the runner says the branch is ready.**

## 1. CR-0 memory gate: done
You accepted CR-0 on #189. Your hours 12–24 fit passed: the upper 95% bound was +0.32 MiB/h, against a bar of ≤ 2 MiB/h. #214 records the acceptance.

The Linux soak in step 3 is now **the binding memory gate**. It uses the same fit on anonymous private memory, and `analyze_cr0.py` computes it (#215).

## 2. Tag the release candidate: done, 2026-10-30 18:5xZ
**Status:** you rejected rc.1's release deployment (run 41062406135) and tagged `v0.5.1-rc.2` at `6be95525`. Release run 41110604990 builds it. rc.2 is the gate candidate. Approve the `release` environment when its package jobs wait, then do steps 3–5.

### What this step was (rc.2 was ready: `release/v0.5.1` at `6be95525`, all workflows green)
You tagged `v0.5.1-rc.1` (`8b67f269`), and its Release run is waiting for your approval. **rc.1 doesn't have the storage and cache gauges you asked for** in your ruling.

They're now on `release/v0.5.1` (#222). CI, E2E with all-themes, and Release are green on `6be95525`, and `web/` is unchanged since the cut, which was Web-green. Tag it:
```sh
git fetch origin release/v0.5.1
git tag -a v0.5.1-rc.2 origin/release/v0.5.1 -m "v0.5.1-rc.2"
git push origin v0.5.1-rc.2
```
Recommended: reject or leave the rc.1 run, and do steps 3–5 on rc.2.
The Release workflow builds:
- the image `ghcr.io/dvortsis/lodestar-v2:0.5.1-rc.2` (`:latest` doesn't move for a pre-release);
- the Linux tarballs, the MSI and the portable zip, with checksums, on a draft pre-release.

Approve the `release` environment when GitHub asks. The runner records every artifact hash in `docs/decisions/2026-10-30-v051-cut.md`.

## 3. Unraid: the 24 h soak, as an in-place upgrade
1. **Don't restart the old pre-CR-0 container.** The rc image replaces it.
2. **Back up with Lodestar stopped:** copy the whole appdata folder somewhere off the appdata share, for example `cp -a /mnt/user/appdata/lodestar /mnt/user/backups/lodestar-pre-0.5.1`. Don't use `lodestar backup` here: it needs a running Lodestar, and the old container must stay stopped. A cold copy of a stopped data folder is consistent. This copy is your rollback if the rc's migrations go wrong.
3. Change the container's image tag to `0.5.1-rc.2` and start it on the same appdata. Note the intensity preset it runs at.
4. Start the collector, following `docs/soak/tools/linux/README.md` ("Run it on Unraid"):
   - `--commit` is the rc's 40-character commit (`git rev-parse v0.5.1-rc.2^{commit}`);
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

## 6. Read the verdict
`analyze_cr0.py` now opens its report with a **Run** section and a `gate_evidence` line (#207, on main; use main's copy of the tool). The 24 h run must say `Gate evidence: yes`, and the gluetun leg must say `yes` with "(2 h leg)". The 24 h report's **Memory gate** section must say "meets the bar": the upper 95% bound of the slope over hours 12–24 is ≤ 2 MiB/h. A rehearsal, a short run or a missing `--intensity` says `no`, with the reason.

## 7. Hand back
Push the evidence the same way as `soak/cr0-final`: aggregates and hourly data only, no raw logs, databases or personal paths. Use one branch per run. The runner analyzes it, records the outcome and, if everything passes, asks you to tag `v0.5.1` on the same commit as the passing rc.


## Status, 2026-10-30 21:35Z
- **Artifacts: verified.** The minisig checks against `packaging/keys/lodestar-release.pub`, the four archives match `SHA256SUMS`, and the image digest `sha256:8abcaf52…` matches run 41110604990.
- **Unraid 24 h soak: running.** It started 21:07Z from a fresh copy of the cold Oct-22 backup, and the V3→V12 upgrade went through. The baseline runs 2026-10-31 03:37Z to 2026-11-01 03:37Z; then comes the gluetun leg (at least 2 h). Then send `analyze_cr0.py`'s aggregate outputs.
- **Windows portable: PASS.**
- **Windows MSI service: running** since 21:22Z, with the Defender exclusion active. Keep it running to about 04:30Z on 10-31, past the 03:22Z suppression end, then record the stored count and rate.

## 2026-10-31 04:32Z: rc.2 gate results (owner)
- Windows MSI service run: PASS. 7 h 10 m uptime, 69.7 min after suppression ended, Defender exclusion on, clean stop (2.89 s) and restart from snapshot, 17.0 stored/min after suppression, private memory 423.6 MiB, 353 handles.
- Windows portable zip: PASS.
- Unraid 24 h soak: baseline running from 2026-10-31T03:37:39Z to 2026-11-01T03:37:39Z, then the gluetun leg (at least 2 h). At sample 443: 10,131 torrents, 493 MiB private, 0 restarts.

## Unraid v051-rc2-main, owner report (2026-10-31, baseline +1.33 h)
- Sample 470, elapsed 28,229 s, health 200, metrics_ok true.
- 10,423 torrents (+1,460 since warmup began, 1 h 50 m: about 800/hour, 13.3/min).
- Working set 646.53 MiB, private 487.66 MiB (sample 443: 627.5 / 493.2).
- Baseline ends 2026-11-01T03:37:39Z, then the gluetun leg (≥ 2 h measured) on the same digest and volume.
