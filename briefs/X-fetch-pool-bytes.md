# X-fetch-pool-bytes: Cap the bytes the fetch pool holds in finished fetches
**Model:** Opus, with `/security-review`. This is concurrency-heavy, and the input that grows it is untrusted: remote peers choose info-dict sizes. A fresh Opus reviewer follows. Runner brief, 2026-11-06T21:50Z, launched into the slot CMP-0 freed (#548 merged), within owner §2.42's four slots.

## The finding (runner, 2026-11-06T20:45Z, from WK-7's review)
`crates/sources/src/pool.rs` runs up to `max_sockets` (400) fetches in a `JoinSet`. A finished fetch holds its info dict (up to `FetchOptions::max_metadata`, 16 MiB) while `on_result` awaits `self.out.send(out)`. When the consumer is slow, finished results pile up in the task set.
- **The worst case:** about 400 × 16 MiB ≈ 6.3 GiB, plus the 64-slot output channel (up to about 1 GiB) and the prepare batch.
- **It's pre-existing and also in v0.5.1.** It needs crafted large dicts announced on the DHT and hasn't been seen in soaks: real dicts are small, and soak r5 measured realistic memory.
- **The runner's call:** not a v0.5.1 blocker. It's fixed for v0.5.2. WK-10 (a worker's 256 MB budget, owner §1.6.5) depends on it.
- **Read:**
  - `docs/decisions/2026-11-06-WK-7.md` (the WK-7 estimate, the "not bounded by WK-7" item, and the oversized-dict note), and WK-7's `crates/pipeline/tests/stages.rs::the_worker_queues_fit_the_budget`. WK-7 is #549, merging soon; read it from `main` once it's there, or from `origin/pull/549/head`.
  - `docs/decisions/2026-11-06-runner-sources-split.md`.

## Do
1. **A byte budget in the pool.** A global cap on the dict bytes the pool holds across fetches in flight, finished fetches waiting to be sent, and items it has handed over but whose channel slot is still occupied, if you can count that cleanly.
   - When the budget is full, the pool **stops starting new fetches** (backpressure) instead of dropping finished ones. Keys keep their exactly-once accounting: every key is still either delivered or reported as a `Loss`.
   - A fetch whose dict alone exceeds the remaining budget still finishes. One item over the cap passes alone, as WK-7's `byte_queue` does. Bound that case and document it.
   - The default must fit single-machine installs comfortably, and the config must let a worker set something tighter (WK-10). Propose numbers in the decisions file. The runner decides performance numbers under §2.26.
2. **Review `max_metadata` (16 MiB)** against real dict sizes: BEP 9 practice, the size budgets, soak data if you can find any in docs. Recommend a value, and change it only if the evidence is clear. Record the reasoning either way.
3. **Tests (`crates/sources/tests/pool.rs`):**
   - **Stalled output:** a stalled consumer with many large fake dicts shows the held bytes stay at or under the cap, and fetching pauses.
   - **Recovery:** once the consumer moves, every key arrives exactly once.
   - **Oversized dict:** a dict over the remaining budget is delivered and not lost.
   - **Shutdown:** shutdown while the budget is full drains or reports every key.
   - **Timing:** use paused tokio time or condition waits with deadlines of at least 10 s, and **no fixed sleeps**.
4. **A metric** for the held bytes, if the pool exposes metrics today. If it doesn't, ask the runner first, because `docs/metrics.md` is shared.
5. **Decisions file** `docs/decisions/2026-11-06-X-fetch-pool-bytes.md`, with notes for WK-10.

## Own
- `crates/sources/src/pool.rs`
- `crates/sources/tests/pool.rs`
- the decisions file

**Ask the runner before touching anything else.** If the pool's config struct lives elsewhere, or `crates/lodestar` wires `PoolConfig`, name the file and the runner adds it.

**Running beside it:**
- ENR-9b (`crates/pipeline/src/unverified.rs`);
- SRC-15b (`web/`);
- WK-7 (#549, `crates/pipeline/src/{stages,worker}.rs`). It uses the pool's output. If you change `PoolConfig` or the output types, say so to the runner so WK-7 and WK-10 stay compatible.

## Done when
- **Checks pass:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test -p lodestar-sources`, plus `-p lodestar-pipeline` and `-p lodestar` if a public type changes;
  - `cd tools/status-connector && npm test`;
  - `/security-review`.
- **The PR:**
  - branch `slice/X-fetch-pool-bytes`;
  - PR titled "X-fetch-pool-bytes: <short imperative title>";
  - its slices.yaml entry arrives with runner PR #550. Don't add one yourself.
- **How you work:**
  - follow `.claude/skills/steward/SKILL.md`;
  - never merge;
  - push once per round;
  - message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA via the claude-code-remote `send_message` tool.
