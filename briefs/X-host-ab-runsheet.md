# X-host-ab-runsheet (runner brief, 2026-11-23Z)

Docs slice, Sonnet, reviewed on Opus before merge. Placed by `docs/decisions/2026-11-23-runner-records-r14.md` (owner-decisions §2.82); its slices.yaml entry is `X-host-ab-runsheet`.

## Why
The experiment host (the owner's Unraid box) is down. The owner said "We should be able to run tests on unraid soon" and chose "Cloud now, Unraid later" (§2.82). When the owner says the box is back, the first block should start within hours. The host gives the runs of record for CR-AB-ADM and CR-AB-4 (the cloud's runs are provisional and never count as the gain).

## Goal
A run sheet in `docs/soak/runsheets/` (new folder) that an operator follows step by step for the host's stage 1:
1. **What runs:** CR-AB-ADM (B is `dht.coordinated_admission`; A is the shipped defaults), then CR-AB-4's uTP arm (B is `dht.utp`), back to back. The route backoff arm runs only if a baseline shows `fetch_no_route_total{family=ipv6} > 0` (decisions/2026-11-02-CR-4.md).
2. **The design:** CR-SPEC as amended by r5.1 (build-plan §0.5, decisions/2026-11-20-path-to-100-cr-spec.md, decisions/2026-11-02-CR-SPEC.md): two balanced blocks per treatment, ABBA then BAAB, with CR-SPEC's segment length (6 h measured; check the spec for warm-up) and its stopping rules. Analysis with `docs/soak/tools/analyze_cr.py effect --design abba|baab` (X-fleet-baab, #1089).
3. **Host safety (§2.68 and the earlier host decisions):** backup set to excluded; parity check and backup paused for the run, with a restore step that always runs (also after a failed or stopped run); disk and memory bounds; how to stop at a safe point.
4. **Reproducibility:** the pinned image (by digest, filled in at run time from the build that's on main then), the collector (`docs/soak/tools/linux/collect.py`) and its hash, the preset, and what's recorded per block. Never stitch runs.
5. **Evidence:** where results go (`docs/soak/evidence/cr-ab-adm/**`, `cr-ab-4/**`), what's scrubbed, and that raw logs, DBs, credentials and executables stay private.
6. **A checklist** for the operator's first hour, and what to send the runner (numbers only).

Read first: build-plan §0.5, the CR-SPEC and path-to-100 decisions, owner-decisions §2.34, §2.50, §2.68 and §2.82, decisions/2026-11-02-CR-COL-3.md through 2026-11-21-CR-COL-6.md, docs/soak/tools/linux/README.md, and the earlier host write-ups in docs/soak/ (2026-10-28 native CR-0 launch, 2026-10-29 checkpoint). `docs/operations/cloud-fleet-runbook.md` §3.3 shows the fleet's BAAB and A/A steps; borrow its style, not its infrastructure.

## Left out (don't do)
- Nothing touches the box: no SSH, no commands on it, no cloud calls. This is docs only.
- No host addresses, share names, user names, credentials, or anything that identifies the box or network. Use placeholders such as `<host>`.
- No change to CR-SPEC's numbers (they're the owner's); if the spec is ambiguous, say so in step 0.
- No edits to `cloud-fleet-runbook.md` or `docs/soak/tools/**`.

## Step 0 (send me, then wait for "go")
1. The run sheet's outline (headings), and the spec numbers you'll use, each with its source (file:line).
2. Anything in the specs that conflicts or is unclear.
3. Owns, to refine: `docs/soak/runsheets/**`, `docs/decisions/2026-11-23-X-host-ab-runsheet.md`. Name anything else, with a reason.
4. An estimate of the length.

## Rules
- Developer docs (exempt from user-docs), but plain and step by step: the operator may be the owner, following it alone.
- UTC Z only. No 12-digit numbers.

## Branch and PR
- Branch `slice/X-host-ab-runsheet`. PR title "X-host-ab-runsheet: the experiment host's stage 1 run sheet".
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
