# X-healthz-count-async

The runner's brief, 2026-11-28Z. The slice entry is in `runner/slices.yaml` (runner records r20).

## Why
- X-healthz-cold-start (#1159) moved the have-filter fill to a background thread.
- `count(*)` still runs in the pipeline's `prepare()`. At 10M stored torrents, a cold start falls from about 5.3 s to 0.9 s with it kept there, so the count is most of what's left before `/healthz` answers.

## What
1. Move the start-up torrent count to the fill thread.
2. Make `stats.stored_torrents` nullable while the fill runs. This is a contract change, so it covers:
   - the `docs/metrics.md` row;
   - the API DTO (`crates/api/src/dto/ops.rs`) and its example;
   - the regenerated `web/src/lib/api/openapi.json` and `schema.d.ts` (`npm run api:types`);
   - every reader of `Monitor::torrents()`;
   - the web readers of `stored_torrents`.
3. State the `lodestar_torrents` gauge's behaviour during the fill in `docs/metrics.md`.

## Owns
- As in `runner/slices.yaml`.
- **Refine at step 0:** name each web reader of `stored_torrents` and each reader of `Monitor::torrents()` you need to change. The runner adds them to owns before you edit them.

## Rules
- It's concurrency-sensitive: nothing reads the count as final before the fill ends.
- Test the null window, with a condition wait whose deadline is at least 10 s. No fixed sleeps.
- `cargo fmt --check`, clippy with `-D warnings`, and the touched crates' tests must pass.
- The bench's cold-start example measures before and after at the largest size the container can hold. Say which size.
- Add a decisions file `docs/decisions/2026-11-28-X-healthz-count-async.md` (§2.26 for any runner-level refinement).

## Step 0
Before writing code, send the runner:
- the readers you found;
- the null-window design;
- how the gauge behaves during the fill.

## Done
- A PR titled `X-healthz-count-async: <title>` from `slice/X-healthz-count-async`.
- Send `ready <full sha>` when CI is green.
