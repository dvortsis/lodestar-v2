# X-import-drain-memory: The server stays within its memory budget while it drains a large import

**Runner brief, 2026-11-27T17:25Z.** Core, Opus (memory and concurrency on the ingest and index path). Target < 600 changed lines. Step 0 is a measurement report: send it to the runner and wait for the reply before you change code.
- **Branch:** `slice/X-import-drain-memory` from fresh `origin/main`.
- **PR title:** `X-import-drain-memory: The server stays within its memory budget while it drains a large import`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- **Add this slice's own `runner/slices.yaml` entry** (kind core, model opus, deps [], your owns, notes "runner 2026-11-27T17:25Z, from the G-import-1M ladder on the test VM") and run `cd tools/status-connector && npm test`. Edit nothing else in that file or in `runner/dashboard.yaml`.

## Why
The Magnetico import ladder on the test VM (2026-11-27, provisional, main 68799e1b) measured the **server's** resident memory during the **drain**: from server start until `import status` reads `complete`, the phase in which the server indexes what `lodestar import` wrote.
- 1M torrents: drain peak 1.005 GB, then 610 MB after, and 679 MB during searches.
- 10M torrents: drain peak **4.45 GB**, then 727 MB after, and 920 MB during searches. `/healthz` took 10.6 s to answer after a start at 10M, against 0.56 s at 1M.

The import process itself stayed at 311–604 MB. The memory budget is under 1 GB of VmRSS (§3; the runner's #1107 decision keeps that gate on VmRSS, the owner's figure, with RssAnon reported as information). So the drain grows about 4× from 1M to 10M, and it should be bounded.

## Do
1. **Step 0: measure, don't change.** Reproduce the drain at 1M, and at 2–3M if your disk allows, with the smoke generator (`docs/operations/imports/pilots/smoke/gen_magnetico.py`; see `docs/decisions/2026-11-15-X-import-rate-1m.md` for the driver it used). Sample `/proc/<pid>/status` (VmRSS, RssAnon, RssFile, RssShmem) and `smaps_rollup` over the drain. Find what grows with the import's size:
   - the search index writer's heap and merge threads (tantivy);
   - SQLite's page cache or mmap;
   - the catch-up batch size, or a queue of pending documents;
   - the import job's bookkeeping;
   - the health or start-up path that made `/healthz` slow.
   Use stack samples (`gdb`; `perf` isn't available) or allocation counts if needed.
   - Report to the runner, with file:line, the composition at the peak and how each part scales from 1M to 2–3M, then a proposal: which bounds to add, their expected peak at 10M, and the cost to drain time.
   - Say how much of the peak is file-backed (RssFile, mmap of index or DB files). The gate stays on VmRSS; whether file-backed pages are the bulk matters for the fix.
   - Also say why `/healthz` is slow at 10M.
   - Name the owns you need. Wait for the runner's reply.
2. **After the runner's go:** build the bounds. Don't lower any import-rate or search budget to make memory fit; the import pass line of 2,000/s and the search budgets are owner numbers. If a bound costs drain time, report the measured cost.
3. **Tests:** a bounded-memory test or bench row that fails if the peak grows with the import's size, for example the peak at N and at 2N within a fixed margin. Use the bench's existing RSS sampling (`bench/`, from #1107) if it fits. No fixed sleeps; condition waits with deadlines of at least 10 s.

## Owns (step 0)
Read-only, plus scratch tooling outside the repo. Owns for the fix come with the runner's go.

**Not yours, whatever the fix needs:**
- `crates/lodestar/src/config.rs`, `metrics.rs`, `docs/metrics.md` and `crates/dht/**` (CR-0w);
- `crates/store/src/saved.rs` and `crates/lodestar/src/services.rs` (X-saved-verified-rewrite, until it merges);
- `crates/search/src/doc.rs` and `population.rs` (X-remote-verified, until it merges).

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the tests for the crates you change. Say which you ran. Put the before and after memory tables in the PR description. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no test-VM or Unraid use (a re-run at 10M is the runner's to book), no new accounts, no spending, no force-push. Held security detail stays off every branch.
