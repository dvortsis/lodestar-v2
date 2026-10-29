# Lodestar v0.5 light review: Area B results

Date: 2026-10-24

Instructions: [Codex light review](codex-review-v0.5-light.md), **Area B: packaging and install**.

Reviewed a fresh clone of `main` at `2d5e761d30cb1b7947fb09493959668eabcb84ab`. Locations refer to that commit. The review was read-only; this report was subsequently saved at the owner's request. No fixes, commits, pushes or GitHub comments were made.

## Validation and limits

- Compared Docker, Compose, Unraid, systemd, Linux archives and Windows MSI/portable packaging with `docs/operations.md`, `docs/setup.facts.md`, the user guide, architecture §11/§19.6 and owner decisions §2.14/§2.23.
- Applied the project `user-docs` rules and the installed `github-docs` review gates. Preflight was READY; documentation sync was current; **1,068 lint checks, 0 failed, 4 warnings**. Warnings concerned reading-path formatting, heading case and pending screenshots; these were not reported as install bugs.
- All **three Unraid templates passed** `packaging/unraid/validate.py`.
- All **eight Windows PowerShell scripts parsed**. `test-service-sid.ps1` passed under Windows PowerShell with a process-only execution-policy override. No machine-wide policy was changed.
- Inspected the pinned local Tokio 1.54 Windows signal implementation to distinguish Ctrl+C from console-close events.
- No Docker image, Linux/systemd or MSI installation was executed. Conclusions below are based on source and configuration traces, not installed-system smoke tests.
- Excluded planned worker mode, split storage and the owner's explicitly approved unsigned Windows builds. Snapshot restoration correctly rejects mismatched hot-column stamps and rebuilds those columns.

### 1. Linux build instructions produce a binary without the web UI

Location: `docs/operations.md:74`\
What happens: Follow the source-install instructions on a fresh checkout. They run only `cargo build --release -p lodestar`, leaving `web/build` absent. `crates/lodestar/build.rs` calls the bundler, whose missing-build branch embeds **UI not built**, rather than the setup and Search screens. The build example in `packaging/linux/README.md` also omits the frontend prerequisite.\
Expected: Build the frontend before compiling Rust, as required by `crates/lodestar/build.rs` and the archive's promise of an embedded UI. State the dependency-install/build commands and where to run them.\
Confidence: high

### 2. Plain Docker example permits termination before shutdown saves finish

Location: `packaging/docker/README.md:18`\
What happens: The documented `docker run` command omits `--stop-timeout=60`. Docker's default 10-second timeout can expire while Lodestar is draining a stalled HTTP connection for `SHUTDOWN_GRACE` (10 seconds), before `Handles::shutdown()` persists the pipeline, DHT state, index and hot columns. A normal stop can therefore become a forced kill instead of a clean stop.\
Expected: Allow the longer shutdown window specified later in this README and already configured in Compose and all Unraid templates. Add the stop timeout to the runnable example.\
Confidence: high

### 3. Portable Windows instructions recommend an unhandled shutdown action

Location: `packaging/windows/portable/README.txt:9`\
What happens: Follow **Close the window to stop Lodestar**. The console-close event is distinct from Ctrl+C. `main::shutdown_signal()` subscribes only to Ctrl+C on Windows, and the pinned Tokio implementation falls through to the OS handler when there is no close-event subscriber. The process exits without running its normal shutdown saves. `Start Lodestar.cmd:3` repeats the close-window advice.\
Expected: Instruct users to press Ctrl+C and wait, or implement console-close handling, preserving architecture §12's graceful shutdown. This finding concerns the portable process; the Windows service has its own proper stop handling.\
Confidence: high

### 4. Unraid guide documents a field absent from the templates

Location: `docs/user/02-install-lodestar-on-unraid-and-docker.md:112`\
What happens: Open advanced template settings to change the documented **Log level** field. None of `lodestar.xml`, `lodestar-gluetun.xml` or `lodestar-worker.xml` defines that field or an environment variable with target `LODESTAR_LOG`. The appendix and setup-facts document also claim the field exists.\
Expected: Describe the actual template fields, or explain how to add the variable, per the project user-docs rule that labels/defaults come from the implementation.\
Confidence: high

Checked and clean: `docker-compose.yml`, `packaging/docker/Dockerfile`, `packaging/systemd/lodestar.service`, `packaging/windows/main.wxs`, `packaging/windows/service-sid.ps1`, `packaging/windows/secure-datadir.ps1`, `packaging/windows/remove-datadir.ps1`, `packaging/linux/build-tarball.sh` (no additional defects identified in this light pass).
