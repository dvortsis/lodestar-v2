# Native Windows pre-soak completion audit

The applicable Windows pre-soak preparation is complete. **No public soak has
started.** This is readiness for a new integrated reference, not baseline or
release acceptance. Environment observations must be refreshed at actual launch.

## Frozen candidate and launch record

- Build source: `744b84d68c7609d8220430ffc9938c05c6d6a527`.
- Source tree: `cfedc8fbb12db34f773f66eceda6f2d43e55419d`.
- Private package: this worktree's `target/launch-candidate-r21`.
- Executable SHA256: `24CB1EF43D86AD8C0F9F80C41E9D2D661F3FD6FE5573E72C13A87285E11E1475`.
- Embedded index SHA256: `725C490222B3A52FD79E55253FE2353C90149E1DB48A9339D32E12CD3E6E6399`.
- Private launch/configuration review: `target/launch-review-r21.json`;
  SHA256 `1A8FC66FAB1E298B4659F3A042B00AD7A2D9250BBF6CAB1EACD57695299376BD`.

The preparation command built frontend first, passed every unchanged size limit,
copied a frozen UI, built the release binary offline, and checked source identity
before/after building and smoke testing. The package contains its build record,
all UI files, executable and the three collector/signal files. The manifest hashes
them all. A separate verification invocation passed, as did the production
collector's build-record and environment preflight functions.

The launch review records exact arguments, environment overrides, guardrails,
temporary/evidence volume and a fresh proposed run path. The proposed run directory
is deliberately absent: the collector must create it exclusively on launch and
rejects existing evidence. Keep the packet and review unchanged; source work uses
separate checkouts. The smoke fixture logs/database stay private.

## Requirement audit

Evidence paths below are relative to this integration checkout. Historical
checks are retained where their source has not changed; later focused checks
cover the subsequent frontend and tooling changes. Zero-selected groups do not
establish any platform acceptance.

| Item | Outcome and authoritative evidence |
| --- | --- |
| PRE-01 | Incident explicitly records interrupted/acceptance not achieved, last sample, usable coverage, missing files and SQLite shutdown error: `docs/soak/2026-10-27-interrupted-cr0.md`. |
| PRE-02 | Private manifest has 1,567 files, zero original inventory read failures, two expected missing final files. Final recheck matches all hashes; the two protected log comparisons are recorded separately after approved read access. No raw logs/tokens published. |
| PRE-03 | `target/incident-2026-10-27-complete/recovery.json`: original DB/WAL/SHM copied first; copy integrity `ok`, zero foreign-key violations. Original never opened writable or reused. |
| PRE-04 | Storage record identifies generated caches, Docker images and logs; historical exhaustion cause remains unknown. After stopping Docker, free space increased slightly across a 62-minute quiet interval. Final reserve is 116.2 GiB. |
| PRE-05 | Root and four worktrees retained. Integration changes are local commits; `744b84d6` identifies the actual build. Root remains `43d054df`; separate Settings work is not included. Execution ledger is authoritative. |
| PRE-06 | Named Cargo cache is about 23.51 GiB, below the 35 GiB inspection ceiling. Four jobs/incremental off; no cleanup. Evidence-preserving exact-path cleanup procedure remains documented. |
| PRE-07 | Collector storage/log guardrails and failure cases passed: 50/20/10 GiB, 10-second disk cadence, 1 GiB redirected logs, capacity-read failure and growth projection. Volume/threshold evidence is recorded. |
| PRE-08 | Protected cleanup, independent final writes, bounded atomic replacement and identity checks pass injection/rehearsal cases. Actual final smoke records delivered Ctrl+C, confirmed exit 0, no errors, 1.2967526-second shutdown. |
| PRE-09 | Clean source/tree/toolchain and executable/UI/collector hashes recorded in the package. Served UI matched. Collector retains 21,604-second suppression, 30-minute warmup and a valid endpoint after a full measured 24 hours. Fake-clock endpoint cases pass; they are not real soak hours. |
| PRE-10 | Versioned analyzer, unknown completion, per-metric coverage, resets/gaps/phases, fixed reasons, hourly/disk history and honest throughput boundaries: 24 analyzer cases in the 37-case Python run. |
| PRE-11 | Admission/channel reason accounting implemented without changing attempts or scheduler decisions; workspace actor and focused pool tests pass. Fixed labels only. |
| PRE-12 | Eviction service and unaccepted retry diagnostics tested, including full/closed finder channels and rediscovery. Entry size remains 72 bytes; no retained per-entry growth on this target. |
| PRE-13 | Counter contracts distinguish stores/searchability/cohorts and unknown measurements. Isolated release checks: two passing cases; median added uncontended counter cost 4.23 ns/event. No crawler speedup claimed. |
| PRE-14 | Snapshot journal hold expiry/release, locked cleanup/retry, differing IDs/old schemas, rollback and durable obligations pass workspace tests, including `deletion_evidence_survives_expiry_release_restart_and_failed_snapshot`. |
| PRE-15 | Cache regressions cover oversized/spare-capacity entries, replacement, LRU, clear and retained bytes: `files::cache_regressions` in workspace log. |
| PRE-16 | Cold scan cancellation before/during enumeration, deadlines, uncached cancellation and normal search/facets pass workspace tests. |
| PRE-17 | Verified partial-upgrade naming/encoding/archive behavior and transaction rollback/retry pass store/import tests, including `verified_partial_upgrades_replace_names_encodings_and_archive`. |
| PRE-18 | **Explicit release deferral permitted by the plan:** updater engine is Unix-only; existing Ubuntu has no Cargo/rustc/Node in PATH or standard locations. Concurrent-save/restart acceptance remains unexecuted, never inferred from Windows test counts. |
| PRE-19 | Detail visibility/cache/session and deferred-response cases pass the Windows frontend/browser gates; latest affected auth/result cases and deferred magnet tests retained. |
| PRE-20 | Setup/welcome/navigation/focus checks pass. Three held-font viewport cases preserve the CLS assertion; final boot/error run included. |
| PRE-21 | Discover All/typed query, precedence, invalid query, filter/pagination/sort/reset/history checks pass existing unit and browser gates. |
| PRE-22 | Responsive drawer/fullscreen/jump/mobile facts/landmarks/short lists pass applicable Windows checks. Pinned Linux pixels and actual screen-reader/native-zoom release work remain explicit. |
| PRE-23 | Documentation sync up to date; 1,156 lint checks, zero failures/warnings; all three Unraid templates valid. Final frontend-before-Rust package serves the embedded UI and exits gracefully. |
| PRE-24 | All 21 integrated collector rehearsals pass in `target/rehearsal-r17/checks.json`; standalone atomic/cleanup injections pass. All 37 Python cases pass, including nine new package safety cases. No disk-filling test. |
| PRE-25 | Final native isolated smoke passes: `target/launch-candidate-r21/smoke/smoke.json`. Fresh fixture, health, metrics, exact UI hash, no UDP, clean Ctrl+C; public DHT/scraping disabled. |
| PRE-26 | Source review recorded; exact committed package verified; production environment preflight passes after Docker graceful stop; HTTP 8484/UDP 6881 free; reserve passes; original evidence preserved; stop/recovery instructions rehearsed. Frozen launch arguments recorded. Launch itself is not performed. |

## Executed validation and limitations

- Rust workspace: 2,120 passing cases across 140 result groups, 109 nonempty;
  strict workspace Clippy and formatting pass. Relevant parser/property,
  migration, retention, cache, cancellation and import cases are in that run.
- Baseline frontend: earlier full suite 2,311 pass/one Windows symlink privilege
  skip; broad Windows browsers 503 pass/11 explicit skips. Subsequent changed
  paths have focused checks: 29 resolver tests, 185 auth/result tests, 128
  status/Alerts/script cases, and 28 final primitive cases.
- Final frontend check passes, with three existing Discover CSS warnings;
  E2E types pass. Final Windows browser follow-up: 67 pass, zero failures/skips
  or retries, including boot/error states, Alerts keyboard and light/dark axe.
- Final clean-source package build and every bundle budget pass. The first
  clean-source attempt failed the Alert budget by 14 bytes; its logs remain.
  The rejected channel-icon grouping also remains documented. Neither was
  represented as a pass or used to raise a budget.

Pinned Linux visuals, Unix updater execution and other cross-platform/scale/
security release gates remain outstanding. Their explicit deferral is part of
the approved Windows-baseline scope, not release acceptance. The separate
Settings product branch still has incomplete features and failed bundle gates;
it is excluded from this packet.

The forthcoming public run still needs at least 24 measured hours after
suppression/warmup, 99% core coverage, gap/ownership/progress/resource review,
confirmed graceful shutdown and isolated searchable/index consistency checks.
Those are soak acceptance tasks, not evidence produced by this preparation.
