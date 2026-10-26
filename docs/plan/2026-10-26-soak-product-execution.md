# Product work prepared during the CR-0 soak

Worktree: `target/worktrees/soak-product`, branch `work/soak-product`, based on support commit `80d7e198`. Scope and acceptance: [DHT implementation plan §13](dht-crawler-implementation-plan-2026-10-25.md#13-product-work-during-the-soak).

The active collector, executable, configuration, data, network settings and ports are unchanged. No dependency installation, application build, browser suite, benchmark or runtime fixture has been started. The original Docker crawler remains outside this work's scope and must stay stopped through the soak and its cooldown.

## PW-1: detail visibility — prepared, validation pending

- Changes: `web/src/lib/features/detail/{cache.ts,visibility.ts,model.svelte.ts,open.ts,magnet.ts,README.md}`, their regression tests, and `web/src/lib/features/auth/session.{svelte.ts,test.ts}`.
- Every open validates visibility before exposing cached identifying metadata. Content settings/resync invalidate caches and mounted models. Session changes cancel stale work; sign-out does not initiate a new request. All asynchronous cache producers check invalidation generations, including hash-based magnet actions.
- Prepared regressions: close/reopen, failed revalidation, content events from other tabs, resync, session sign-out, invalidation during detail/hit/prefetch requests, navigation to another hash and cancellation races.
- Executed: formatting and syntax checks of changed frontend files using the already installed local tools, plus `git diff --check`. These are lightweight source checks, not executed regression tests or semantic type checks.
- Deferred until actual collector completion: targeted unit/auth/browser regressions, frontend checks, full unit tests, production build, size budgets and applicable keyboard/reduced-motion/desktop/mobile browser coverage. No slice is accepted as validated yet.
