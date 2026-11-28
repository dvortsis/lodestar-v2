# X-healthz-cold-start: a large store answers /healthz in seconds after a cold start

**From:** the runner, 2026-11-28T00:17Z. **Model:** Opus. **Branch:** `slice/X-healthz-cold-start`. **PR title:** `X-healthz-cold-start: A large store answers /healthz in seconds after a cold start`.

**Why (found by X-import-drain-memory, step 0, on the test VM):** at 10M stored torrents a cold start takes about 14 s before `/healthz` answers. Most of that is `prepare()` in `crates/pipeline/src/lib.rs` (around 1429–1545), which runs before the pipeline is up:
- `store.count()` sizes the have-filter;
- `store.for_each_swarm_hash(|h| have.insert(&h))` fills the filter from every stored hash.

Docker's health check, the Unraid template and the Windows service (`windows/service.rs:1490`) all wait on `/healthz`. So a big library looks broken, or trips a restart, for the first seconds after every start, and it gets worse as the library grows.

**The goal**
- `/healthz` (and the UI and search) answers within about 2 s of the server starting, at 10M, on the same class of machine.
- Crawling still never re-fetches metadata for a hash that's already stored, and never loses one.

**What the runner expects, so you can plan**
1. **Fill the have-filter on a background (blocking) thread.**
   - The filter's contract today: a "no" goes straight to the fetch pool, because a stored hash is never a "no". So while the fill runs, a "no" isn't trustworthy.
   - Choose and justify one approach. Either:
     - (a) the pipeline's intake waits for the fill before it admits anything (UI, search and /healthz are up meanwhile), or
     - (b) while filling, a "no" is checked against the store before the pool.
   - (a) is simpler and the runner's default. Take (b) only if you can show (a) delays crawling noticeably at 10M.
   - Either way, the filter must never be read as complete before the fill has finished. Prove it with a test that fails if the ordering is broken.
2. **Size the filter without a full count.**
   - Use the cheapest correct upper bound, for example `max(rowid)`, and say why it's an upper bound.
   - A filter that's too big is fine. One that's too small just rebuilds (have.rs says it doubles), but check that the rebuild path is safe while a background fill is still running.
3. **What /healthz reports.**
   - Look at how it's wired (crates/api and crates/lodestar/src/services.rs) and what it promises: `docs/user`, `docs/operations.md`, the Docker HEALTHCHECK and the Windows service.
   - If /healthz should still say "starting" while the fill runs, keep that distinction in its body or status, not in a long wait before the first answer. Don't change its documented contract without telling the runner first.
4. **Measure it.**
   - Add a bench or a test that measures time-to-first-/healthz on a synthetic store, at the largest size CI can afford, and report the numbers before and after.
   - If `bench/` already has a cold-start or store-size fixture, use it.
   - The 10M check on the test VM is the runner's job after merge, so tell it what to run.
5. **Write the decisions file** `docs/decisions/2026-11-28-X-healthz-cold-start.md`. Record the approach, the bound and any refinement.

**Owns** (send the runner a plan with your exact list before you change code):
- `crates/pipeline/src/lib.rs`: only `prepare()` and the have-filter wiring. X-import-hot-stamp's PR edits the hot-stamp section near `open_hot`; if it has merged, bring main in only if you conflict;
- `crates/pipeline/src/have.rs`;
- `crates/store/src/lib.rs`: only a new bound query, if you need one;
- tests in those crates;
- a bench entry;
- the decisions file.

Ask before touching `crates/api`, `crates/lodestar`, the Dockerfile or user docs. Leave `runner/` alone.

**Checks before `ready`:**
- cargo fmt --check;
- clippy --workspace --all-targets -D warnings;
- the tests of every crate you touch, plus the workspace tests CI runs;
- no fixed sleeps (condition waits with deadlines ≥ 10 s);
- never tokio test-util in the lodestar crate.

Follow `.claude/skills/steward/SKILL.md`. Send `ready <PR> <sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when CI is green.

**Size:** about 300–600 lines. A fresh Opus review follows; concurrency is the focus.
