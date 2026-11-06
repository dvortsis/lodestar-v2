# X-fetch-meta-reserve: Reserve a fetch's announced metadata size before downloading it
**Model:** Opus, with `/security-review`. This is concurrency-heavy code and a peer-wire path that handles untrusted input: the remote peer announces `metadata_size`. A fresh Opus reviewer follows. Runner brief, 2026-11-06T23:35Z. It launches into the slot X-fetch-pool-bytes freed (#554 merged), within owner §2.42's four slots. Its `runner/slices.yaml` entry arrives in the next runner PR; don't add one yourself.

## Background
- **What #554 left open.** X-fetch-pool-bytes caps the bytes the pool holds in *finished* fetches (`PoolConfig::max_held_bytes`, 64 MiB). Bytes inside a *running* fetch still belong to peer-wire, which grows its buffer as pieces arrive. The peak is therefore still `max_held_bytes + max_sockets × max_metadata`, about 6.3 GiB at today's defaults.
- **What to read:**
  - `docs/decisions/2026-11-06-X-fetch-pool-bytes.md`, including its "For X-fetch-meta-reserve" note;
  - `docs/decisions/2026-11-06-WK-7.md`.
- **Why it's required for workers.** A worker's 256 MB budget (owner §1.6.5), once WK-7's 227 MiB and the 4 MiB worker held budget are counted, leaves about 14 MB. Without reservation that's about 3 sockets, so WK-10 depends on this slice.
- **Where the hook goes.** `crates/peer-wire/src/fetch.rs` learns `metadata_size` from the extended handshake, at about line 417 in `fetch_on_stream`, before it requests any piece.

## Do
1. **Reserve before requesting pieces.**
   - Once a peer's extended handshake announces `metadata_size`, and after the existing range check against `max_metadata`, the fetch reserves that many bytes from a budget the pool shares with its held bytes. Only then does it request pieces.
   - If the budget is full, the fetch waits (backpressure) or gives up with a distinct reason. Choose one and justify it. It must never over-allocate.
   - The reservation is released exactly once on every path: success (handed over to the pool's held accounting with no double count), failure, timeout, cancellation or drop, and a peer that announces a size and then stalls.
   - A peer that announces a size larger than its actual data, or sends more than it announced, can't make the fetch exceed its reservation.
2. **Plumbing.**
   - Keep the `Fetcher` trait change minimal and give it a default, so test fetchers and WK-7 still compile.
   - `FetchedKey` and the output channel type stay the same, or you tell the runner why first.
   - The pool's `has_room` estimate can then use real reservations instead of the `flying × estimate` guess, if that's simpler and correct.
3. **The pool notes from #554's review, in `pool.rs`:**
   - Add a shutdown drain deadline. A finished `Ok` result can land in the outbox at shutdown while the consumer is stalled, and `out.send` then never returns. After the deadline, requeue the rest, as the `out_closed` path does. Test it on paused time.
   - `drop_source` on the outbox also removes the key from `recent_order`.
   - Fix the snapshot comment at about line 799, which overstates what restore filters.
   - Add a doc comment on `PoolIo::out`: the pool must be its only sender.
   - Add a test for the estimate or reservation gate, and one for the receiver closing while the pool is running.
4. **The `fetch_held_full_total` help text** in `crates/lodestar/src/metrics.rs` should match `docs/metrics.md`: stops caused by the estimate or reservation count, outbox backpressure doesn't. If you add a metric, for example reserved bytes, add its row to `docs/metrics.md`.
5. **The numbers.** Restate the peak with reservation at today's defaults, for single-machine and worker budgets, in a decisions file. The runner decides the defaults under §2.26.
6. **Tests.**
   - Fake peers in `crates/peer-wire/tests/fetch.rs`: many peers announcing large sizes against a small budget, so fetches wait and the reserved total stays at or below the budget.
   - A peer that announces a size and then stalls: its reservation is freed at the timeout.
   - A peer that sends more than it announced: refused.
   - A peer that announces a size and closes: freed.
   - Pool-level tests in `crates/sources/tests/pool.rs`.
   - Use paused time or condition waits with deadlines of at least 10 s. **No fixed sleeps.**
7. **The decisions file:** `docs/decisions/2026-11-06-X-fetch-meta-reserve.md`, with notes for WK-10.

## Own
- `crates/peer-wire/src/fetch.rs`
- `crates/peer-wire/tests/fetch.rs`
- `crates/sources/src/pool.rs`
- `crates/sources/tests/pool.rs`
- `crates/lodestar/src/metrics.rs` (help text, plus a new metric's line if you add one)
- `docs/metrics.md`
- the decisions file

Ask the runner before touching anything else, including `peer-wire/src/lib.rs` or `wire.rs`.

**Running alongside you:**
- WK-2: `crates/lodestar/src/{cli/worker.rs, worker/**}`, with one line each in `cli/mod.rs`, `main.rs` and `Cargo.toml`.
- X-win-log-held-open: `crates/lodestar/src/windows/logfile.rs`.
- X-consent-gate-polish (#553): pipeline `control.rs` and `lib.rs`, plus logging.

None of them overlaps your files.

## Done when
- These pass:
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test -p lodestar-peer-wire -p lodestar-sources -p lodestar-pipeline`, plus `-p lodestar` for the metrics doc tests (one crate at a time if the disk is short);
  - `cd tools/status-connector && npm test`;
  - `/security-review`.
- **The PR:**
  - branch `slice/X-fetch-meta-reserve`, titled "X-fetch-meta-reserve: <short imperative title>";
  - follow `.claude/skills/steward/SKILL.md`. Never merge, and push once per round;
  - message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA through the claude-code-remote `send_message` tool.
