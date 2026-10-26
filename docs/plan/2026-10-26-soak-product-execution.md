# Product work prepared during the CR-0 soak

Worktree: `target/worktrees/soak-product`, branch `work/soak-product`, based on support commit `80d7e198`. Scope and acceptance: [DHT implementation plan §13](dht-crawler-implementation-plan-2026-10-25.md#13-product-work-during-the-soak).

The active collector, executable, configuration, data, network settings and ports are unchanged. No dependency installation, application build, browser suite, benchmark or runtime fixture has been started. The original Docker crawler remains outside this work's scope and must stay stopped through the soak and its cooldown.

## PW-1: detail visibility — prepared, validation pending

- Changes: `web/src/lib/features/detail/{cache.ts,visibility.ts,model.svelte.ts,open.ts,magnet.ts,README.md}`, their regression tests, and `web/src/lib/features/auth/session.{svelte.ts,test.ts}`.
- Every open validates visibility before exposing cached identifying metadata. Content settings/resync invalidate caches and mounted models. Session changes cancel stale work; sign-out does not initiate a new request. All asynchronous cache producers check invalidation generations, including hash-based magnet actions.
- Prepared regressions: close/reopen, failed revalidation, content events from other tabs, resync, session sign-out, invalidation during detail/hit/prefetch requests, navigation to another hash and cancellation races.
- Executed: formatting and syntax checks of changed frontend files using the already installed local tools, plus `git diff --check`. These are lightweight source checks, not executed regression tests or semantic type checks.
- Deferred until actual collector completion: targeted unit/auth/browser regressions, frontend checks, full unit tests, production build, size budgets and applicable keyboard/reduced-motion/desktop/mobile browser coverage. No slice is accepted as validated yet.

## PW-2: setup completion — prepared, validation pending

- Changes: welcome route, `features/auth/WelcomePage.svelte` and its tests, English/Chinese auth messages, setup browser fixtures and welcome browser regressions.
- Reuses the auth frame and existing controls. A fresh `/auth` check gates success on direct visits and refresh; setup-required and signed-out visitors redirect, and an unreachable server offers Retry. Search and Settings are normal links, with no settings writes. The ready heading receives focus only while focus is unclaimed.
- Executed: changed-file formatting, TypeScript syntax checks, in-memory Svelte compilation and diff checks. Prepared unit and desktop/mobile browser checks cover authentication, actions, refresh and keyboard order. Test execution, visual review, updated Linux welcome snapshots and the full frontend gates remain deferred until the collector completes.

## PW-3a: detail history focus — prepared, validation pending

- Search retains the lazy detail host after its first open. The host observes shallow Back/Forward closure, restores a connected invoker or the query editor, and leaves focus chosen outside the drawer alone. Cleanup runs on route exit, including cancellation of queued focus restoration.
- Prepared browser checks cover Back/Forward, explicit close, removed invokers, focus already outside the drawer, full-page navigation and reload at desktop/mobile widths.
- Executed: formatting, TypeScript syntax checks, in-memory Svelte compilation and diff checks. Browser execution and the full frontend gates remain deferred.

## PW-3b: Discover All browsing — prepared, validation pending

- Changes span Discover links, URL search state, lazy Search loading, result/facet request identity and the Clear all action. `browse=1` opts an empty query into the existing `{op: 'all'}` AST. Explicit queries retain parsing precedence; parse failures do not become match-all. The flag never enters API request bodies.
- Browsing retains density, paging, sorting, filters and facet leases. Clear all/reset removes browsing and returns to the empty Search landing. Back/Forward reads the URL state.
- Prepared unit/browser regressions cover All and typed links, match-all paging/facets, sorting/filtering, invalid/direct queries, query precedence, reset and history. Corrected the history test's focus assertion to target the focusable row wrapper.
- Executed: changed-file formatting, TypeScript syntax checks, in-memory Svelte compilation and diff checks. Regression execution, frontend/type checks, budgets and browser/theme coverage remain deferred.

## PW-4: installation and shutdown docs — source checks passed

- Changes: operations and Linux build instructions now run `npm ci` then `npm run build` under Node.js 22+ before Rust, and explain the missing-UI stub. Standalone Docker uses `--stop-timeout=60`. Portable Windows guidance tells users to press Ctrl+C and wait for shutdown before closing the window.
- The Unraid guide/reference describe optional custom `LODESTAR_LOG` (default `info`) instead of a template field. Refreshed setup facts and regenerated the source-backed Advanced View illustration with only actual fields. Verified and corrected the discovery scanner's unresolved multiline Windows firewall attributes against WiX source.
- Executed: documentation-skill preflights; guide sync/check; **LINT: 1,156 checks, 0 failed, 0 warnings**; all three Unraid template validators; diff checks. Reviewed the rendered four-field SVG (one image, no clipping). BRAND: Lodestar, existing `brand/brand.json`.
- General `lint_material` reports 27 format failures and two warnings on operations/Linux/Docker documentation, exactly reproduced on the unchanged support-commit versions. These concern existing front matter, heading case, a placeholder and terminology, not the scoped corrections. This separate lint gate is not green; no broad document reformat is claimed.
- Deferred: packaged-UI build/smoke checks on fresh fixture data and ports after actual collector completion. No launcher or container was executed during this work.

## Source review and remaining gate

- Follow-up source review prevents pending detail module imports/prefetch from starting after Search unmounts, and cancels an existing prefetch on exit. Search now tracks which canonical query its AST answers so a changed URL cannot briefly dispatch a prior match-all AST before parsing. Added a regression for that transition and brought new auth waits to the required ten-second condition deadline.
- Lightweight ESLint passed on all 36 changed frontend TypeScript/Svelte files using the existing installed tools and repository rules, with project-service type analysis disabled for this source-only check. This does not replace `npm run check`. Changed-file formatting, syntax compilation and diff checks also passed.
- Local commits: `bff278fb` (PW-1), `d59fcd00` (PW-2), `5673a4e8` (PW-3a), `3d2e4f3c` (PW-3b), `7b69eab1` (PW-4). No push, merge or deployment.
- At the latest inspected baseline status (2026-10-26 14:34 MDT), HTTP health was 200 and metrics were present. Native crawler PID 99212 and collector PID 84132 were independently confirmed running. Collector completion, not its expected time, releases the deferred validation gate.
- Required next work after that external gate: targeted detail/cache/auth/search regressions, full frontend check/unit/build/size gates, browser suites including keyboard, reduced motion and desktop/mobile, Linux welcome snapshots/visual review, and packaged-UI smoke with fresh fixture data/ports. Keep the stopped original crawler's six-hour-plus-four-second cooldown after the actual native stop. Review completed baseline evidence before considering CR-1; later experiments remain gated.
