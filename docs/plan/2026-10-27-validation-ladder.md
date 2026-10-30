# Validation ladder and platform evidence

Status: **specified** (DUR-24, DUR-28, DUR-31–32). Execution results belong in the
[baseline ledger](2026-10-27-baseline-execution.md). This document does not certify
a release or start a crawler. Use fresh fixtures and explicit, otherwise unused
ports; never run a write test against the baseline instance.

## Choose the smallest useful first check

| Change | First check | Next gate | When sustained collection is required |
| --- | --- | --- | --- |
| Prose, labels, navigation availability | Documentation sync/lint, message consistency, route fixture | Frontend semantic/build checks; keyboard/axe where behavior changes | No soak for prose alone |
| UI state, cancellation, history, visibility | Deterministic delayed-response, navigation and invalidation cases | Unit suite, route/browser tests, bundle budgets, relevant visual/accessibility review | Only if it changes backend lifecycle or resource use |
| Analyzer/report formatting | Synthetic old/new evidence, missing tails, resets, gaps, fixed-enum privacy | Analyze a preserved real run read-only and compare aggregate totals | No new network soak needed to validate arithmetic |
| Collector supervision | Fake disk/HTTP/clock/process and write failures | Offline packaged fixture with real signal delivery and final publication | Full baseline with frozen collector before relying on its completion evidence |
| Storage migration, deletion, archive upgrade | Transaction rollback/retry, old schema, locked file, restart fixtures | Workspace tests; isolated database/WAL integrity and index reconciliation | Sustained writer/index/shutdown acceptance when lifecycle or growth changes |
| Cache or cancellation overhead | Boundary/accounting tests; fixed matched workload and retained bytes | Search/ingest latency and memory checks at representative sizes | Bounded endurance first; long run if ownership or retention can accumulate |
| Admission, streaming, fairness or transport | Paused-time saturation, cancellation, stale-generation and drain fixtures | Isolated overhead, local network fixture and bounded endurance | Full measured soak for a candidate that passes smaller gates |
| Packaging/update/service | Embedded assets, install layout, capability and rollback fixtures | Native platform startup/setup/health/stop/restart smoke | Platform endurance only if service/update lifetime changed |

Every executed check records source identity, exact command, start/end, exit code,
selected/passed/failed/skipped counts and artifact locations. A zero selected test
count is not acceptance. Avoid rerunning broad suites until a source change,
failure or unresolved concern makes the repeat useful.

## Fast, medium and long runs

**Fast:** formatting, deterministic unit/property regressions and script fault
injection. Check rejection does not consume an attempt, every acquired owner has a
terminal release, and cancellation cannot publish a stale result. Fail here before
spending time on live traffic. Do not use real disk exhaustion as fault injection.

**Medium:** fresh local packaged smoke, finite fixture replay, memory/handle
accounting, controlled slow consumers and repeated stop/restart. Use workload
completion and bounded deadlines, not a fixed arbitrary sleep. Record peak and
retained state after drain. Exercise both healthy and failed shutdown/publication.
This is packaging and lifecycle evidence; disabled public networking does not
validate public-DHT productivity or the six-hour suppression behavior.

**Long:** a new frozen public run, production suppression of 21,604 seconds,
30-minute warmup and at least 24 measured hours ending with a valid observation.
Use it for sustained-network/resource conclusions after the fast/medium gates.
Do not spend 24 hours on every edit, concatenate interrupted runs, or treat a
short run as full baseline acceptance. Retain the 99% coverage and three-minute
gap review thresholds, confirmed graceful stop, and post-stop consistency checks.

## Failure-coverage inventory

| Area | Existing evidence or fixture | Remaining focused scenario |
| --- | --- | --- |
| Settings persistence | Workspace atomic-save/conflict/preview tests | Disk-write failure during acknowledged save; UI reconnect with stale revision |
| Updater | Prepared serialized persistence and restart test | Execute actual Unix test; distinguish failed durable save from acknowledged pause |
| Logging | Windows workspace rotation/privacy tests | Fixture write refusal, rotation failure, full history/file stream resync; collector log-size stop |
| Store/snapshots | V12 deletion journal, rollback and imported upgrade regressions | Repeated locked snapshot retry and crash between scrub and journal acknowledgment |
| Writer/index | Workspace commit/retry tests | Fault at each transaction/commit boundary; reindex after WAL recovery on copies |
| Cancellation | Cold query and detail generation tests | Long enumeration overhead; stale clipboard/import completion after invalidation |
| Collector | Atomic replacement, 21 production-loop injections through all phases, actual Windows Ctrl+C on offline fixture | Revalidate frozen final candidate; full measured baseline and evidence review |
| Ownership | Actor capacity/rejection and pool finder/drain tests | Saturated downstream terminal delivery, timeout+eviction race, long-run zero-owner drain |

## Cross-platform matrix

| Environment | Required fixture/gate | Artifacts and limits |
| --- | --- | --- |
| Native Windows | Full Rust/Clippy; frontend; Chrome functional/axe/keyboard; `smoke_windows.ps1` on loopback 18485 | Preserve smoke.json, private logs, metrics, executable/index hashes; Windows pixels do not replace Linux snapshots |
| Supported Unix/Linux | Nonzero selected updater ordering/restart regression; full workspace gate | Record exact test name and count; Windows cfg exclusion is not a pass |
| Pinned Linux browser image | Project's Playwright 1.56.1 image, committed visual baselines, responsive/theme/motion cases | No snapshot replacement to hide unexplained changes; record image identity |
| Portable Windows/MSI | Existing portable/MSI builders and service smoke, fresh install fixture | Embedded UI, setup claim, protected data/log ACL, graceful service stop, upgrade/rollback; never install over the soak |
| Linux tarball/systemd | Existing tarball builder/smoke and service unit | Executable permissions, UI, health, setup, SIGTERM stop, restart survival |
| Docker | Existing Docker packaging, isolated fixture data and explicit ports | Frontend-before-Rust, non-root ownership, healthcheck, 60-second stop allowance; only after the native soak stops |
| Unraid | Three XML validators; later isolated app install/update checks | Port/path defaults, graceful stop, persistent data ownership, VPN template reachability |

On this PC, compilation, browser suites, package installation, Docker, scale
fixtures, fuzzing and benchmarks stop during the measured run. Source preparation,
formatting and small deterministic scripts remain useful. Resume deferred runtime
checks only after confirming both collector and crawler have actually stopped.
