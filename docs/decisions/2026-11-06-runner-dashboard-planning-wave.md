# Runner: the planning wave's slices on the dashboard

Runner, 2026-11-06. The owner asked on 2026-11-05T23:58Z to "Update the dashboard to incorporate all the new plans". This file records what went into `runner/slices.yaml` and `runner/dashboard.yaml` from the seven merged plans of the planning wave (owner §2.45).

## What was pasted
175 new slices in `runner/slices.yaml`, each copied in flow style from its plan's ready-to-paste block:
- **P-v07** (`docs/plan/2026-11-05-v07-m7.md` §4.1): 47 slices. GS-\*, RX-\*, TZ-0, OUT-\*, UP-\*, ENR-9 to ENR-14, the 12 enrichment plug-ins, UN-\*, IRC-\*, SRC-3b, G-v07-scale and G-v07. The plan's comment that X-search-files-6 isn't in `slices.yaml` yet is out of date, so it now just says why GS-0 depends on it.
- **P-v06-companions** (`2026-11-06-v06-companions.md` §4.1): 21 slices, CMP-0 to CMP-G.
- **P-v06-workers** (`2026-11-06-v06-workers.md` §4.1): 22 slices, WK-0 to WK-G.
- **P-m6-audit** (`2026-11-06-m6-gap-audit.md` §4.1): 17 slices, PK-\* and G-m6. This is the plan's main branch, which assumes the owner says yes to Q13. If the owner declines Q13, the runner drops PK-win-upd-2 and PK-win-upd-3 and swaps in the plan's two alternative entries for PK-guide-3 and G-m6.
- **P-v08** (`2026-11-06-v08.md` §5.1): 24 slices. PL-\*, CW-\*, NOS-\*, LEM-\*, ADH-0, DOC-v08 and G-v08.
- **P-m8** (`2026-11-06-m8-hardening.md` §4.1): 43 slices. M8-\*, R-v1-cut and the G-v1-\* gates.
- **P-v052-manifest** (`2026-11-06-v052-feature-manifest.md` §3.4): X-consent-gate-052 (below). The manifest proposes no other P-v052-cut entry.

No id was defined twice across the plans, and no pasted id was already in `slices.yaml`. Every slice dep names a slice that exists, and no plan had a typo.

## The runner's additions
- **(a) IRC-0 depends on NOS-2a.** NOS-2a is the routed, guarded TCP connect that both IRC-0 and NOS-2b call (v08 plan Q-r15).
- **(b) X-consent-gate-052:** kind core, model opus, deps `ext:v051-soak-verdict`, with the notes "/security-review; runner call on P-v052-manifest Q4; lands before P-v052-cut". P-v052-cut now depends on it. The plan names no owns. The consent check lives in `Control::update_source` (`crates/pipeline/src/control.rs`, `consent::need_in` under the store lock), not in the API handler, so the slice owns `crates/pipeline/src/control.rs` (enable path), `crates/pipeline/tests/control.rs` and `crates/lodestar/tests/consent_gate.rs`, and depends on X-tcache-recheck, which owns control.rs's disable paths.
- **(c) E-v08 depends on E-v06-companions as well** (v08 plan §5.1).
- **(d) Nothing shows as ready before the soak verdict.** A script went over the whole graph, using the live dashboard's merged states. It found four new slices with no other unmet dep that lacked `ext:v051-soak-verdict`: GS-gate, RX-0, ENR-9 and UN-0. They now carry it. Every other new root already did. No existing slice needed it: B-import-server-2 is held in runner-state, SRC-15b waits on owner Q1, and the slices running now were left alone (X-search-files-6, RP-G, X-claim-store-polish, X-tcache-recheck, X-sources-low and X-sp4-followups).

## Corrections to the pasted copies
- **M8's dependencies on epics also name each epic's gate.** An epic counts as met once it's expanded, and this PR expands E-M6, E-v06-workers, E-v06-companions, E-M7, E-v08 and E-M8. Without the gates, M8's reviews and audits would show as ready as soon as the soak verdict lands, long before the code they review exists. The v08 plan states this rule and follows it for PL-0 (WK-G) and CW-0 (CMP-G). The M8 plan didn't, so its entries keep their epic deps and also gain the matching gate:

  | Epic | Gate added |
  |---|---|
  | E-M7 | G-v07 |
  | E-v06-workers | WK-G |
  | E-v06-companions | CMP-G |
  | E-M6 | G-m6 |
  | E-v08 | G-v08 |
  | E-v06-sources | SRC-G, RP-G and ENR-G |

  This affects 15 entries: M8-PERF-2, M8-CHAOS-1, M8-POL, M8-PURGE, M8-DR, M8-SEC-2 to M8-SEC-8, M8-DOC-1, M8-DOC-3a and R-v1-cut. It only makes them wait longer.

## Updated in place
- **Slice entries:** E-v08 (deps, addition c) and P-v052-cut (deps, addition b).
- **`dashboard.yaml`:**
  - `expanded` gains the six epics above, and `held` is now empty. The soak hold now sits in the slices themselves.
  - The `ws-workers` rule moves above `ws-web`, whose `W*` pattern would otherwise catch WK-\*.
  - The workstreams ws-workers, ws-companions, ws-m6, ws-m7-todo and ws-v10 have new descriptions.
  - The roadmap steps unhold, v052, workers, comp, m7, v08 and m8 now list the slices instead of the epics. Two steps are new: m6 (packaging) and v1 (the 1.0 release candidate and its gates).
  - The `G-10` gate is replaced by G-v1 and the G-v1-\* gates (M8 plan §4.5).

## New dashboard entries
- **Placement and workstreams:**
  - P-v07 is split into seven waves: ws-m7-w1 to ws-m7-w6 and ws-m7-gates, placed before `ENR-*` in ws-src-plan.
  - CMP-\* goes to ws-companions, and PK-\* and G-m6 to ws-m6.
  - v0.8 has three workstreams: ws-v08-workers for PL and CW, ws-v08-feeds for NOS, LEM and ADH, and ws-v08-gate for DOC-v08 and G-v08.
  - M8 has five: ws-m8-tooling, -chaos, -review, -docs and -release.
  - X-consent-gate-052 goes to ws-cut.
  - A script confirmed that every new slice lands where its plan says and that no existing slice moved.
- **Conditions,** each with `met: false`, and also listed under `external` in `slices.yaml`:
  - owner-v07-q1 to q6;
  - owner-cq1 to cq8 (cq6 is Q15, cq7 Q20 and cq8 Q21);
  - owner-wkq1 to wkq3 (Q1, Q2 and Q6);
  - owner-m6q13 and owner-m6-manual;
  - owner-v08-q1, q3 and q4;
  - owner-m8-q1 to q4, and q6;
  - bench-runner, windows-soak-host and unraid-soak-host;
  - owner-v1-rc (`auto: tag:v1.0.0-rc.1`) and v1-tagged (`auto: tag:v1.0.0`). v1-tagged is listed under `external` because no slice depends on it yet.
- **Gates:** CMP-G, G-m6, G-v07, G-v08, and G-v1 with eleven G-v1-\* gates. G-chaos now takes its status from WK-G.
- **Owner queue item 10:** the planning-wave questions, relayed by the dashboard session with the runner's recommendations.

## Conflicts found
- **G-chaos.** The workers plan (§4.1) gives the G-chaos gate `slice: WK-G`. The M8 plan (§4.5) links G-chaos to G-v1-chaos. The runner keeps both, which refines M8 §4.5 (a runner call, flagged to the owner under §2.19):
  - G-chaos keeps its name ("Remote workers chaos gates"), so it takes WK-G, the v0.6 gate under build-plan §0.6.
  - The 1.0 re-run on the release candidate gets its own G-v1-chaos entry.
