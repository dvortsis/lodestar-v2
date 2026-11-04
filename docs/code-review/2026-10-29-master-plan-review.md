# Review of Claude's soak-to-release master plan

Reviewed October 29, 2026, America/Denver. Scope: the supplied attachment headed
"Master plan update: from the CR-0 soak to v0.5.1, the crawler program, imports,
then 1.0". This is a review, not approval to rewrite history, restart automation,
change the running soak, or implement the proposed program. Owner answers quoted
in the attachment are treated as reported decisions, not independently verified
records of another session.

Assessment: the phased landing, explicit safety fixes, cloud-only development,
import pilots and unchanged bundle limits are useful. Revise the plan before
using it as an execution contract. Findings below distinguish factual errors,
unsupported inference and missing implementation/acceptance requirements.

## P1 — Restart shortcut needs a durable lifecycle protocol

Plan locations: Part A2, lines 61–65; Lane C, line 129.

A versioned atomic snapshot and clean marker alone do not establish safe reuse.
Concrete failure: run A exits cleanly; B loads its marker and sends new sampling
requests; B crashes; C sees A's still-valid-looking marker and repeats requests
whose obligations were only known to B. A crash after invalidating the marker but
before that invalidation is durable is another relevant boundary.

Require exclusive ownership; durably consume/invalidate the prior clean generation
before any new sampling send; stop sends and settle or conservatively reserve
in-flight obligations before shutdown publication; preserve every unexpired
restriction without capacity-based eviction; and publish clean state only after
successful persistence. Test disk full, rename/fsync failure, truncated state,
unknown version, stale marker, repeated restart, forced termination and concurrent
launch. Specify conservative clock handling across reboot/clock changes, snapshot
age, instance/egress identity and copied/rolled-back data. Restoring a database
backup must not restore permission to disregard more recent sampling obligations.

"Sample immediately" must mean only currently eligible responders. It must not
erase restored cooldowns. The existing startup fallback and the last permitted
send plus query-timeout horizon remain necessary when persistence is uncertain.
This is a persistent-format and recovery change even if it adds no SQL migration.

Protocol basis: [BEP 51](https://www.bittorrent.org/beps/bep_0051.html) defines the
responder's sampling interval and the six-hour maximum. It does not specify the
restart shortcut or prove the proposed persistence protocol.

## P1 — Throughput and latency conclusions overstate the evidence

Plan locations: Context lines 26–28, Part A1–2, especially line 59.

The arithmetic 718/23 is approximately 31, but those historical windows are not a
matched controlled comparison. The published checkpoint expressly disclaims a
demonstrated speedup. The external download is also a possible network/host
contention confounder, even though the owner has stopped it.

32 * 3,600 / 4,640 = 24.83 seconds assumes all 32 slots are continuously occupied
and that the completion count matches that ownership population. Capacity is not
time-averaged occupancy. At the earlier checkpoint, peer requests were at capacity
in only 294 of 951 baseline samples. Cancellations, observation boundaries and
family/logical accounting also matter. This calculation is not a measured mean
lookup duration. Likewise, stored/completed lookups is a window ratio, not a
completed-cohort conversion estimate.

Change these statements to hypotheses. Measure admission-to-first-peer,
admission-to-terminal, cancellation, occupancy integral, first-peer-to-verification,
and writer/index lag with bounded histograms and defined populations. Distinguish
time budget spent waiting from useful work. Admission saturation is demonstrated;
exclusive attribution to lookup latency, with sockets/CPU ruled out, is not.

Evidence: `target/worktrees/soak-followups/docs/soak/2026-10-29-native-cr0-checkpoint.md`
and its saved analysis. Do not turn descriptive throughput into a causal claim.

## P1 — Admission coordination is missing from the implementation sequence

Plan locations: Part A2; Lane C; post-soak crawler program.

The strongest new evidence is approximately 63.7M actor rejections, 32.8M full
command-channel events and 98.9% pre-service evictions. The plan proposes streaming,
hints and a larger actor cap, but does not schedule the bounded admission/credit
and wakeup design already specified by DUR-02–04. CR-2 fairness is not by itself an
implementation of that contract.

Add an explicit admission-coordination slice with bounded waiters, coalesced
wakeup, cancellation/eviction cleanup, generation fencing, pause/resume and fair
access for retained work. Evaluate it independently. Keep cap sizing conditional
on measured resource and protocol behavior.

Streaming does not necessarily shorten lookup ownership: the existing contract
retains the slot after an early peer batch and releases it at terminal completion
or cancellation after verified fetch success. It may improve overlap and yield
without producing the asserted 8.4x lookup-rate increase. Hints have their own
success distribution and guards. Do not promise that either solves the observed
rejection churn.

Evidence: `target/worktrees/soak-followups/docs/plan/2026-10-27-streaming-and-hints.md`
sections "Independent lifetimes" and "Responder hints"; the sibling
`2026-10-27-crawler-experiment-protocol.md` section "Candidate backpressure contract".

## P1 — Six-hour A/B experiments are underspecified and combine changes

Plan locations: Part A3, Lane C, Part D crawler program.

The current contracts require streaming and hints to be measured separately.
One combined switch cannot attribute improvement or regression. Use independent
flags for baseline, streaming, hints and their later combination; likewise keep
cap, admission, retention and transport changes separate.

Define six hours as measured time after suppression/warmup. Specify sequential
arms, replicated/counterbalanced order, initial database/queue/routing state,
cooldown transfer, carryover handling, frozen configuration and flag observability.
Two simultaneous crawlers on the same host/egress would contaminate the comparison
and potentially violate shared responder obligations; changing ports does not
isolate them. Switching one process without draining old ownership and recording
the state transition also contaminates treatment boundaries.

A same-binary switch reduces build variation but does not remove time-of-day,
network or state-history bias. Add offline simulator validation against observed
distributions; a simulator can validate accounting but cannot predict real-network
throughput on its own. Freeze practical improvement thresholds before results.

Define the 100/min gate precisely: unique hash-verified durable public crawl
records, deduplication, filtering, imports/recovery exclusions, searchable lag,
24-hour denominator including failures, and whether "sustained" means daily mean
or a minimum in specified shorter windows. Pin bitmagnet configuration, resource
limits, transport, initial state and counting semantics as well as its version.

## P1 — Soak acceptance does not automatically transfer to the release

Plan locations: Lane L, especially line 111; Part D v0.5.1 release gate.

Landing the preserved changes with review is reasonable, but "merge as it is"
must not require landing a known blocking defect. Fix blocking findings before
merge and classify the validation consequences. A note that main differs from
the soaked binary is traceability, not validation of the new behavior.

Require a final release SHA, dependency lockfiles, configuration and artifact
hashes; map each post-soak change to tests/re-soak requirements. Restart/cooldown,
lookup lifecycle, migration, filtering and writer changes deserve explicit
integration acceptance. Decide whether CR-0d/imports/Settings enter v0.5.1 at all;
concurrent landing on main otherwise expands the release beyond its listed gates.

F-metainfo is listed for implementation but omitted from the v0.5.1 gate. Mark it
required or explicitly deferred with the compatibility consequence. Linux/Docker
sustained validation is positioned later than v0.5.1, while only a smoke gates the
primary deployment. Either move that soak before release or explicitly accept and
document the limited platform evidence; a smoke is not a sustained soak.

Also separate BEP 47 parser requirements: symlink length may be omitted; padding
path may be omitted. The phrase "symlinks and padding files without a length or
path" must not authorize arbitrary omission of both fields for both kinds.
[BEP 47](https://www.bittorrent.org/beps/bep_0047.html) states those cases separately.

## P1 — Import scope reduction drops essential correctness contracts

Plan locations: Part A6; Lane I; post-soak imports.

CLI-first and PostgreSQL-first are reasonable scope options. They do not eliminate
the need for destination-transaction acknowledgment, a durable cursor committed
with records/provenance/counters, idempotent crash replay, source snapshot identity,
bounded parent/file reads, tombstone/filter rechecks, or truthful pause/cancel
semantics. Preserve these as acceptance criteria in IMP-1/4, not implicit details.
The current adapter reads PostgreSQL text into Rust Strings and calls Store
directly; adding job tables alone does not establish writer-share scheduling.

"Both carry file lists" does not establish that every record has complete or usable
metadata. Current Bitmagnet conversion explicitly handles no_info/over_threshold
and partial lists and rejects empty names. Keep verification, reported file
completeness and recovery eligibility separate. Preserve private/public/unknown
policy and prevent a public duplicate from clearing restrictions. Persist hash-only
candidates and reasons even if CR-3 network execution is deferred. Unknown-source
records must not silently become public recovery work.

Recovery deferral is a product-scope reduction relative to the existing plan's
explicit missing-metadata-recovery goal. Name it as such: import completion is not
recovery completion. Explain what incomplete records users can search or inspect.
Use a migration compatibility/backfill contract if eligibility arrives in V14 after
V13 imports; otherwise work can be omitted or selected twice during upgrade.

Evidence: `docs/plan/2026-10-29-magnetico-bitmagnet-migration.md` sections 6–8;
`target/worktrees/soak-followups/crates/sources-importers/src/bitmagnet.rs`.

## P1 — PostgreSQL restoration needs its own bounded operational slice

Plan location: Part A6, line 80.

"The owner restores once" moves complexity out of the adapter but leaves an
unplanned prerequisite. Define who prepares the database, exact Bitmagnet schema
version, extensions, encoding, restore failure policy, disk reserve, isolation and
verification of row/file counts. Use a copy of the physical cluster, never the
only source copy. A PG16 directory and a PG14 logical dump require different
procedures. Start with a small representative restore and adapter read.

The observed dump is SQL_ASCII. Non-ASCII bytes are not guaranteed valid UTF-8;
successful restoration does not prove safe decoding by the Rust client. Define
byte-preserving extraction/decoding and issue reporting before running the full
corpus. Restoration executes supplied SQL, so its isolation/trust review is an
explicit replacement for the safe-parser boundary, not a free simplification.

Primary references: PostgreSQL [logical dumps](https://www.postgresql.org/docs/16/backup-dump.html),
[physical backup requirements](https://www.postgresql.org/docs/16/backup-file.html),
[SQL_ASCII semantics](https://www.postgresql.org/docs/16/multibyte.html), and
[Bitmagnet restore/version guidance](https://bitmagnet.io/guides/backup-restore-merge.html).

## P2 — Storage and scale estimates are misstated and incomplete

Plan locations: Part D line 193; Part E line 219; Part A6 line 91.

Architecture section 3 calls 2.3 KB/torrent an **all-in target** for canonical store,
torrent index and smart file index at a 30-file average. It is not measured SQLite
size to which indexes should then be added. New provenance/membership/candidate
tables and the real corpus may exceed it. The 50 microsecond filter calculation is
arithmetic at a budget, not a measured import-time estimate for these records.

Build a peak-space envelope covering retained sources, PostgreSQL restore/indexes,
WAL/temp files, Lodestar data/indexes, provenance, old and new search indexes during
rebuild, compaction and backup. Measure long-tailed file counts and encodings, not
only row count. Corpus counts and overlap are advertised, not verified. Capacity
approval must precede even the 50M trial; final revised budgets can follow it.

B5b provides a rebuild mechanism, not proof that an arbitrary new schema is safe.
Require old-index compatibility, deletion/filter consistency while the old index
serves, catch-up correctness, interruption/disk-full tests, swap/rollback behavior,
and rebuild reserve. Define how a provenance-dependent query behaves before the
new index is ready. Bound Low-priority import work by bytes/time as well as rows;
a huge file list can monopolize the writer inside a nominal 50% transaction share.

## P1 — History replacement and branch accounting need corrections

Plan locations: Context line 14, Part B, Lane L1.

Local comparison against the fetched `origin/main` shows changes in **both**
`docs/owner-decisions.md` and `docs/architecture.md` for `058d9937`.
The assertion that only owner-decisions differs is false. Record and review the
exact intended tree delta before the owner-authorized replacement.

Preserve a private recovery bundle and old/new ref mapping; stop branch writers
and runner automations; use an expected-old-SHA lease instead of a blind force;
restore protection and required checks immediately; audit tags/release refs and
branch bases; and only then resume the runner. Do not put scrubbed historical
material back on a public backup branch. The mention of GitHub cached PR objects
is useful, but does not substitute for this cutover/rollback procedure.

L1 omits `23a986fd`, which contains the root review/research material and both
October 29 import plans pushed in this session. Inventory every pending head and
map each changed path/commit to a landing slice or explicit exclusion. Avoid a
blind range cherry-pick: L4's range overlaps the separately listed `7b69eab1`, and
L6's `18a02564` also contains storage-test and search compatibility fixes. Documents
landed first must not advertise pending code as shipped or link to absent files.

## P2 — CI green needs an explicit check-to-requirement mapping

Plan locations: Part A5; Verification.

Current `ci-ok` depends on Rust-oriented jobs; it does not aggregate the separate
web and E2E workflows. Require their check names on the final SHA, including the
Unix updater case and targeted new tests, rather than relying on `ci-ok` alone.
Verify selected tests actually executed instead of being skipped by path filters
or conditional configuration.

The E2E workflow regenerates snapshots when its marker/input is enabled and runs
the all-theme sweep on a different trigger from ordinary PR checks. Require a
comparison run with regeneration disabled, review changed baselines, and schedule
the coverage the plan actually promises. Generic green CI cannot establish native
zoom, assistive-technology acceptance, package behavior or a live-network soak.

Evidence: `.github/workflows/ci.yml` job `ci-ok`, `web.yml` and `e2e.yml`.

## P2 — Parallelism and collector ownership need explicit integration gates

Plan locations: Parts C/E and runner restart instructions.

Lanes C, I, F and S overlap actors/pool, writer/store/schema, auth/filter contracts,
metrics and UI bundles. Four helpers is a concurrency limit, not conflict control.
Add file/contract ownership and a dependency DAG: L2 before crawler candidates,
L3 before import durable outcomes, L5 before B-update, IMP-1/4/5 together before
any measured pilot, and shared web dependencies before Settings reconciliation.
Assign migration numbers at integration, even with one migration writer.

CR-COL is more than a mechanical language port. Preserve process/container identity,
signal delivery, bounded shutdown, valid final measurement, atomic evidence writes,
disk monitoring across actual Docker volumes, restart detection and uncertainty
after reporting failure. Port the fault-rehearsal suite and compare output with
the Windows analyzer before trusting Docker results. Disable automatic container
restart for acceptance. Require a correctness review of the port.

## Suggested approval conditions

1. Correct the history, throughput, latency and size statements.
2. Add the crash-safe restart protocol, admission-coordination slice and separate
   experiment treatments with defined metrics and state boundaries.
3. Define the v0.5.1 cutoff/final candidate, behavior-change revalidation matrix,
   parser decision and Linux/Docker evidence level.
4. Preserve import durability, source/privacy/quality contracts and add a restore
   preparation slice plus peak-capacity preflight.
5. Complete commit-to-slice coverage and exact CI/collector acceptance checks.

No running process, source implementation, branch history, remote setting or
automation was changed during this review. No new builds or live experiments ran.
