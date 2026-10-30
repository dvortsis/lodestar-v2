# v0.5.1 release-cut runbook

Runner, 2026-10-30. Sources:
- `docs/decisions/2026-10-30-master-plan.md`, Part D;
- `docs/build-plan.md` §0.4;
- `.github/workflows/release.yml`. It accepts `vX.Y.Z[-prerelease]`; on a tag it builds the Docker image and attaches the packages to a DRAFT GitHub release; `latest=auto` keeps `:latest` off pre-releases.

## Entry conditions (all must hold before the cut)
- Merged to `main`, each with green CI on its head and a fresh-review MERGE:
  - L1–L7 (done 10-30);
  - F-auth #190;
  - F-filters #179/#192;
  - F-metainfo #177;
  - F-updater #198;
  - CR-0f #200.
- Also on `main`:
  - CR-COL #197 (done), so the Unraid soak can run;
  - F-status #202 (the honest-UI item, plan F8);
  - T-v051-gaps (targeted tests for delta items B2/E1).
- **Owner:** a ruling on the CR-0 memory gate (#189). If the ruling is "accept", the full-res h12–24 private-bytes fit has been published with upper CI ≤ 1 MiB/h.
- `main` is green on its last push run: CI, Release, Web, E2E.

## Cut (runner)
1. Pick the cutoff SHA, the current `origin/main` tip. Record it in `docs/decisions/<date>-v051-cut.md`, together with the delta mapping refreshed from `notes/v051-delta-draft.md`. The refresh covers `744b84d6..<cutoff>` and adds #196, #198, #199, #200 and #202.
2. Create `release/v0.5.1` from the cutoff SHA, then open a PR on it (or push it directly if the ruleset allows release branches; otherwise a PR). It does three things:
   - bumps the workspace `Cargo.toml` `version = "0.5.0"` to `"0.5.1"`, and regenerates `Cargo.lock` with `cargo update -w`;
   - regenerates `web/src/lib/api/openapi.json` `info.version` with the repo's openapi tool (`cargo run -p lodestar --bin openapi`), never by hand;
   - merges the release notes, D-v051 #201, with its "Before release" checklist resolved.
3. Dispatch every workflow on `release/v0.5.1`: CI (workflow_dispatch runs everything, macOS included), Release, Web, and E2E with all-themes (no `update_snapshots`). All must be green, and no run may have regenerated baselines.
4. Owner pushes `v0.5.1-rc.1` on the release branch head. The Release workflow then builds:
   - `ghcr.io/dvortsis/lodestar-v2:0.5.1-rc.1`;
   - the Linux tarballs, the MSI and the portable zip, with checksums, on a draft pre-release.

   Record every artifact hash in the cut decisions file.

## Gate on the rc artifacts (owner hardware)
- **Unraid Linux Docker soak, 24 h measured.** Run `docs/soak/tools/linux/collect.py` on the rc image, as an in-place upgrade of the 22 October volume after `lodestar backup`. This is also the re-soak that CR-0f's bootstrap/pause change needs (delta class A).
- **Gluetun leg:** a separate run with at least 2 h measured, using `--leg gluetun --require-gluetun`.
- **Analysis:** both through `analyze_cr0.py`. Until CR-COL-2 lands, read `settings.leg` and `settings.rehearsal` from `run.json` by hand.
- **MSI smoke:** at least 6 h in service mode on the owner's PC. Check service start and stop, a clean shutdown and restart, and a non-zero stored rate. Record whether a Defender exclusion was active.
- **Portable zip smoke.**
- **Never** restart the old pre-CR-0 Docker crawler. The rc image replaces it.

## Release
- If a gate fails: triage, fix PR, a new rc (`-rc.2`), and the affected gates rerun on the new artifact.
- If all gates pass: the owner tags `v0.5.1` on the same SHA as the passing rc, approves the release environment, and publishes the draft. Then the runner:
  - records the outcome in the decisions file and in the build-plan §0 status;
  - cherry-picks or merges the release-branch version bump back to `main`.
