# v0.5.2-rc.3: your soak steps (owner)

These runs are v0.5.2's own gate (manifest §5.2, §5.3, §6, §9.4). rc.3 is `release/v0.5.2` at `114bb625a43af24602d8c9e7148c198f068245c3`: rc.2 (`ec40f10c`) plus #740 (tokio 1.54.1, and the notice line) and #751 (release notes only). No long run used rc.1 or rc.2, so every run below uses rc.3.

**You roll back to the published v0.5.1** (`12cd25ca`, rc.4 plus #558), the build users have:
- Docker: `ghcr.io/dvortsis/lodestar-v2:0.5.1@sha256:2ffce5cb9988b3173a46aeafb2a62e24cf66962da39d3286da0adbbd5d864c17`. This is the registry's digest for `0.5.1`, checked 2026-11-12. rc.4's digest is a different one, `4d9d611a…`.
- Windows: the v0.5.1 release MSI, `lodestar-0.5.1-x86_64.msi`, SHA-256 `a942f6344c0ad8a4189bcbf514ebfd905d4bdf8c51b03750a0ba9bfd8c8c4072`. This is from the release's `SHA256SUMS`, whose minisign signature verifies against `packaging/keys/lodestar-release.pub` (checked 2026-11-12).

**Start only when the runner says the tag's Release run is green and the artifacts are verified.**

There are four legs:
- the Docker upgrade and rollback check (a), then the 24 h Docker soak (b), both on copies of a real v0.5.1 folder;
- the Windows upgrade and rollback check (c), then the ≥ 6 h Windows run on a fresh folder (d).

The two machines run independently.

## 0. Tag and verify (the runner sends the exact commands)
1. Tag `v0.5.2-rc.3`, annotated, on the head the runner names, and push it. It's a pre-release, so `:latest` doesn't move.
2. Approve the `release` environment when the package jobs wait.
3. The runner verifies the minisign signature (`packaging/keys/lodestar-release.pub`), `SHA256SUMS` and the image digest of `ghcr.io/dvortsis/lodestar-v2:0.5.2-rc.3`. Use only verified artifacts below.

## 1. Before any clock starts (every leg)
- **Back up first:** make a cold copy of the v0.5.1 folder, with Lodestar stopped, kept outside the appdata share (`cp -a`). This is your way back.
- **Finish setup:** on a fresh folder nothing crawls (no DHT, scrape or fetch) until `POST /setup`. A run's clock starts after setup.
- **Accept Legal Notice v4:** sign in once and accept it. Until then every session route answers `409 notice_required`, and the Linux collector stops with that reason rather than recording gaps.
- **Record:** the intensity preset, whether a Defender exclusion is active (Windows), and the pack state (Filters & Safety → Packs).

## 2. Docker (Unraid): upgrade smoke and rollback, on a copy (manifest §5.2, §5.3)
Use a **copy** of a v0.5.1 data folder. The r5 soak volume from rc.4 is fine: rc.4 (`dba1a5c0`) and v0.5.1 (`12cd25ca`) both end their migrations at V12, so the schema version is 12 either way. Any folder with ≥ 1M torrents is best. Never use the live folder.
1. Record:
   - the store's schema version, `SELECT max(version) FROM schema_migrations`, which must be 12;
   - that the index directory is `v5-*`;
   - the torrent count;
   - one search and its result counts.
2. Start rc.3 on the copy. Check, in order:
   1. `<db>.pre-v13.db` exists before V13 runs, with schema version 12;
   2. the schema version reaches 14;
   3. the old index serves read-only while v7 builds;
   4. `search.swapped` is logged, with `disk_high_water_bytes`;
   5. re-classification finishes (`CLASSIFY_VERSION` 4);
   6. the pack state matches step 1, packs without a row read off, and `unauthorised-copy` is off;
   7. the notice v4 shows once after login;
   8. delete one torrent and record one hold, and write both down.
3. **Pass:** every check holds, and the counts match step 1 apart from torrents that `CLASSIFY_VERSION` 3 → 4 re-classifies as adult.
4. **Rollback:**
   1. Stop rc.3.
   2. Run `lodestar rollback v13 --yes` with the rc.3 binary. Check that the live store is copied to `lodestar.before-rollback-<time>.db`, the index is moved to `index.before-rollback-<time>`, and the restored store reports schema version 12.
   3. Start the **published v0.5.1** image (pinned digest above) on the folder. It must start, rebuild its v5 index and serve search. It must **not** show a notice:
      - `lodestar rollback` restores only the store and the index, so `auth.json` keeps the v4 you accepted under rc.3;
      - v0.5.1's notice is version 3, and it asks only when the recorded version is missing or lower (`notice_pending`, `crates/api/src/auth.rs`: `notice.is_none_or(|v| v < 3)`);
      - sign-in works as usual.
   4. **Pass:** v0.5.1 serves step 1's torrents less the deleted one, and the hold still holds.

## 3. Docker (Unraid): the 24 h soak on an upgraded folder (the binding memory gate)
1. Make a **new** copy of the v0.5.1 folder, not step 2's rolled-back one.
2. Start rc.3 and let the upgrade finish: V14, the v7 rebuild and re-classification. At 10M torrents on 4 cores that's about 2 h, and it needs about twice the index's disk.
3. Accept the notice, then **turn every pack on** and record the pack state.
4. Record the `filter.sp4.sigs.%` row count now, at hour 12 and at the end. No gauge reports it yet.
5. Start the Linux collector from the **release branch's** `docs/soak/tools/linux/collect.py`, following its README ("Run it on Unraid"), with:
   - `--commit`: rc.3's 40-character commit (`git rev-parse v0.5.2-rc.3^{commit}`);
   - `--intensity`: the preset from section 1;
   - a **new** `--root`;
   - `--screen-thresholds docs/soak/screen-thresholds.json`;
   - `--storage-path` for appdata;
   - `--stop`;
   - optionally `--password-file` (`chmod 600`, loopback `--url`) to record `build_target`. Without it, the analysis takes `--c-allocator mimalloc`.
6. It runs 6 h of suppression, 30 min of warm-up and 24 h measured: about **30.5 h**. Leave it alone.
7. Watch:
   - `fetch_held_bytes` + `fetch_reserved_bytes` ≤ 64 MiB;
   - `fetch_reserve_timeouts_total`;
   - `fetch_held_full_total`;
   - `lodestar_mem_anon_mappings`;
   - the zero-valued `lodestar_cluster_*` and `lodestar_remote_*` families.

## 4. Windows: the upgrade and rollback check (manifest §5.2 step 4)
Repeat step 2 on a **real v0.5.1 data folder** on Windows: a copy, or a machine you can roll back. The MSI check in `release.yml` doesn't count, because it runs one binary twice. This covers:
- V14 and the pre-v13 snapshot on NTFS;
- deleting the old index on NTFS;
- `lodestar rollback v13 --yes` on Windows;
- the **v0.5.1 release MSI** (SHA-256 above) on the rolled-back folder, with no notice shown, as in step 2.

The pass criteria are step 2's.

## 5. Windows: the ≥ 6 h service run on a fresh folder
1. Install the rc.3 MSI on a machine with **no** `C:\ProgramData\Lodestar`. Finish setup and accept the notice. Packs are on from the first start.
2. Run the Windows collector (`docs/soak/tools/cr0-soak.ps1`; `docs/soak/tools/README.md`) for ≥ 6 h after suppression ends.
3. Check service start and stop, a clean shutdown and restart, and a non-zero stored rate in Insights. Write down whether a Defender exclusion was active.
4. Smoke-test the portable zip: start it, search, stop.

## 6. Read the verdict
Use `analyze_cr0.py` from the release branch:
- the 24 h Docker run must say `Gate evidence: yes`;
- its Memory gate must say it meets the bar: the upper 95 % bound of the hours 12–24 slope ≤ 2 MiB/h on `anon`;
- state whether the SP-4 set reached its cap (17–26 MB), from the row counts.

The soak screen is an early warning only. Its known red memstats cost test doesn't count.

## 7. Hand back
- Push the evidence as before: aggregates and hourly data only (no raw logs, databases or personal paths), one branch per run.
- Report steps 2 and 4 pass or fail item by item.
- The runner then analyses, records the outcome and, if everything passes, asks you to tag `v0.5.2` on rc.3's commit.
