# P-release: Release matrix, Linux tarball, Windows MSI and portable zip, smoke jobs
**Model:** Sonnet (packaging, CI). The runner reviews it line by line. **Branch:** `slice/P-release`.

**Spec:**
- `docs/build-plan.md` P3 (M6 packaging, the [S] items);
- `docs/architecture.md` §11 (deployment), §12 and §12.2 (updates: what an install looks like, signing comes later), §13 (layout), §14 (M6 exit);
- `packaging/*/README.md`;
- `.github/workflows/release.yml` (already builds and pushes the Docker image);
- `docs/decisions/2026-10-22-runner-ci.md`.

## Own
- `packaging/windows/**` (new: WiX source via cargo-wix, portable-zip layout, README);
- `packaging/linux/**` (new: tarball layout, install notes; the systemd unit already lives in `packaging/systemd/`, reference it rather than move it);
- `.github/workflows/release.yml`: new jobs only (Linux tarball and Windows MSI/zip build plus smoke). Don't change the Docker jobs, and keep #90's `arm64-needed` gate pattern;
- a new `.github/workflows/package-smoke.yml` if that's cleaner;
- `Cargo.toml`: only `[package.metadata.wix]` or the equivalent in `crates/lodestar/Cargo.toml`, if cargo-wix needs it. Say so.

## Build
1. **Linux tarball** for x86_64 and aarch64, musl static, matching the Docker build. It holds the binary, LICENSE, a README pointer and the systemd unit, plus a checksum file. Smoke: unpack, run `lodestar serve` with a temporary data dir, check `/healthz`, stop.
2. **Windows MSI** via cargo-wix:
   - installs to Program Files, data in ProgramData;
   - registers the service. The service entrypoint itself is slice M6-win's; until it merges, install the service but let the smoke test run the binary directly, and say so;
   - firewall rules for the web port (private profile only) and the DHT UDP port;
   - an opt-in Defender exclusion checkbox, off by default, with the trade-off explained (§11);
   - a Start-menu shortcut to the web UI;
   - a clean uninstall that keeps data unless asked.

   **Portable zip** too. Smoke on `windows-latest`: install the MSI silently, start the binary or service, check `/healthz`, uninstall.
3. **Release matrix:** tag builds attach the tarballs, MSI, zip and checksums to the GitHub release (draft until the owner publishes). Nightly builds them as workflow artifacts only. **No code signing yet:** the certificate is an owner action, so leave a clearly marked step that's skipped without the secret.
4. **PR gating:** the new jobs run on PRs only when `packaging/**`, `release.yml`, Cargo manifests or lockfile, or the toolchain change, or with `full-ci`. Always on tags and nightly.

## Common rules
- **Bar and precedence:** `runner/standards.md` is the bar. Read CLAUDE.md, `docs/owner-decisions.md`, and the architecture sections cited.
- **Decisions** go in a new `docs/decisions/<YYYY-MM-DD>-<slice-id>.md` (see `docs/decisions/README.md`), not in §16.
- **Before pushing,** merge `origin/main`; never rebase. Run every check CLAUDE.md lists for the files you touch.
- **Scope:** stay inside **Own**. Shared-file edits needed elsewhere go in the PR description.
- **PRs** stay at or under about 1,500 changed lines, or state the reason in the description.

## Done when
- The workflow YAML is valid; actionlint is clean if you can run it.
- **Locally:** `cargo build --release --target x86_64-unknown-linux-musl -p lodestar` works, and so does the tarball script.
- **CI on your PR:** the new smoke jobs are green. Label the PR `full-ci` once so every job runs.
- The PR is titled "P-release: Release matrix, Linux tarball, Windows MSI and portable zip". The description explains what's skipped until the owner adds a code-signing certificate.
