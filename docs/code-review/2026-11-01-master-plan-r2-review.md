# Review of the finalized master plan r2

Reviewed November 1, 2026 (local time), against `e528c369`, the merge of
PR #369. The plan files use the UTC date November 2. Scope: `docs/build-plan.md`,
`docs/handoff.md`, `docs/decisions/2026-11-02-master-plan-r2.md`,
`runner/slices.yaml`, and the implementation and contracts they rely on.

The remote was refreshed with `git fetch origin main`. The existing working
branch and untracked October 29 review were preserved. Review sources were
extracted into `target/plan-review-r2` from that exact commit. This review does
not change the plans, run a live experiment, approve a release, or operate the
runner.

The direction is substantially better than the October 29 proposal. Release
evidence is separated from CR-0, experiments have independent switches and
repeated measured windows, imports have a restore runbook and staged capacity
gates, and the migration hold and owner decisions are explicit. The graph parses:
179 slices, 23 external conditions, no duplicate IDs, unresolved dependencies,
or cycles. That validates graph structure, not readiness to execute every gate.

**1. P1 — The existing Linux collector discards the evidence the crawler gates require.**

Plan: build-plan lines 172–195; graph lines 438–446.

`docs/soak/tools/linux/collect.py:58–115` allowlists the old CR-0 metrics. It
does not retain `searchable_new_total`, the discovery-to-searchable histogram or
its dropped-measurement counter, `fetch_family_*`, `fetch_no_route_total`,
`fetch_route_*`, or `dht_utp_in_total`. The analyzer likewise only accepts its
old counter list (`analyze_cr0.py:15–26,108–120`), and explicitly disclaims
verified-searchable yield and cohort latency at line 642. The current gate
slices own evidence directories; no scheduled prerequisite owns this tooling
upgrade.

Confirmed with a small execution of `sanitize_snapshot`: input containing the
searchable counter, a latency bucket, dropped latency, IPv6 successes, uTP
datagrams and `writer_stored_total` retained **only** `writer_stored_total`.
Thus a completed Linux A/B can permanently lack its primary outcome and the
mechanism checks needed to prove the treatment was active.

Add a collector/analyzer slice before the live runs. Retain bounded, explicitly
validated labels and histogram buckets, including `+Inf`; calculate searchable
rate, hourly minima, latency coverage and transport/family breakdowns; reject
missing evidence and counter resets. Test it with known passing/failing fixtures
and a short end-to-end rehearsal before booking days of experiment time.

**2. P1 — Reserving schema fields does not populate existing records when later M7 code arrives.**

Plan: build-plan line 108; graph SB-6 at line 460 and SRC-3 at line 408.

SB-6 creates the fields now, while later slices fill them without another schema
bump. `crates/search/src/rebuild.rs:569–600` opens an existing current-version
directory without rebuilding. The live indexer processes new IDs and explicit
`reindex_log` entries (`crates/search/src/index.rs`, `Indexer`). Merely
changing document construction later will not revisit the existing corpus.
This affects source/verification queries, grouping, signatures and regex
prefilters. In particular, a prefilter cannot safely treat an unpopulated field
as proof of no match.

The architecture already provides in-place reindexing for enrichment changes;
the missing part is assigning and gating its use for these new producers.
Each producer needs a resumable population/backfill generation, durable progress,
completion criteria and truthful feature availability during catch-up. Test
SB-6 → import → later producer on an existing database, including restart and
deletion/filter changes during backfill. Either implement producers before the
large import or budget and measure their later full-corpus rewrites. “Indexed
once at 50M” is not yet supported by this sequence.

**3. P1 — Path B has no takeover fence against a surviving or automatically restarted runner.**

Plan: handoff lines 55 and 72; runner/README lines 134–143.

An `updated_at` older than six hours is accepted as evidence that no runner is
live. It can also mean a failed state push or unavailable service. More
concretely, the watchdog is designed to start a replacement after four hours.
Path B neither disables that watchdog nor records ownership in a place it must
honor, and a Path B agent is told not to update runner state. A serial agent can
therefore start a slice while the watchdog or old runner launches overlapping
work. “No open PR” is not a claim: a branch can be under development before a PR.

Require an explicit handover that pauses runner/watchdog launches and records
the current owner, slice and owned paths in a shared claim. Both paths must
recheck it before launches and pushes, and a returning runner must reconcile
unfinished Path B branches. If a non-Claude agent cannot establish runner
liveness, stale time alone should not authorize takeover. This matters for the
actual usage-exhaustion scenario the new handoff is meant to solve.

**4. P2 — The 10M rebuild acceptance path is not specified by the proposed order.**

Plan: build-plan lines 76, 108 and 256; graph G-import-10M at line 452.

Installing SB-6 before importing 10M produces a v6 index as records arrive. It
does not exercise a v5 → v6 blue/green rebuild at 10M. The implementation only
takes that path when it finds an older index to serve. The plan promises both
“SB-6 before the 10M pilot” and “its rebuild measured at 10M” without specifying
the separate fixture and upgrade sequence needed to achieve both.

Add an explicit preparation step: build and checkpoint a 10M v5 corpus/index
using a pinned compatible binary, upgrade a copy to SB-6, then measure the
rebuild, concurrent serving, catch-up, disk high-water mark and interrupted
resume. Keep steady-state v6 import measurements separate. Do not count an
initial v6 indexing pass as blue/green evidence.

**5. P2 — Crawler acceptance needs a frozen measurement specification before collection.**

Plan: build-plan lines 172–178 and 192–195.

A/B windows explicitly exclude suppression/warmup; the 24-hour gate on a fresh
directory does not state its start boundary equally clearly. A first installation
has six-hour sampling suppression, so including it versus excluding it changes
both the mean and worst-hour result. Define fixed versus rolling hours, treatment
carry-over, which events belong to the measured cohort, and treatment of latency
samples from before or after the rate window.

Also define an acceptable latency-observation loss rate before calculating p95.
`docs/metrics.md:799–800` and the CR-gate-metrics decision allow bounded queues to
drop latency samples while still counting throughput. Loss can be concentrated
in congested periods; a p95 from the survivors is not automatically a passing
gate. Specify coverage and missing-data rules, including how unfinished work is
reported.

Checkpoint 1 should report a projection with uncertainty, not call it a measured
ceiling. Isolated gains are not necessarily additive or multiplicative, and
six-hour arms do not establish a 24-hour worst-hour bound. The final combined
soak remains necessary. The failure breakdown supports testing transport first;
it does not identify how many timeouts uTP can recover. CR-4's own decision also
says route backoff should be skipped without baseline IPv6 no-route events and
expects essentially no throughput gain; preserve that conditional in its gate
completion criteria.

**6. P2 — Making the listed web checks required needs a workflow change first.**

Plan: build-plan owner queue item 7, line 135.

The proposed `Web / web` and `E2E / run` labels do not match the current explicit
job display names in `.github/workflows/web.yml:25` and `e2e.yml:40`. Both
workflows also have top-level PR path filters. `runner/README.md:22` already
explains why making the existing path-filtered workflows required would stall
unrelated PRs. The queue presents this as only an owner setting change.

Schedule an always-reporting aggregate or equivalent check design first, with
explicit applicability and failure propagation. Discover the exact emitted
check names from a real run. Demonstrate a web-changing PR and a docs-only PR
before changing protection, and define when backend changes require E2E even
without `web/**` edits. Keep snapshot regeneration separate from comparison.

**7. P2 — Online import acceptance depends on explicitly parked implementation.**

Plan: build-plan lines 79 and 199–202; G-import-* graph entries.

The gate lists 2,000/s offline and 1,000/s online, while B-import-server-2 is
parked pending owner approval. The current CLI refuses writes while the server
runs; B-import-server-1 only completes indexing jobs and shares the filter
loader. IMP-4b explicitly calls 1,000/s a proposed target for the later online
writer slice. A pilot cannot presently demonstrate it through a supported
online path.

Mark the current ladder offline-only and attach online throughput, writer-share
fairness and concurrent search-latency acceptance to B-import-server-2, or make
that implementation a prerequisite. Define import throughput's endpoint:
durably written, indexed, or job complete. Report write time and indexing drain
separately so the synthetic insert result cannot stand in for end-to-end import
performance. Decide how pilot NDCG uses a judged corpus: the existing relevance
binary builds its own synthetic fixture, so simply running it beside a 10M
database does not measure ranking on the pilot data.

**8. P2 — Remaining ordering rules exist only in prose, contrary to the handoff's readiness rule.**

Plan: graph lines 438–445; handoff §3; runner/README step 7.

CR-AB-ADM does not depend on the comparator, although the plan puts the comparator
before tuning. CR-COMB does not depend on CR-5, although CR-5's notes say the
combined run must wait if tuning is launched. The experiment slices have disjoint
`owns`, so path-overlap checks do not serialize their shared host. Host bookings
can enforce this, but must be exclusive run-specific bookings rather than a
once-satisfied external flag.

Also, runner/README still instructs launches up to the YAML's 16+16 limits and
exempts docs from those limits; §0 says four total including docs. The new plan
has authority, but the launch procedure a successor executes still contradicts
it. Add the temporary total cap directly to the launch rules/configuration and
encode optional CR-5 completion-or-explicit-skip and comparator ordering. A
cycle-free graph can still launch work in the wrong order.

**Owner decisions and scheduling risks to prioritize**

- **Release safety:** #357 and #362 are already disclosed, not newly discovered
  omissions. I would prioritize them ahead of cosmetic backlog. Recommend
  including fail-closed filter loading in the first public release, with the
  delta-appropriate validation. For #362, deciding to ignore a configured proxy
  changes privacy expectations; retain an explicit owner decision and user-facing
  egress behavior rather than treating the SSRF guard as a proxy replacement.
- **v0.5.2 scope:** freeze the actual cut and feature manifest before its soak.
  “Cut from main soon” can include V14 and partially landed Sources/M7 work, not
  just the listed fixes. Map all enabled behavior to its own acceptance and
  document excluded/inactive features. The added upgrade/rollback test is useful;
  pin the actual pre-upgrade snapshot and schema, not a loosely named `pre-vN`.
- **Machine time:** each single-treatment ABBA is at least 24 measured hours.
  The seven listed treatments total seven measured days if all run; omitting the
  conditional route experiment gives six. The comparator and combined soak add
  two more days, before suppression, warmup, gaps, repeats, release soaks or
  imports. Add a concrete reservation calendar and interruption/retry budget.
  The four-agent limit cannot shorten this serial host path.
- **Later capacity:** the 50M approval should name the feature set actually
  populated. Passing while trigram/grouping/signature fields are empty does not
  approve their future disk, rebuild and RSS costs. Recheck affected budgets
  when their producers and backfills become active.

**Validation performed:** source inspection at the pinned merge; YAML parse,
ID/dependency/cycle checks; executable collector-sanitization reproduction.
No Rust/web build, full test suite, live crawler run, import, merge, push, release
operation or runner-state write was performed. Findings about future slices are
missing acceptance/design requirements, not claims that unimplemented code has
already failed.
