# CI/CD, release supply chain, packaging and infrastructure review
Reviewed at 55e155a9.

## Coverage
- Read in full:
  - `.github/workflows/*.yml` (all 15), `.github/scripts/ci_changes.py`, `.github/scripts/update_manifest.py`
  - `packaging/docker/Dockerfile`, `packaging/docker/check-ids.sh`, `docker-compose.yml`, `.dockerignore`, `rust-toolchain.toml`
  - `packaging/systemd/lodestar.service` and its README, `packaging/linux/{build-tarball,smoke}.sh`, `packaging/keys/README.md`
  - `packaging/unraid/*.xml`, `packaging/companions/compose/*.yml` and four of its Unraid templates
  - `packaging/windows/main.wxs`, `{secure-datadir,remove-datadir,dacl-check,service-sid,sign,build-msi,build-portable}.ps1`, `portable/*`
  - `tools/status-connector/src/index.ts`, `wrangler.toml`, `package.json`, README; `.claude/settings.json`, `.claude/hooks/merge-guard.sh`
  - `infra/operator/{wake,bridge,ghapp,message,oplock,apiclient,tripwire,thread,agent_apply}.py`, `agent/*`
  - `infra/fleet/{fleet,apply,command,gates,reader,limits,reaper,audit,lock,state,linode,deployment_policy,emergency_cleanup,cleanup_registration,comments,status,timefmt,ancestry}.py`, `limits.yaml`, `steps/{remote,run,rules}.py`, `steps/vm/compose.yaml`
  - `infra/aws/fleet-control/cleanup_handler.py`, `infra/aws/expiration-guard/deployed/index.py`, `infra/akamai/watchdog/deployed/index.py`
  - `infra/akamai/terraform/*.tf`, `cloud-init/ubuntu24.yaml.tftpl`, `pia-example/*`
  - `infra/pia-bridge/deliver.py`, `vm/pia-wireguard.sh`
- Skimmed: `infra/aws/fleet-control/build_templates.py` (the two role stacks, lines 1-330; not the cleanup stack), `tools/status-connector/src/plan.ts` (first 120 lines), `infra/akamai/terraform/scripts/Deploy-AwsWindows.ps1` (grep only), `docs/operations/fleet-control-cleanup.md`, `docs/plan/2026-11-08-cloud-operator.md` (the residual-risk paragraphs), the decisions files for P-release, S-keys, status-connector, OP-1/OP-4.
- Not read:
  - `infra/operator/injection/**` and every `tests/` folder under `infra/` (test harnesses; time)
  - `infra/fleet/steps/vm/*.sh`, `scrub.py`, `docker-proxy.cfg` (run only on the disposable test VMs)
  - the generated CloudFormation JSON, the other `infra/akamai/terraform/scripts/*.ps1` (owner-run, manual)
  - `.github/scripts/{licenses,log_lint}.py` and the script tests
  - `packaging/windows/{smoke,test-secure-datadir,test-service-sid}.ps1`, `packaging/smoke/flow.py`, `packaging/unraid/validate.py`
  - nothing was built or run; GitHub's own settings (rulesets, environments, secrets) can't be seen from the checkout, so every statement about them comes from the repo's documents.

## Findings

### INFRA-01 The Docker image is pushed to GHCR (`:X.Y.Z`, `:X.Y`, `:latest`) on any `v*.*.*` tag push, with no environment gate or owner approval
- Severity: important
- Kind: security
- Confidence: confirmed (workflow); the repository's tag ruleset, if any, is not visible from the checkout
- Where: `.github/workflows/release.yml:302-369` (job `publish`: no `environment:`); compare `:635` (`package-windows`), `:790` (`package-release`), `:886` (`update-manifest`), which use `environment: release`; `packaging/unraid/lodestar.xml:4` and `docker-compose.yml:7` (both default to `:latest`); `.claude/hooks/merge-guard.sh:25-38` (blocks pushes to `main`, not tag pushes)
- What: The binaries, the draft release and the update manifest wait for the owner's approval in the `release` environment (`docs/decisions/2026-10-22-P-release.md`: "that gate is intended"; `packaging/keys/README.md` "Decided"). The `publish` job has `packages: write` and no environment, so it pushes the multi-arch image, `:latest` included, as soon as `smoke` and `build-arm64` pass. Nothing checks who pushed the tag, or that the tagged commit is on `main` or a `release/*` branch. The image is not signed and has no attestation, so a user can't tell an approved image from an unapproved one.
- Failure scenario: any session or collaborator with push access pushes `v9.9.9` on an arbitrary commit whose `Cargo.toml` says `9.9.9`. `package-gate` passes, `smoke` passes, and `ghcr.io/dvortsis/lodestar-v2:latest` points at that commit. Every Unraid and compose install that pulls `:latest` gets it, and the owner never saw an approval prompt. Unraid is the project's main audience, so the least-gated artifact is the most-used one. The operator plan already accepts that "any session that posts as the owner" exists (`docs/plan/2026-11-08-cloud-operator.md:131`), which is what makes an ungated tag path matter.
- Suggested fix: put `publish` in `environment: release` on tag refs (and `nightly` for the nightly); refuse a tag whose commit is not an ancestor of `origin/main` or a `release/*` branch; sign or attest the pushed digest in the same job.
- Tracked?: not found in `docs/decisions/` or `runner/slices.yaml`. `M8-SEC-5` (planned, a read-only review of workflows and signing) and the running REL-01 provenance work touch the area; neither names this gate.

### INFRA-02 `freshness.yml` re-signs whatever envelope sits on the `update-manifest` release, so a rolled-back manifest stays "current" for good
- Severity: important
- Kind: security
- Confidence: likely (the workflow is confirmed; the attacker needs write access to release assets)
- Where: `.github/workflows/freshness.yml:104-177` (downloads `<channel>.json`, checks only the manifest's signature, signs a new 30-day statement, uploads); `.github/workflows/release.yml:1021-1027` (the next release takes the published envelope as `--previous` with the same single check); `crates/update/README.md:133-136` (the client's high-water mark is per install)
- What: The signed manifest exists so that someone who can write to the repository's releases still can't install or roll back anything. What is left to them is a freeze, and the weekly job makes it permanent. An older `stable.json` is still validly signed by the release key. The job has no floor (no "the manifest must list the newest published `v*` release", no remembered highest timestamp), so it signs a fresh statement over the old manifest every Monday. Without the job, the old envelope's own freshness statement would lapse within 30 days and clients would report a manifest that is no longer current.
- Failure scenario: a session posting as the owner (or any job token with `contents: write`) uploads last month's `stable.json` over the current one with `gh release upload --clobber`. Installs that already saw the newer manifest refuse it (their high-water mark). Every new install, and every install that turns update checks on later (they are off by default), has no mark: it accepts the old manifest, sees a current freshness statement, and reports "up to date" while a security release exists. Nothing fails anywhere. The next tag's manifest job then builds on the old envelope and silently drops the releases published after it.
- Suggested fix: before signing, require each channel's manifest to list the newest published non-draft release for that channel (`gh release list`), and keep the last signed `(timestamp, sha256)` per channel somewhere a release-asset writer can't change (the `freshness` environment's variables, or a file on `main`); refuse a lower timestamp. Give `release.yml`'s `--previous` step the same floor.
- Tracked?: not found. `docs/decisions/2026-10-30-X-freshness-hardening.md` covers concurrency and failed uploads, not a rollback of the asset.

### INFRA-03 A backport tag moves `:latest` backwards
- Severity: important
- Kind: correctness
- Confidence: likely (rests on `docker/metadata-action`'s documented `latest=auto` behaviour: every non-pre-release semver tag gets `latest`, with no comparison against existing tags)
- Where: `.github/workflows/release.yml:342-351` (`flavor: latest=auto`, `type=semver`); `packaging/unraid/*.xml:4`, `docker-compose.yml:7`
- What: Releases are cut from `release/vX.Y.Z` branches while `main` moves ahead (`Cargo.toml` on `main` says 0.5.0; v0.5.1 is published; the owner decisions plan patch releases on older lines). Tagging a fix on an older line after a newer minor has shipped re-points `:latest` at the older version. The update manifest orders by semver and is not affected; only the image tag is.
- Failure scenario: v0.6.0 ships and migrates the store. A security fix is tagged as v0.5.3 from `release/v0.5`. `:latest` is now 0.5.3. Unraid installs with automatic image updates pull an older binary onto a 0.6 store; at best it refuses to start, and the user's working install is down until they find out why.
- Suggested fix: compute `latest` in the workflow: set it only when the tag is the highest non-pre-release version among the repository's `v*` tags (`flavor: latest=false` plus a `type=raw,value=latest,enable=...` from a small version-compare step).
- Tracked?: not found.

### INFRA-04 The released image is built from the shared Actions cache, and mutable-tag actions on `main` can write that cache
- Severity: important
- Kind: security
- Confidence: likely (the asymmetry is confirmed; a working poisoning of BuildKit's GHA cache was not attempted)
- Where: `.github/workflows/release.yml:77-78` and `:299-300` (`cache-to: type=gha,mode=max` on every run, `main` included), `:367-369` (`publish` builds the pushed image with `cache-from: type=gha`); compare `:530-534` and `:657-661` ("No shared build cache on tag builds: a release is built from scratch", packages only); `.github/workflows/ci.yml:58-586` (48 `uses:` by tag or branch: `actions/checkout@v4`, `dtolnay/rust-toolchain@stable` and `@nightly`, `Swatinem/rust-cache@v2`, `taiki-e/install-action@v2`, `actions/setup-python@v5`), `.github/workflows/web.yml:52-53`
- What: The tarballs and the MSI are built without a cache on tags, on purpose. The Docker image, the artifact most users run, is assembled from BuildKit layers in the repository's Actions cache. A tag run can read caches written on the default branch, and any job that runs on `main` can write them, whatever its `permissions:` block says. `ci.yml` and `web.yml` run on every push to `main` with third-party actions pinned by a movable tag or branch, and they run `npm ci` and `cargo` build scripts there. The comment at `release.yml:70-71` also shows the design depends on the cache: `publish` is "the build that was smoked" only when the cache hits; on a miss it pushes a fresh, unsmoked build.
- Failure scenario: a tag behind one of those actions is moved (the tj-actions pattern), or a dependency's install script runs in a `main` job. The code writes cache entries that the next tag's `smoke` and `publish` builds import as the "Rust binary" stage. The smoke checks health and a search, not what else the binary does. The published `:latest` carries the planted layer; the signed tarballs from the same tag don't.
- Suggested fix: build the release image once without `cache-from` on tag refs, push it by digest, smoke that digest, then add the tags with `docker buildx imagetools create`; pin every action in `ci.yml` and `web.yml` by commit SHA as `release.yml` and `e2e.yml` already do.
- Tracked?: partly. `docs/decisions/2026-11-02-status-connector.md:11` notes that "some older jobs" in `ci.yml` still use tags; the release image's cache is not discussed. `M8-SEC-5` plans a review of "pinning".

### INFRA-05 Rust tests that read files outside `crates/` are skipped on PRs that change only those files, and `ci-ok` passes
- Severity: important
- Kind: test
- Confidence: confirmed
- Where: `.github/scripts/ci_changes.py:46-52` (`RUST_ALSO`), `:198-210` (`touched` maps only paths under `crates/*`, `bench/`, `fuzz/`); the tests: `crates/update/src/keys.rs:66-68` (`packaging/keys/*.pub` against the embedded key chain), `crates/lodestar/src/metrics.rs:1712,1729` and `crates/lodestar/src/cluster/serve.rs:2613` (`docs/metrics.md`), `crates/search/tests/doc_examples.rs:10` (`docs/query-language.md`), `crates/companions/tests/templates.rs:295-325` (`packaging/companions/**`), `crates/lodestar/src/update/windows/portable.rs:2242` (`packaging/windows/portable/Start Lodestar.cmd`), `crates/sources-plugins/tests/safety.rs:956` (`docs/user`); `.github/workflows/ci.yml:653-665` (a skipped job counts as passing)
- What: `ci_changes.py` sets `rust=false` unless a changed path is inside a workspace package or matches `RUST_ALSO`. At least six test groups read contract files that live elsewhere. A PR that edits only one of those files runs no Rust job, `ci-ok` is green, and the test first fails on `main` after the merge.
- Failure scenario: a docs PR edits a metric row in `docs/metrics.md`, or a packaging PR replaces `packaging/keys/lodestar-release.pub` or edits a companion template. `test` is skipped and the PR merges green. `main` goes red in `memory_and_segment_families_are_documented`, `embedded_keys_match_packaging` or `templates`. Because `ci.yml` cancels a running `main` build when the next merge lands (`ci.yml:30-34`), that red run can itself be cancelled, so the break is found by whichever later PR happens to touch Rust.
- Suggested fix: add the outside paths to `RUST_ALSO` (`^packaging/keys/`, `^packaging/companions/`, `^packaging/windows/portable/`, `^docs/(metrics|query-language)\.md$`, `^docs/user/`), and add a `test_ci_changes.py` case that greps `crates/` for reads that leave the workspace and fails when one isn't covered.
- Tracked?: partly. PR 784 `X-ci-main-signal` is about the signal on `main`; the path gap was not found in `runner/slices.yaml` or `docs/decisions/`.

### INFRA-06 `fleet-emergency-down.yml` is outside the `infra` job's path filter, so its structural tests don't run on a PR that changes it
- Severity: important
- Kind: test
- Confidence: confirmed (checked the regex against the three file names)
- Where: `.github/scripts/ci_changes.py:76` (`INFRA` matches `fleet\.yml`, not `fleet-emergency-down\.yml`); `infra/fleet/tests/test_workflow.py:32` (`COVERED` lists it), `infra/fleet/tests/test_emergency_cleanup.py:24-60`
- What: The workflow that holds the only live teardown the owner can trigger is pinned by tests (the actor ID gate, `main` only, first attempt, its environment, no extra permissions). The `infra` job runs them only when `INFRA` matches, and the pattern lists `fleet`, `operator-wake`, `operator-apply`, `operator-injection` and `operator-tripwire*`.
- Failure scenario: a PR edits only `.github/workflows/fleet-emergency-down.yml` (drops the `actor_id` condition, adds a second job with `id-token: write`, or changes the environment). `infra` is skipped, `ci-ok` passes, and the tests that would have refused it run first on `main`, after the change is live.
- Suggested fix: `fleet[\w-]*` in `INFRA`, as `operator-tripwire[\w-]*` already has, plus a `test_ci_changes.py` case that every file in `COVERED` matches `INFRA`.
- Tracked?: not found.

### INFRA-07 The Cloudflare deploy token is a repository secret, and a manual run deploys any branch's Worker
- Severity: important
- Kind: security
- Confidence: likely (the README and the workflow comment say "repository secrets"; the live settings are not visible)
- Where: `tools/status-connector/README.md` "Deploy from GitHub" (the table headed "Repository secret"); `.github/workflows/status-connector-deploy.yml:6` (comment), `:13` (`workflow_dispatch`), `:26` (`environment: status-connector-deploy`, with no `github.ref` condition on the job)
- What: The signing keys were moved into environments on purpose ("not as repository secrets", `docs/decisions/2026-10-22-P-release.md`). `CLOUDFLARE_API_TOKEN` (the account-wide "Edit Cloudflare Workers" template) is documented as a repository secret. A repository secret is available to every workflow run in the repository, including a run on a branch whose own workflow file was changed. `workflow_dispatch` also lets any branch run the deploy job with that branch's Worker source.
- Failure scenario: a session that has read untrusted input pushes a branch with a workflow that sends `secrets.CLOUDFLARE_API_TOKEN` out. With it, the `lodestar-status` Worker is replaced; the Worker's own secrets (`GITHUB_TOKEN`, `PATH_SECRET`) are bound to the script, so the replacement reads them too.
- Suggested fix: move both Cloudflare secrets into the `status-connector-deploy` environment, limit that environment to `main` with the owner as reviewer, add `if: github.ref == 'refs/heads/main'` to the job, and say so in the README.
- Tracked?: `M8-SEC-5` names `status-connector-deploy` as a review subject; no fix slice found. `docs/decisions/2026-11-02-status-connector*.md` don't discuss the secret's scope.

### INFRA-08 Release binaries are compiled with whatever `stable` is on the day; the Docker image with 1.97.1
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `rust-toolchain.toml` (`channel = "stable"`); `.github/workflows/release.yml:526,653` (`dtolnay/rust-toolchain@<sha> # stable`: the action is pinned, the toolchain it installs is not); `packaging/docker/Dockerfile:28-36` (`rust:1.97-bookworm@sha256:…`, `RUSTUP_TOOLCHAIN=1.97.1`, with the comment that `stable` "would otherwise make rustup download whatever is newest"); `runner/standards.md:17` (CI's stable was 1.99 on 2026-11-11); `docs/decisions/2026-10-22-P-release.md:6` ("so tarball and image binaries match")
- What: The Dockerfile pins the compiler for exactly the reason the package jobs don't. A tag build today gives a Docker binary from rustc 1.97.1 and a tarball, updater binary and MSI from rustc 1.99 or later. They are the same source and different programs, and the tag does not determine the second set: rebuilding the same tag next month uses another compiler. Soak and smoke evidence gathered on one artifact doesn't carry to the other, and the decision's stated parity doesn't hold.
- Failure scenario: a codegen or std change between two stable releases affects the allocator or mmap behaviour the soak work has been chasing. The Docker soak passes on 1.97.1; the Windows MSI and the Linux self-update binary ship built with a compiler nothing soaked.
- Suggested fix: pin `rust-toolchain.toml` to an exact version, let the Dockerfile read it (or assert the two agree in CI), and bump it by PR.
- Tracked?: REL-01 (build provenance) is the nearest work; this pin was not found in `runner/slices.yaml` or `docs/decisions/`.

### INFRA-09 No dependency-advisory check anywhere in CI, and nothing refreshes the pins
- Severity: important
- Kind: gap
- Confidence: confirmed (`grep` over `.github/`, `runner/slices.yaml`, `docs/build-plan.md` for cargo-audit, cargo-deny, npm audit, osv, dependabot, renovate, CodeQL: no hits; no `deny.toml`, no `.github/dependabot.yml`)
- Where: `.github/workflows/ci.yml` (the only dependency job is the licence allowlist, `:135-140`); every digest and hash pin (`packaging/docker/Dockerfile:16,28,100`, action SHAs, `infra/fleet/requirements-*.txt`)
- What: The product parses hostile input from the DHT, peers and feeds with a large crate tree, and the release is built from `Cargo.lock` and `package-lock.json` as they are. No job fails, warns or opens an issue when a locked crate or npm package gets an advisory. The one case in the record (the tokio bump, `docs/owner-decisions.md:651`) was handled by hand. The careful digest and SHA pins have no mechanism that proposes a newer one, so they age silently: the distroless base, the Rust image, the Playwright image and the actions.
- Failure scenario: a RUSTSEC advisory lands for a crate on the peer-wire path a week before a tag. Nobody is told; the tag ships it.
- Suggested fix: a nightly `cargo deny check advisories` (or `cargo audit`) and `npm audit --omit=dev` job that opens or updates one tracking issue, as `all-themes-issue` in `e2e.yml` already does; Dependabot or Renovate limited to actions and Docker digests, with the runner reviewing its PRs.
- Tracked?: `M8-SEC-5` lists "pinning, dependencies" as a review subject; no CI slice found. PR 786 adds `SECURITY.md` and the review method only.

### INFRA-10 Live mode no longer writes the spend ledger, so the $40 monthly cap rests on a hand edit; the code and comments still describe the retired reaper
- Severity: polish
- Kind: maintainability
- Confidence: confirmed
- Where: `infra/fleet/apply.py:372-387` (`destroy` always raises in live mode; `finish_down`, the only writer of `fleet/spend-ledger.json`, is reached only after `destroy` returns); `infra/fleet/fleet.py:279-281,349-352,445-448` (live `down`, `reap` and `dispatch-down` all refuse; no workflow calls `reap` or `dispatch-down`: `infra/fleet/tests/test_workflow.py:81,210`); `infra/fleet/limits.py:34-44` (`check_up` adds the ledger to the projected cost); stale text: `.github/workflows/operator-tripwire.yml:3` and `infra/operator/tripwire.py:12` ("never fleet.yml, which runs the reaper"), `infra/fleet/README.md` (`reap` as "the hourly reaper"), `infra/akamai/terraform/main.tf:33-35` ("the hourly audit checks the rule set"), `infra/fleet/steps/run.py:336` and `infra/pia-bridge/deliver.py:440` ("the next sweep removes it")
- What: After the move to the independent cleanup broker (`docs/operations/fleet-control-cleanup.md`, a recorded decision), nothing in live mode books a finished fleet's cost. `month_spend_usd` is the ledger plus the running fleet's accrual, so the owner's cap is enforced only if the owner adds each fleet's cost to the ledger by hand during reconciliation (`docs/operations/cloud-fleet-runbook.md:225` says to reconcile it, not how much to write). A reconciliation that tombstones `fleet.json` and clears the flag but leaves the ledger alone makes every later `up` see zero spend. About 250 lines of reaper, sweep, audit and delete code are now reachable only from tests, and five comments tell a reader that an hourly reaper or sweep exists. The real behaviour for a job rule that could not be closed is different from what the message says: the broker treats a stale rule as drift and deletes the fleet.
- Failure scenario: the owner reconciles three fleets in a month without touching the ledger; the cap check passes on the twelfth as on the first. Separately, a maintainer reads "the next sweep removes it", expects the rule to go, and instead loses a running screen at the broker's next pass.
- Suggested fix: have the broker (or a small owner-run `reconcile` command) book the accrued cost when it confirms the deletions, or make `up` refuse while the last tombstone has no ledger entry for its fleet; delete the dry-run-only live paths or mark them plainly; correct the five comments.
- Tracked?: the manual reconciliation is recorded (`docs/operations/fleet-control-cleanup.md:38`); the ledger consequence and the stale text were not found.

### INFRA-11 The scheduled freshness job has no watchdog of its own
- Severity: polish
- Kind: resource
- Confidence: likely (GitHub disables scheduled workflows in a public repository after 60 days without repository activity; the repository's visibility is taken from the operator prompt's "public repository")
- Where: `.github/workflows/freshness.yml:11-13` (weekly cron), `:70-76` (fails only when it runs)
- What: Every installed copy's ability to install updates depends on this job running at least once every 30 days. If it stops running, nothing fails: a disabled schedule, an expired or removed `FRESHNESS_SIGNING_KEY`, or four red Mondays that only the last editor of the cron line is mailed about. Clients then stop installing updates, security fixes included (`crates/update/README.md:136`).
- Suggested fix: a second, independent check (the nightly `ci.yml` run can do it with no secret): download each channel envelope, fail and open the tracking issue when its freshness statement has less than 14 days left.
- Tracked?: not found.

### INFRA-12 The signing jobs install minisign from apt at run time; CI pins it by hash because apt was unreachable once
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `.github/workflows/release.yml:826,963`, `.github/workflows/freshness.yml:80` (`sudo apt-get update && apt-get install minisign`); `.github/workflows/ci.yml:588-602` (the pinned upstream 0.12 with its SHA-256, added after #694's outage)
- What: The round-trip test proves the manifest format against minisign 0.12; the jobs that hold the release and freshness keys use whatever version the runner image's apt serves, and stop if the security pocket is down, in the middle of a release the owner has already approved twice.
- Suggested fix: move the pinned install into a small script and call it from all four places.
- Tracked?: not found.

### INFRA-13 `sign.ps1` writes the certificate to a fixed temp name and passes its password on the command line
- Severity: polish
- Kind: security
- Confidence: confirmed
- Where: `packaging/windows/sign.ps1:23-28`
- What: The PFX goes to `%TEMP%\lodestar-sign.pfx` (a predictable name, default ACL) and `signtool` gets `/p <password>` as an argument, visible in the process list while it runs. On a single-use hosted runner this is low risk; the script is also what the README tells the owner to run on their own PC.
- Suggested fix: a random file name in a folder created with an owner-only ACL (the pattern `secure-datadir.ps1` already uses), or import the certificate into the user store and sign by thumbprint (`/sha1`), which needs no password argument.
- Tracked?: not found (the certificate itself is an open owner action).

### INFRA-14 The Unraid template checker is in the repository and no workflow runs it
- Severity: polish
- Kind: test
- Confidence: confirmed (`grep` for `validate.py`, `test_validate` and `packaging/unraid` in `.github/workflows/`: no hits)
- Where: `packaging/unraid/validate.py`, `packaging/unraid/test_validate.py`; `.github/workflows/release.yml` (runs on `packaging/**` changes, never calls it)
- What: The three templates users install from are checked only by hand.
- Tracked?: PR 781 `X-unraid-validate-all` is open on this.

## Checked and found sound
- **No script injection through event fields.** Every `${{ github.event.* }}`, input and label value reaches a script through `env:`, never inside `run:` (all 15 workflows). The two exceptions interpolate values the workflow itself computed (`matrix.target` and `matrix.arch`).
- **`pull_request_target` is used once and safely** (`operator-tripwire.yml`): it checks out `main`, holds no secret, and reads only numbers and enum fields.
- **`ci-ok`, `web-ok`, `e2e-ok`** fail on a failed or cancelled need. `web-ok` and `e2e-ok` also check the applicability output is exactly `true` or `false`. `ci-ok` trusts `changes`' outputs, which is sound because the step fails under `pipefail` when the script crashes.
- **`e2e.yml`'s `commit-baselines`** (the one PR job with `contents: write`) runs no PR code, refuses symlinks, checks each path against a strict pattern, size and PNG magic, and pushes only to a same-repository PR branch.
- **Release signing path:** each key is visible to one step in one environment; the release key expression is empty outside tag refs (`release.yml:925-927`); the manifest job compares the published files byte for byte with what the run built, checks them against the signed `SHA256SUMS`, verifies the previous envelope before extending it, and never clobbers a published release. `update_manifest.py` refuses duplicate keys, foreign hosts, non-semver versions and a re-run.
- **Dockerfile:** all three base images are pinned by digest, zig by SHA-256, cargo-zigbuild built from source with `--locked`; the image has no shell, a healthcheck, and the smoke job proves the privilege drop (every thread's ids, zero capabilities, `NoNewPrivs`) and that `PUID=0` is refused.
- **MSI:** fixed install folder enforced in the execute sequence; the service runs as `NT SERVICE\Lodestar` with privileges cut to two; the web firewall rule is opt-in and private-profile only; every SYSTEM custom action runs a system binary or a file from `Program Files`; `AllowDowngrades` covers the shared MSI version of nightlies and release candidates; `secure-datadir.ps1` fails closed on links, foreign owners and unreadable ACLs, and keeps its log in a new SYSTEM-only folder.
- **systemd unit:** `DynamicUser`, `ProtectSystem=strict`, empty capability set, restricted address families and namespaces. (`SystemCallFilter=@system-service`, `MemoryDenyWriteExecute`, `ProtectProc=invisible` would add a little; not a finding.)
- **Fleet and operator control plane:** strict allow-list parsers for `/fleet` and operator instructions; numeric author IDs; first-attempt-only gates; fixed strings in logs and comments; S3 conditional-write locks with `ExpectedBucketOwner`; IAM roles limited to exact object keys with an exact OIDC subject per environment and workflow; the App token minted for one repository with two permissions and revoked after; the agent's network limited to `api.github.com`; the tripwire fails closed on an incomplete sweep. The residual risk that every session shares the owner's login is recorded (`docs/plan/2026-11-08-cloud-operator.md:131,263`).
- **Cleanup broker and watchdogs:** exact-ID deletion only, account and tag re-checks before each delete, bounded counts, no firewall deletes, fail-closed on unreadable inventory. That a partial `terraform apply` is not adopted automatically, and needs the owner, is a recorded decision (`docs/operations/fleet-control-cleanup.md:22`).
- **PIA bridge:** the login travels only on SSH stdin with a pinned host key, values are masked before use, remote output is discarded, `/run/lodestar-pia` is checked gone afterwards, and the VM script passes credentials to curl from files and pins the endpoint.
- **Status connector:** nothing is served without a path secret of at least 32 characters, compared by digest; read-only GitHub calls; single JSON-RPC objects only; errors carry a status and a path, never the token.

## Area-level concerns
- **The least-protected release artifact is the one most users run.** INFRA-01, -03 and -04 are all about the Docker image: no approval, no signature, a shared cache, and a `:latest` that can move backwards. The binaries got an environment gate, a from-scratch build and a signature; the image got none of the three.
- **Path-filtered CI plus cancel-in-progress on `main` leaves holes at the edges.** Skipped jobs count as passing, so every file a test reads must be in a filter (INFRA-05, -06). A break that slips through is then found on a `main` run that the next merge cancels. PR 784 addresses the second half.
- **`release.yml` has no aggregate check and is not required** (recorded, `runner/README.md:20`). A PR can break the Docker build, the tarball smoke or the MSI and still satisfy branch protection; only the runner's reading of the checks stops it.
- **What CI does not gate, against CLAUDE.md's "Before pushing":** `cargo fmt`, clippy and `cargo test` run on Linux and Windows for Rust PRs (clippy for `cfg(windows)` on the Windows leg); macOS only on `main`, nightly or `full-ci`. All the web checks run on Linux. Not in any required check: search latency, the filter budget, reporting overhead, the all-themes and fixture E2E runs, the soak screen, the whole Release workflow, and `tools/status-connector` tests (their own unrequired workflow).
- **Stale structure after the cleanup-broker change** (INFRA-10): the fleet code now has two descriptions of teardown, one live and one only in tests and comments. The next person to change it has to learn which is real from a runbook.
- **Everything about GitHub's own settings is asserted, not checked,** except for the fleet environments, which `deployment_policy.py` audits before every role assumption. The `release`, `nightly`, `freshness` and `status-connector-deploy` environments have no such audit; a `release` environment created unprotected on first use (the P-release decision warns of it) would hand out the release key with no approval. A small read-only audit in the same style, run before the signing steps, would close that.
