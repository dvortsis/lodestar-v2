# PK-manifest: release.yml sets LODESTAR_VERSION in every build and publishes the signed format-1 update manifest per channel

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review` (it signs with the release key). **Branch:** `slice/PK-manifest` from `origin/main`. **PR title:** `PK-manifest: <title>`. **Steward:** `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

Runner brief, 2026-11-08T15:10Z. Slot 2. M6 critical path: PK-manifest → PK-win-upd-1 → PK-win-upd-2/3 and PK-smoke → G-m6 → the v0.6 tag.

## Read first
- `docs/plan/2026-11-06-m6-gap-audit.md`: §2.5 (the two **Missing** rows), §3 ("the signed manifest format doesn't change"), §4.3's **PK-manifest** bullet (the spec), §5 **Q8** (the nightly key), §6.
- `crates/update/README.md` (format 1, the caps) and `crates/update/src/doc.rs` (read-only: **no format change**).
- `.github/workflows/freshness.yml`: it reads one `<channel>.json` envelope per channel from `update-manifest` and re-signs it. Your output must be exactly what it reads.
- `docs/decisions/2026-10-30-X-freshness-hardening.md`, `docs/decisions/2026-10-23-B-update-r2.md` (lines ~127, 150: the `LODESTAR_VERSION` follow-up), `packaging/keys/README.md`.
- `crates/lodestar/build.rs` (~66, ~123): how `LODESTAR_VERSION` is read.
- `docs/owner-decisions.md` §2.21 (keys only in their environments).
- `runner/standards.md` is the bar.

## Owns
- `.github/workflows/release.yml`
- `packaging/docker/Dockerfile` (the `LODESTAR_VERSION` build arg only)
- `.github/scripts/update_manifest.py` and `.github/scripts/test_update_manifest.py` (new)
- one `docs/decisions/2026-11-08-PK-manifest.md`
- **Ask the runner first** for anything else (a CI job that runs the round-trip check through `lodestar-update`'s verifier may need a few lines in `ci.yml` or a small test in `crates/update/tests/`; ask, with the exact lines).

## Do
1. **`LODESTAR_VERSION` in every build:** tarballs, MSI, portable zip and the Docker image. Tags give the tag's version; nightlies give a rising, comparable version (match what `build.rs` and the update engine's ordering expect; check `crates/update` for the comparison rule and say which form you chose).
2. **The stable manifest on `v*` tags:** in the `release` environment (approval-gated), build a format-1 manifest for the stable channel with each artifact's URL, SHA-256 and size: the two Linux musl binaries, the MSI and the portable zip. Docker updates through the musl binary overlay, so it has no entry. Sign it with the release key, wrap it in the `<channel>.json` envelope `freshness.yml` reads, and upload it to `update-manifest` in the `update-manifest` concurrency group.
   - Prereleases (`-rc.N`): decide whether they publish to a separate channel or not at all, from what `crates/update` and `freshness.yml` already support. **No new channel or format**; record the call.
   - A re-run never overwrites a manifest for a release the owner has already published (mirror the draft-release guard).
3. **The nightly step** runs only when the `nightly` environment has the nightly update key; otherwise it logs a notice and skips (Q8).
4. **The builder** (`update_manifest.py`): stdlib only; inputs are the artifact files and the tag; output checked against the README's caps; never prints, logs or writes a key outside `$RUNNER_TEMP` with `umask 077` and a `trap` cleanup (the existing SHA256SUMS step is the pattern).

## Done when
- `test_update_manifest.py` covers: field values and order, sizes and hashes from real files, the caps, a missing artifact (fails loudly), and that no secret appears in output.
- The manifest round-trips through `lodestar-update`'s verifier in CI with a throwaway test key (never the real key).
- `actionlint` (if available) or a YAML check passes; secrets appear only as `${{ secrets.* }}` in `env:` of steps in the `release`/`nightly` environments; no `pull_request` path can reach a key.
- A dry run of the workflow on your branch, if `release.yml` supports `workflow_dispatch` without signing (say what you ran).
- **Size:** about 450 lines (plan). Past ~900, tell the runner first.
- **Process:** push once per round; no fixed sleeps in any test; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and full head SHA once CI is green.

## These go to the runner
- any change to the manifest format, a new channel or target kind;
- a key used outside its environment, or a new secret;
- any change to the existing signing, checksum or draft-release steps beyond what the manifest needs.

## Coordination
PK-smoke edits release.yml's smoke steps later; keep your changes in their own jobs or steps so it merges cleanly. Running now: CMP-5, CMP-10, WK-10e. None touch your files.
