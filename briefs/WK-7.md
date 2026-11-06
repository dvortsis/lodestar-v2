# WK-7: Store-free worker stages: fetch pool, prepare and funnel without a Store; claim batching; bounded outbound bytes
**Model:** Opus. The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-06T19:10Z. It launches into the slot ENR-9 freed (#542 merged at `f907139d`), within owner §2.42's four slots. It sits on the workers critical path: WK-9 needs WK-0, WK-1, WK-6 and WK-7.

## What to build
- **The plan:** `docs/plan/2026-11-06-v06-workers.md` §4, the WK-7 row: claim batching at ≤ 512 or 250 ms, and bounded outbound bytes. It's the spec; read the whole plan. The architecture sections it cites are §19.4 and §19.2, and `docs/owner-decisions.md` §1.6.5 (a worker's ≤ 256 MB budget).
- **Its `runner/slices.yaml` entry (`WK-7`), notes included:**
  - a shared edit to `Preparer::from_dict` in `crates/store/src/lib.rs`;
  - a single-machine parity bench, before and after.
- **What WK-0 left you** (merged #538; `docs/decisions/2026-11-06-WK-0.md`):
  - the `LeaseAuthority`;
  - `Fetched` keeps the lease until commit;
  - `WorkerSession::committed`;
  - the byte-capped queues;
  - the "For WK-9 and WK-10" notes.

  Respect them: nothing here commits remotely, and nothing dials or stores a peer hint unfiltered.
- **WK-6's codecs are close to merging** (#539, `crates/pipeline/src/wire/**`). Use them when they land. If you need them before then, ask the runner.
- **The single-machine path must stay byte-identical in behaviour.** The built-in worker keeps using the Store. Show the parity bench (mem-ingest) before and after, alternating runs.

## Own
- `crates/pipeline/src/worker.rs`, `crates/pipeline/src/stages.rs`, `crates/pipeline/src/lib.rs`, `crates/pipeline/tests/stages.rs`;
- the one shared edit named above;
- its tests;
- a decisions file, `docs/decisions/2026-11-06-WK-7.md`;
- its own `runner/slices.yaml` entry (`owns` only).

**Running beside it:**
- WK-1, which touches lodestar binary files and `crates/cluster/src/server.rs`;
- WK-6 (#539), which adds `pub mod wire;` to pipeline `lib.rs`; whichever PR merges second brings `main` in;
- CMP-0a (#543).

Message the runner before touching any file outside your `owns`.

## Done when
- These pass:
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test` for pipeline and its dependants (run them per crate if the disk is short, and say so);
  - `cd tools/status-connector && npm test`.
- Memory bounds are tested: outbound bytes are capped, and the worker-side pool and queues stay within the §1.6.5 budget.
- No fixed sleeps. Wait on a condition with a deadline of at least 10 s, and use paused tokio time for batching windows.

Branch `slice/WK-7`. Open the PR titled "WK-7: <a short imperative title, under 60 characters>". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
