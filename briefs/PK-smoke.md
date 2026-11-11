# PK-smoke (Sonnet, helper, /security-review on the release.yml diff): the M6 exit smoke on every packaged artifact

Assigned by the runner on 2026-11-11 to the session that built PK-unraid (#680) and PK-guide-1 (#682). The plan is `docs/plan/2026-11-06-m6-gap-audit.md`, rows 85-88 and 120, plus Q1. The slices.yaml entry is PK-smoke. Its deps PK-manifest and PK-win-restart are merged.

## Goal
Today's smokes check health only. Each packaged artifact must now pass **setup → crawling → search**:
1. complete setup by token, through the same API the wizard uses;
2. see the crawler running (a status or metrics field, not a log grep);
3. import a tiny fixture;
4. find it with a search.

The artifacts are the Docker image (release.yml `smoke` job), the Linux tarball (`packaging/linux/smoke.sh`), and the MSI and portable zip (`packaging/windows/smoke.ps1`).

## Rules
- **release.yml:** edit only the smoke steps. Never touch build, sign, minisign, attest, publish or permissions steps, or `environment:`. The Opus review diffs these line by line.
- **Shared code:** put shared fixture or setup helpers under `packaging/smoke/**`. Use one tiny fixture (a few torrents). Don't add a network fetch: CI runners may have no DHT reach, so "crawler running" means the task is up, not that results came in.
- **Waits:** every wait is a condition loop with a deadline of at least 10 s. No fixed sleeps.
- **Secrets:** no secrets in logs. Print a setup token only redacted, and leave no credentials in artifacts.
- **Read the existing patterns first:** the setup-token API (grep crates/api for setup and token), the import path (CLI `lodestar import`, or the API), and the search API.
- **Windows:** smoke.ps1 already has install, service, Restart now, upgrade and uninstall checks. Add the new flow without weakening any of them.
- **Scope:** don't edit crates/. If a Rust change seems needed, say so and stop.

## Owns
- `.github/workflows/release.yml` (smoke steps only)
- `packaging/linux/smoke.sh`
- `packaging/windows/smoke.ps1`
- `packaging/smoke/**`
- `docs/decisions/2026-11-11-PK-smoke.md`

## Bar
- `bash -n` and shellcheck on smoke.sh.
- PowerShell parse check if available; otherwise say so.
- Run the Linux smoke locally against a `cargo build --release` binary or tarball if feasible, and report what you ran.
- The Release workflow's pull-request trigger runs the smoke jobs, so CI is the proof. Make sure the PR triggers them (check release.yml's `on:` paths).
- Branch `slice/PK-smoke`, PR `PK-smoke: <title>`, steward skill.
- Reply "ready" with the head when the smoke jobs and ci-ok are green.
