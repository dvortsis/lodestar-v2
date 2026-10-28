# Hardened baseline execution ledger

Owner authorization: October 27, 2026. Integration branch `work/soak-followups`,
starting at `5e1a53c0`, including the support and prepared product/follow-up stack.
This ledger supersedes historical statements that the October 26 soak is running.
No subsequent soak has started. No push, merge, deployment or Docker restart is
part of the current execution. The original evidence is preserved.

## Pre-soak work

| IDs | Status | Evidence / remaining work |
| --- | --- | --- |
| PRE-01–03 | Checks passed | [Incident](../soak/2026-10-27-interrupted-cr0.md); complete private manifest hashes 1,567 files; copied DB/WAL/SHM passes integrity and foreign-key checks |
| PRE-04 | Investigation recorded | C: recovered approximately 141 GiB before this work; application logs under 1 MiB; source of historic disk growth not established |
| PRE-05–06 | Inventoried; storage procedure specified | [Workspace and storage record](2026-10-27-build-storage.md); four worktrees preserved; Cargo output 23.4 GiB; no cleanup performed |
| PRE-07–09 | Harness checks passed; final package pending | Disk reserves, log cap, frozen inputs, protected shutdown, healthy endpoint, schema/provenance; Docker/process/port preflight and minimum durations enforced; final source review recorded; clean-commit package command added |
| PRE-10 | Checks passed | Analyzer missing-result handling, freshness, fixed-enum breakdowns, hourly deltas, disk history, per-metric coverage and confirmed shutdown; 24 Python regressions |
| PRE-11–13 | Checks passed on Windows | [Counter contracts](../decisions/2026-10-27-soak-diagnostics.md); actor tests and 19 pool cases including full/closed finder channels, eviction before service/after fetch, rediscovery versus retry; entry size 72 bytes unchanged; isolated added counter median 4.23 ns/event |
| PRE-14–17 | Windows checks passed | Full workspace: 2,120 passed, no failures; 17 swarm migration cases passed after correcting the V4 fixture. Clippy caught and fixed a minimum-Rust-version violation in cancellation polling |
| PRE-18 | Cross-platform pending | Ubuntu exists, but cargo/node absent from its default PATH; native Windows excludes updater engine |
| PRE-19–22 | Windows checks passed | 124 frontend files, 2,311 passing tests, one Windows file-symlink privilege skip. 503 browser core/keyboard cases pass, 11 skips; 185 auth/result-row tests rerun after latest fixes; semantic/lint and e2e type checks pass |
| PRE-23 | Windows checks passed | October 28 corrected r16 build and all unchanged bundle budgets pass; 29 resolver tests, full check, E2E types and 30 focused browser cases pass; refreshed release packaging smoke passes |
| PRE-24 | Checks passed | 21 production-loop rehearsals with fake process/HTTP/clock/disk and injected write/cleanup failures; atomic replacement injections; 28 analyzer/preservation cases |
| PRE-25 | Checks passed for r16 candidate | Frozen UI hash matches; fresh offline fixture health/metrics/no UDP checks pass; Ctrl+C confirmed exit 0 in 1.55 seconds. Prior r14 evidence and executable preserved |
| PRE-26 | Pending | Docker running; final source review recorded; source-qualified package preparation and post-Docker storage/ownership recheck pending; public crawler remains stopped |

## Verification rules

Every gate records the command, exit status, selected tests, and any environment
limitation. Logs stay under this checkout's ignored `target/`. No check passes on
an empty selected test set. The initial npm install hit a sandbox cache denial;
an approved retry with a local cache succeeded. WSL enumeration and complete log
inventory likewise required approved access retries. No approval was rejected.

The first focused frontend run passed 863 tests and failed five. Three cold lazy
loads passed in a focused serial diagnostic run; the remaining rapid-removal case
exposed both an abandoned query timer and a fixture that did not actually hold
its first response. Keep full rerun results separate from this diagnostic run.
The initial focused Rust run failed the V4 migration fixture, so it is not a green
gate even though individual prepared-fix tests passed.

The subsequent full Windows workspace run passed 2,120 cases across 140 test and
doc-test groups (`target/rust-workspace.log`); groups selecting zero cases are not
evidence for Unix-only updater tests. Strict workspace Clippy passed after replacing
`usize::is_multiple_of` with equivalent arithmetic compatible with the declared
minimum Rust version. The focused pool run then passed all 17 cases. The full
frontend rerun passed 2,311 cases in 124 files with one explicit Windows privilege
skip for real file-symlink creation (`target/web-all-r8.log`). Svelte semantic checks
have zero errors and three existing CSS-directive warnings. The e2e type check
passes after including SvelteKit's ambient declarations.

The Vite barrel optimizer compared native Windows paths against normalized module
IDs and did not run correctly. Normalizing those identities restores optimization:
production search JS is 41.23 kB and its vendor chunk 30.76 kB (limits 45/31.1 kB).
All route/detail budgets subsequently passed (`target/web-size-r8.log`) after grouping
icons and key helpers used together, sharing content labels and deferring clipboard
code until an action. Detail on-demand accounting includes the deferred code; no
numeric limit changed. The refreshed production build also passes (`target/web-size-r12.log`).
Windows Chrome browser checks ignore Linux pixel snapshots; they cannot satisfy
the pinned Linux visual gate. No visual baselines or budget limits were changed.

Security source review of this change raised three collector/reporting issues:
unconfirmed exit reported as stopped, hidden publication failure, and wait errors
bypassing forced-stop fallback. All three have source fixes and injected tests;
source review alone does not establish actual Ctrl+C delivery on Windows.

Actual Windows rehearsal exposed two further shutdown hazards: attaching the
supervisor to the child console could interrupt its reporting, and an inherited
ignore-Ctrl+C flag could make the child ignore a successfully generated signal.
An isolated signal helper and clearing that inherited flag before spawn resolve
both. The r8 packaged fixture records delivered signal, confirmed exit code 0,
no cleanup errors and 1.16-second shutdown (`target/packaged-smoke-2026-10-27-r8/
smoke.json`). Earlier failed fixtures remain preserved; their orphaned children
were stopped only after verifying executable and start identity.

The broad Windows Chrome core/keyboard run selected 512 cases: 497 passed, 11
skipped and four failed (`target/web-browser-r8.log`). Failures concern an initial
module fetch, hidden mobile alert timestamp, auth-page layout measurement and an
unmocked hash lookup after detail reload. Those failures have been investigated.
The focused follow-up exposed a real auth loading shift in addition to the mobile
regression. Auth now reserves the real form size while its controls remain hidden
and inert. Browser fixtures explicitly model the intended session and the hash
lookup on detail reload. The full rerun passed 503 cases with 11 skips and no
failures (`target/web-browser-r11.log`); the original assertions and Linux
snapshots remain intact.
Neither this run nor ignored snapshots satisfies pinned Linux visual acceptance.
The latest disk check reports 116.6 GiB free on C:. Docker Desktop and backend
processes are now running; HTTP 8484 and UDP 6881 are free, and no native Lodestar
process was observed. Docker workload ownership remains unknown because its CLI
was unavailable on PATH and at the standard installation path. No Docker action
was taken. Launch is blocked by the agreed stopped-Docker requirement.

The integrated collector rehearsal (`target/rehearsal-r4/checks.json`) passed all
21 scenarios using the unchanged production collection loop and a private common
module with fake external adapters. No process or public HTTP request is launched.
Exact and failed-then-later endpoints span 86,400 and 86,460 simulated seconds;
an unhealthy endpoint with readable metrics also waits for a later healthy sample;
these validate the boundary logic, never count as real soak hours. Adapter setup
failure is now guarded so it cannot suppress termination reporting. Direct
atomic-write fault injections and the real process smoke remain separate gates.

The refreshed production UI passes all bundle budgets (`target/web-size-r12.log`).
Strict workspace Clippy passes (`target/rust-clippy-r14.log`). An earlier concurrent
Clippy/Vite run failed because Vite replaced the UI directory while Rust embedded
it; that failed attempt is not evidence. Rust now uses the frozen
`target/packaged-web-r12` copy. The rebuilt r14 release passed the offline smoke,
with confirmed exit 0, no cleanup errors and 1.505-second shutdown
(`target/packaged-smoke-2026-10-27-r14/smoke.json`). Its executable SHA256 is
`294023308FF907D3D82FC42E8B2D73D8247A6AAA235E6A1393EF2C855A9C03BA`.
This is a validation candidate from an uncommitted integration checkout, not a
source-commit-qualified launch packet.

The isolated release diagnostic check passed two selected tests. Entry layout
remains 72 bytes with and without the new flags on this target, so the flags add
zero retained entry bytes at the 200,000 cap. The added aggregate counter payload
is 64 bytes per pool and 48 per actor. Nine alternating million-event trials
measured 8.39 ns/event for the existing increment and 12.62 for aggregate plus
category, a 4.23 ns added median (`target/diagnostic-cost-r12.log`). This is
uncontended instrumentation cost; integrated resource review is still required.

## During-soak queue

No DUR item is accepted merely because it appears here. The next measured run
must keep its executable, collector, configuration and data unchanged. Same-host
work is restricted to source preparation and lightweight checks. The following
is the authorized queue, in priority order, with its required deliverable.

| ID | Deliverable | Current state |
| --- | --- | --- |
| DUR-01 | Aggregate checkpoints: startup, suppression end, baseline start, measured 1/4/8/16/24h | Waiting for next run |
| DUR-02 | Admission churn causal model and deterministic reproduction | [Model and three replay cases checked](2026-10-27-crawler-experiment-protocol.md); next-run causal attribution pending |
| DUR-03 | Bounded admission/backpressure candidate specification | Candidate constraints specified; ownership design pending DUR-02 |
| DUR-04 | Saturation/recovery/pause/cancel/drain fixtures | Pending DUR-03 |
| DUR-05 | Streaming ownership and terminal-completion specification | [Specified](2026-10-27-streaming-and-hints.md); candidate bounds require review |
| DUR-06 | Responder hint TTL, rate units, guards and fallback specification | [Specified](2026-10-27-streaming-and-hints.md); candidate bounds require review |
| DUR-07 | Streaming/hint race and full-channel fixtures | [Scenarios specified](2026-10-27-streaming-and-hints.md); executable fixture source pending |
| DUR-08 | Retention/service analysis including cold work | Waiting for diagnostics |
| DUR-09 | Admission, retention and socket fairness specification | Pending DUR-08 |
| DUR-10 | Durable partial-upgrade producer and writer outcome specification | [Specified](2026-10-27-durable-partial-upgrades.md); service/retry parameters and concrete lease mapping remain gates |
| DUR-11 | 100,001-row and crash/saturation/provenance upgrade fixtures | [Scenarios specified](2026-10-27-durable-partial-upgrades.md); executable fixture source pending |
| DUR-12 | Separate experiment protocols and variance-based comparison templates | [Specified](2026-10-27-crawler-experiment-protocol.md); variance thresholds wait for observations |
| DUR-13 | Production Settings shell and section routing | Source prepared in separate `work/soak-settings`; navigation/mobile shell checked; field search, overview and bundle gates remain |
| DUR-14 | General/Crawler/Network/Storage/Search settings through existing form engine | Core forms and General saved-settings export source prepared in `soak-settings`; Network explicitly read-only pending address recovery; config view/import/overview and bundle gates remain; not accepted |
| DUR-15 | Settings conflict/validation/reconnect/restart fixtures | Settings unit suite 341 passed; refreshed full frontend 2,352 passed/one Windows skip in 130 files; 42 Windows browser checks passed including twelve Settings cases, boot/error behavior and three held-font setup checks. `soak-settings/target/bundle-accounting-browser-r3.log`; E2E types/check/build passed. Original hook timeouts and intermediate browser failures preserved. Settings bundle gates remain failed |
| DUR-16 | Five Insights stage pages through existing contracts | Pending |
| DUR-17 | Stage null/estimate/cohort/CSV reconciliation fixtures | Pending DUR-16 |
| DUR-18 | Logging settings and bounded live/history/files UI | Pending |
| DUR-19 | About/Updates screens with platform capability handling | Pending |
| DUR-20 | Syntax/help destination and verified examples | Pending |
| DUR-21 | Shared route availability for navigation/palette/remediation | Pending screen work |
| DUR-22 | Localization, keyboard, focus, reflow, reduced motion | Pending screen work |
| DUR-23 | Windows module and CRLF validation portability | Source prepared during preflight; October 28 bundle resolver now includes nested layouts/errors and fails on missing inputs; 29 script regressions pass in Settings worktree |
| DUR-24 | Focused failure-coverage inventory | [Specified](2026-10-27-validation-ladder.md); execution remains separately gated |
| DUR-25 | Storage/WAL/index recovery fixtures | Pending |
| DUR-26 | Updater persistence-error semantics specification | [Specified](2026-10-27-persistence-and-import-audit.md); implementation/execution deferred |
| DUR-27 | Read-only historical import audit design | [Specified](2026-10-27-persistence-and-import-audit.md); implementation/execution deferred |
| DUR-28 | Fast/medium/long validation ladder | [Specified](2026-10-27-validation-ladder.md); execution remains separately gated |
| DUR-29 | Matched performance/overhead/memory/disk harness preparation | Pending |
| DUR-30 | Targeted existing-parser fuzz corpus additions | Pending |
| DUR-31 | Windows/Linux/Docker/Unraid matrix and selected-test assertions | [Specified](2026-10-27-validation-ladder.md); execution remains separately gated |
| DUR-32 | Packaging/upgrade/rollback/service smoke preparation | [Specified](2026-10-27-validation-ladder.md); execution remains separately gated |
| DUR-33 | Documentation matching implemented product behavior | Pending screen work |
| DUR-34 | Reviewable slices, dependencies and PR descriptions | In progress; no publication |
| DUR-35 | Release evidence ledger with executed/deferred limits | This ledger; ongoing. Older bundle measurements undercounted root error bytes; corrected r16 frontend/package validation passes. Final provenance/review and cross-platform gates remain |

## Launch and acceptance boundaries

October 28 final-preparation review is recorded in
[the source review](2026-10-28-pre-soak-source-review.md). The integrated collector
rehearsal was rerun successfully: all 21 scenarios pass in `target/rehearsal-r17`,
with no application or network started. The Python suite passes 37 tests,
including nine new package/provenance cases (`target/python-preflight-r17.log`);
the collector injection assertions pass (`target/collector-preflight-r17.log`).
The new preparation command refuses dirty/changed source and preserves failed
outputs. Its successful end-to-end package run is still required before launch.
Current Docker image/log size inventory is in the storage record; it establishes
current consumers, not the historical exhaustion cause or a measured growth rate.

October 28 shared follow-up: the Settings investigation found the bundle resolver
omitted the root error node and could misread nested layouts/errors. Missing
build/manifests now fail instead of producing partial measurements. Root error
preload lists use the existing stripping mechanism. Shared scripts, the root
preload configuration, shorter EN/ZH setup guidance and cold-font fixtures were
copied from `work/soak-settings`; its `target/bundle-shared-files.json` lists the
seven files. No Settings feature code or its required-controls gate was copied
into this baseline, whose Settings screens remain placeholders.

Settings validation passes 42 Windows browser cases including three deterministic
held-font cases at 1280/390/320px. The first run's 38/39 and intermediate 41/42
results are retained in that worktree. The failure was a font-dependent line-wrap
change, reproduced after ordinary repeats passed; shorter copy preserves all
token/paste instructions. These fixes do not change crawler behavior. r14's frozen
UI/executable and original evidence are preserved. Its earlier shutdown evidence
is historical; refreshed packaging evidence is recorded below.

The baseline r16 production build passes corrected bundle measurements
(`target/web-build-r16.log`, `target/web-size-r16.log`): initial Search JavaScript
42.37KB/45KB, total first load 85.26KB/100KB; all other existing gates pass too.
Its two resolver test files pass 29 cases (`target/web-bundle-tests-r16.log`).
Full frontend checks and E2E types pass (`target/web-check-r16.log`,
`target/web-e2e-types-r16.log`); only three existing Discover CSS warnings remain.
The baseline's own focused Windows browser run passes all 30 cases in 53.3 seconds
(`target/web-browser-r16.log`), including cold fonts, boot focus and error recovery.
This is separate from Settings' remaining size failures. Preserve the previous
executable at `target/packaged-bin-r14/lodestar.exe` (hash verified against r14's
smoke record). `target/packaged-web-r16` is the new immutable UI build input;
its index SHA256 is
`390E6D16AE6AF8E8948C0389E5C91840D0ADED5CC7A77E428F09C2F01F200A74`.

The r16 offline release build completed in 1m34s (`target/release-r16.log`), using
the existing bounded Cargo directory, four jobs and disabled incremental output.
The packaged smoke passes (`target/packaged-smoke-2026-10-28-r16/smoke.json`):
served UI hash matches, health/metrics available, no UDP ownership, Ctrl+C delivered,
exit 0 confirmed in 1.550 seconds, no cleanup errors. Executable SHA256:
`F48AAB8D087CF759781140F05AAD02BFF65420C160340B61D83476479B3B1BF3`.
An identical copy is retained at `target/packaged-bin-r16/lodestar.exe`.
This remains a validation candidate from uncommitted source, not a provenance-
qualified launch packet or release acceptance. No public-network run occurred.

Run a fresh native Windows baseline with validated fixes and bounded diagnostics,
not the old partially observed binary. Keep random targets, TCP/MSE, 60-second
floor, budgets and admission behavior. Production suppression remains 21,604
seconds, warmup 30 minutes, measured duration at least 24 hours. Require 99% core
measurement coverage and no unexplained gap over three minutes, stable identity,
no resets/cap violations/stalls/writer failures, confirmed clean shutdown and
successful final publication. Throughput is descriptive, not a 720/hour gate.

The next soak is not launch-ready until applicable checks and review pass. Full
Linux visual/updater, Docker/Unraid, fuzz and scale gates remain explicit release
requirements. No automatic restart, cleanup, performance tuning or suppression
bypass. Current Docker workload state is unknown; launch is blocked while its backend
is running. Any handover uses actual stop evidence and the conservative cooldown
rule, never an abandoned expected finish time.
