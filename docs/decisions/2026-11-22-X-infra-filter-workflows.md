# X-infra-filter-workflows decisions (#1058)

Runner brief, 2026-11-22Z, from #1044's review. `infra/fleet/tests/test_workflow.py` globs every `.github/workflows/*.yml`, but the `INFRA` path filter in `.github/scripts/ci_changes.py` listed only the fleet and operator workflows. A change to any other workflow could break those tests without the `infra` job running. The same kind of filter gap let #1029 reach main without the job, through `config.rs` and `docs/soak/tools/` instead (`2026-11-22-X-backoff-escalate-passthrough.md`).

## The rule: every workflow file, and the files directly in `docs/operations/`

- **Workflows:** `^\.github/workflows/[^/]+\.ya?ml$` replaces the fleet/operator name list. It is the broader of the two options in the brief. A workflow added later needs no edit, which the narrower "exactly the files the tests read" rule couldn't promise: `infra/fleet/tests/test_workflow.py` globs the folder, so the set it reads is "all of it". `infra/operator/tests/test_tripwire.py` reads `operator-tripwire.yml` and `operator-wake.yml` by name, and the pia-bridge, fleet and operator tests read `fleet.yml` and `operator-injection.yml` by name.
  - `.yaml` is matched as well as `.yml`, though the tests glob `*.yml` only. A stray extension costs one infra run, not a miss.
  - A file in a subfolder, a `.bak` and `README.md` don't match.
- **The files directly in `docs/operations/`:** `docs/operations/[^/]+$`. `test_block.py` reads `docs/operations/cloud-fleet-runbook.md`, and `docs/operations.md` was matched but the folder wasn't. A folder rule can't go stale when a test starts reading another doc there.
  - **Not the whole folder** (the runner's call under §2.26, after review). `docs/operations/imports/` holds about 27 import-pilot files that no infra test reads, and a PR touching them would pay for a ~7-minute infra run. My first version matched the whole directory; the runner narrowed it.
  - Three existing near-miss assertions had to change because their paths now match, and no others did:
    - `Infra`: `docs/operations/lodestar-cloud-assets.md` moved from the near-misses to the matches, and `.github/workflows/operator-other.yml` was dropped from the near-misses (it matches now; no test asserts it as a match, the new tests use other names).
    - `FleetEmergencyDown`: the near miss `fleet-emergency-down-x.yml` became `fleet-emergency-down.yml/x`.

## Gaps found

I compared every path the infra job's tests read with `INFRA`:

- **Matched already:** everything under `infra/` (fleet, operator, aws, akamai, pia-bridge, dev-vm), `packaging/companions/gluetun-post-rules.txt`, `docs/soak/tools/linux/collect.py` and `analyze_cr.py`, `crates/lodestar/src/config.rs`, `docs/operations.md`, `docs/user/03-keep-lodestar-safe.md`, and the fleet and operator workflows.
- **Gap 1 (the brief's):** the other workflows, 8 of the 15 in the folder. Before this PR six matched by name and `ci.yml` selected everything anyway.
- **Gap 2:** `docs/operations/cloud-fleet-runbook.md`. A runbook-only PR skipped infra.
- **Not reads:** `docs/decisions/2026-11-02-CR-SPEC.md` is a string inside a test fixture, and `fleet-control-bootstrap.md` and `fleet-control-cleanup.md` are text in generated template metadata. Neither file is opened.
- **Nothing else found** outside `infra/`.

## Every other output is unchanged

Main's 35 tests in `test_ci_changes.py` still pass, with the three assertions above changed, and the PR adds 4 (39 in all: `python3 -m pytest .github/scripts/test_ci_changes.py`). `ci_changes.py pull_request` on a one-file change, before and after this PR. Only `infra` moves; the other outputs (rust, rust_docs, web, e2e, soak_tools, companion_pins, unraid, update_manifest, advisories, mmap_retention, fuzz) are identical in every row.

| Changed path                                    | `infra` before | `infra` after |
| ----------------------------------------------- | -------------- | ------------- |
| `.github/workflows/web.yml`                     | false          | **true**      |
| `.github/workflows/e2e.yml`                     | false          | **true**      |
| `.github/workflows/release.yml`                 | false          | **true**      |
| `.github/workflows/fleet.yml`                   | true           | true          |
| `docs/operations/cloud-fleet-runbook.md`        | false          | **true**      |
| `docs/operations/lodestar-cloud-assets.md`      | false          | **true**      |
| `docs/operations/imports/pilots/smoke/smoke.py` | false          | false         |
| `infra/fleet/fleet.py`                          | true           | true          |
| `web/src/app.css`                               | false          | false         |
| `docs/architecture.md`                          | false          | false         |
| `docs/decisions/x.md`                           | false          | false         |
| `runner/slices.yaml`                            | false          | false         |
| `.github/scripts/test_ci_changes.py`            | false          | false         |

## Cost

The `infra` job now also runs on a workflow-only or operations-docs-only PR. Recent runs on main took about 6.5 to 7.5 minutes (15 minutes is its timeout), plus queue time. Workflow PRs are rare and the job runs beside the others, so it adds no wall-clock beyond its own duration.

## Not done here

- No workflow file, permission, secret or trigger is edited.
