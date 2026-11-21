# X-outputs-e2e-race (brief, runner 2026-11-21)

Fix one flaky e2e test: web/e2e/settings-outputs.routes.test.ts, "starts the bitmagnet export and
shows its progress" (failed once in CI on #1031, at the `outputs-bitmagnet-count` wait).

## Likely cause (check it)
- The test sets `exporting = true` before the screen's mount-time `/jobs` read
  (Bitmagnet.svelte onMount) has been answered.
- When that read is answered late, it already lists export job 5, so `seen = 5`. The press then
  polls for an id above 5 (`ourExport(jobs, after = 5, ...)`), never finds one, and ends `lost`.

## Do
1. Reproduce: make the mount read slow on purpose (e.g. delay the first listJobs answer in the
   mock past the click) and show the test fails the same way. Revert the delay afterwards.
2. Fix the test, not the screen: wait for the first `/jobs` answer (page.waitForResponse
   registered before `go`) before setting `exporting` and pressing Start. If you find that the screen itself
   misbehaves for a real user in the same race (a slow first read on a real server), stop and
   tell the runner before changing app code.
3. Show the fixed test passes with the delay in place.
4. Owns: web/e2e/settings-outputs.routes.test.ts, your decisions file, your own slices.yaml entry
   and a dashboard.yaml placement rule (next to X-snapshot-stale-outputs, ws-web). Run
   `cd tools/status-connector && npm test`.
5. No fixed sleeps; the delay in the reproduction is a mock-side promise held until a condition,
   or a route that fulfils after the click, never a timer in the test body.
