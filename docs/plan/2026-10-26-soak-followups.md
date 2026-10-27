# Unblocked soak follow-ups and validation sequence

Worktree: `target/worktrees/soak-followups`; branch: `work/soak-followups`; base: `07421c3c` (`work/soak-product`). Owner authorized completing all unblocked follow-ups on October 26. The running CR-0 executable remains `43d054df`; none of these changes are deployed.

## Prepared work

| Slice | Change | Executed checks | Deferred acceptance |
|---|---|---|---|
| Retention | V12 deletion journal independent of temporary holds; retry/acknowledgment across snapshots | Rust formatting/syntax; SQLite migration, rollback and hold-backfill checks | Rust store/pipeline tests; migration/rollback and failure smoke on fixtures |
| Search | Byte-bounded file-ID cache; cancellation through cold lease/stem/facet compilation | Rust formatting/syntax | Search tests, memory/latency budgets and full Rust gates |
| UI | Drawer fit/fullscreen background, Insights offsets, mobile facts, headings/landmarks, short lists, explicit unavailable destinations | Changed-file formatting, TypeScript syntax, Svelte compilation, source ESLint | Type checks, unit/browser/axe/visual tests, build and size budgets |
| Validation | Gate, commands, fixtures and release dependencies below | Commands checked against package scripts and repository standards | Execution after actual collector completion |

The previous PW-1–PW-4 work is inherited. Its status remains [prepared with deferred validation](2026-10-26-soak-product-execution.md). This first follow-up addresses snapshot retention and file-query memory/cancellation. The owner subsequently authorized the partial-import name/archive and updater-persistence findings, plus documentation lint cleanup; see the [additional execution record](2026-10-26-soak-final-findings.md). CR-1 and later crawler experiments remain gated on baseline review.

Decision records: [retention](../decisions/2026-10-26-soak-retention.md), [search](../decisions/2026-10-26-soak-search.md), and [UI](../decisions/2026-10-26-soak-ui.md).

## Source verification record — October 26

- `cargo fmt --all --check` and `git diff --check` passed. Rust code has been parsed/formatted, not compiled or type-checked.
- An in-memory SQLite check executed V1–V12, checked deletion-trigger transaction rollback, committed deletion evidence surviving hold expiry, and V11 quarantine/tombstone backfill. This does not replace the Rust store tests or rollback smoke.
- Changed-file checks used the existing root frontend dependencies without installing anything: 26 files formatted/syntax-checked, no Svelte compiler warnings; source ESLint reported 0 errors and 0 warnings across 18 TypeScript/Svelte files. TypeScript transpilation and ESLint with project type services disabled are not semantic type checks.
- The required source security review found one test typing issue (`HTMLElement.inert` in Playwright callbacks), which was fixed. It found no concrete storage/filter security defect. A final source pass also corrected the LRU test budget so three entries exceed it. Those regression tests remain unexecuted.
- UI source review checked layout fit, essential narrow-row facts, accessible names, heading order, focus restoration, and existing design tokens. Browser rendering, keyboard behavior, contrast, zoom, visual snapshots and screen sign-off remain pending.

At the final read, the collector's latest sample was `2026-10-26T20:11:57-06:00`: baseline `running`, HTTP health 200, metrics valid. No `result.json` was present. Its recorded expected end remains October 27 at 14:19 MDT; use actual completion below, not that estimate. The live executable, collector, data and stopped Docker service were not changed.

## Release the waiting gate

Read `C:/src/Github/lodestar-v2/target/soak/cr0-2026-10-25-43d054df/status.json` and `result.json`. Confirm the recorded crawler and collector processes have actually ended, using their start times as well as IDs to avoid PID reuse. An estimated end time, a missing process alone or a failed collector is not a completed 24-hour baseline. Inspect stop reason and shutdown outcome; preserve any partial/failing evidence.

Run the inherited read-only analyzer:

```powershell
python docs/soak/tools/analyze_cr0.py C:/src/Github/lodestar-v2/target/soak/cr0-2026-10-25-43d054df
```

Collector completion releases host-intensive validation; soak acceptance separately requires reviewing health, coverage, counter resets, bounds, lookup progress, memory trend and metadata/searchable yield. Never clean the root repository's `target/`. Keep Docker stopped; any later restart of the older binary requires at least six hours plus four seconds after the actual native crawler stop and separate authorization.

## Validate in dependency order

Run from this worktree. Use its own target directory and synthetic fixture data. No live database, binary, collector or network configuration is used for tests.

```powershell
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) 'target/cargo'
cargo test --locked --offline -p lodestar-store -p lodestar-search -p lodestar-pipeline
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo test --workspace --locked --offline
```

Store regressions must cover ordinary quarantine expiry/manual release, a locked snapshot and retry after reopening, transactional rollback, old snapshot schemas and differing local IDs. Search regressions must cover oversized entries, vector spare capacity, replacement/LRU/clear, already-cancelled and expired cold requests, mid-enumeration cancellation and normal file-query results. The existing full store/search tests cover behavior beyond those focused cases. Separately execute Linux tests and migration/rollback smoke; Windows-only source inspection is not Linux validation.

Then, in `web/`:

```powershell
npm ci
npm run check
npx --no-install vitest run src/lib/features/detail src/lib/features/auth src/lib/features/search src/lib/features/compare src/lib/features/discover
npx --no-install vitest run
npm run build
npx --no-install size-limit
npm run e2e:typecheck
npx --no-install playwright test --project=core detail.routes.test.ts detail-history.routes.test.ts soak-followups.routes.test.ts insights.routes.test.ts compare.routes.test.ts discover.routes.test.ts shell.routes.test.ts
npx --no-install playwright test --project=keyboard-only
npx --no-install playwright test --project=reduced-motion
npx --no-install playwright test
```

Use the repository's preinstalled browser and [harness conventions](../../web/e2e/README.md); do not install a browser. The harness preview uses port 4173 and synthetic OpenAPI responses. On a developer OS without matching snapshots, `--ignore-snapshots` is diagnostic only: visual acceptance still requires the pinned Linux environment. Update affected welcome/layout snapshots there, including Riverstone light/dark, desktop/mobile, compact/comfortable and reduced-motion/keyboard checks. Do not replace baselines to conceal an unexplained difference.

For packaged-UI smoke, build the frontend before the Rust binary. Use a fresh directory under this worktree's `target/fixtures/`, an unused loopback HTTP port (for example 8485 after checking availability), and disable DHT/scraping with the documented environment variables. Do not point smoke tests at the soak data or start a second crawler. Verify the embedded UI, setup completion, Search/Settings links and Ctrl+C shutdown, then stop the fixture process. Restore task-specific environment overrides afterward.

Run the inherited PW-4 documentation sync/lint and template checks. The prior 27 material-format failures were resolved in the [additional authorized cleanup](2026-10-26-soak-final-findings.md); rerun the gates after any subsequent document changes. Search memory/latency benchmarks and release-scale gates remain required where applicable; fixtures do not prove ten-million-torrent budgets.

## Review and publication

Keep retention, search, UI and validation records in separate reviewable commits. They sit above the support and PW commits, so review/land those prerequisites first or use explicit stacked PR bases. Before any push, satisfy repository pre-push gates and resolve relevant findings. Fresh source review supplements, but does not replace, executed tests, green CI and the required final reviewer. This task does not push, merge, deploy or restart a crawler.
