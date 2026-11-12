# X-restart-banner-polish decisions

Test-only follow-up to #718 (runner call: no change to non-test code).

- **The restart-start bump in `begin` matters, in one case.** While a restart lasts, `waiting` alone already makes a `/system` read drop its answer. But `begin(null)` clears `waiting` (a page that gave up, a test), and a read that was in flight from before the restart would then write its older state. The generation bump in `begin` is what drops it. A store test now sets that up (read in flight, `begin(w)`, `begin(null)`, the read lands) and fails with the bump removed.
- **The third store test was renamed** to what it checks ("reads pause while a restart lasts"); its old title claimed "newer than any read in flight" without setting that up.
- **The stale-read tests await the stale read's own promise** instead of yielding with `setTimeout(0)` or `Promise.resolve()`. The MutationObserver yield in `watch.test.ts` is unrelated (it waits for an observer's delivery) and stays.
- **Left out:** the banner learning of a save within 15 s (an Opus follow-up, per the runner).
