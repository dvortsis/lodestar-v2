# X-outputs-e2e-race decisions (#1032)

Slice session, 2026-11-21Z. It fixes one flaky e2e test, `web/e2e/settings-outputs.routes.test.ts` "starts the bitmagnet export and shows its progress", which failed once in CI on #1031: `getByTestId('outputs-bitmagnet-count')` was not found within 10 s.

## The race
- `Bitmagnet.svelte` reads `/jobs` once when it mounts, to learn the ids already taken (`seen`, and `data-ids-known` on the Start link once it has answered). A press then looks for an export job whose id is above `seen`; with no read answered yet (`seen` null) it falls back to the job's start time against the click.
- The test's mock lists export job 5 as soon as the test's `exporting` flag is set, and the mock reads the flag when it answers, not when it is asked. The test set the flag and then pressed Start without waiting for the mount read. If that read was answered between the two, it already listed job 5: `seen` became 5, the press waited for an id above 5, never found one, and ended `lost` (no count line).
- **Reproduced with a mock-side hold, no timer:** the mock's first `/jobs` answer waits for a promise the test releases (`holdFirstRead`). In the old order (`exporting = true`, then release, then press) the test fails with exactly CI's message (`outputs-bitmagnet-count` "element(s) not found", Expected "Done: 42 torrents in the file.").

## The fix is in the test, not the screen
- The test now waits for `data-ids-known="true"` on the Start link (the screen's own signal that the mount read has answered and `seen` is set) before it sets `exporting` and presses Start. The runner's brief suggested `page.waitForResponse`; the attribute is the stronger wait, since it holds only after the answer has been handled, not merely received.
- **The screen needs no change.** A real server creates an export job when the download request arrives, which is after the press, so a read can't list the new job before it; and a press before the mount read answers is handled by the clock fallback (`ourExport` with `after` null), which captures `seen` at the press, so a late read can't change an export already being watched. The test's mock listing the job early is the only thing that made `seen` equal the new job's id.
- **Three orders are covered now:** the test runs as before (`prompt`), with the first `/jobs` read held until the page has rendered and then released (`slow`), and with Start pressed while the read is still held and the read answered after it (`press-first`, which goes through the clock fallback and so tests the claim above rather than only arguing it). Each passes 5 of 5 repeats (`--repeat-each=5`).

## Slice records
`runner/slices.yaml` has this slice's entry and `runner/dashboard.yaml` its placement in `ws-web`, next to X-snapshot-stale-outputs (the runner asked for both). `cd tools/status-connector && npm test` passes.
