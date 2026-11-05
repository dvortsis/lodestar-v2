# RP-4: SP-3 flooding — bounded per-publisher near-duplicate rate, judged in the writer
**Model:** Opus (filter engine core; bounded state on untrusted strings). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T10:55Z. Ships in v0.5.2.

## Why
SP-3 is the last unbuilt spam rule before SP-4 (RP-5) and the rule-pack gate (RP-G). RP-2 (#465) and RP-3 (#478) have merged; #479 (SRC-13b) has merged and frees `crates/pipeline/src/writer.rs`.

## Read first
- `docs/plan/2026-11-04-v06-rulepacks.md` — the RP-4 paragraph (line ~143) **is the spec**, and Q10 (line ~180) gives the numbers: flag at ≥ 20 near-duplicate names from one publisher within 1 h, demote at ≥ 100; at most 10,000 publishers tracked, oldest evicted; without a publisher, count per non-DHT source only (the DHT is not a publisher).
- `docs/decisions/2026-11-04-P-v06-rulepacks.md` (SP-3 judged in the writer only; inside the engine's precedence; `filter::Item` unchanged), `docs/architecture.md` §20.1, §20.6, §20.8.
- `docs/decisions/2026-11-05-RP-2.md`, `2026-11-05-RP-3.md`; `crates/filter/src/{engine.rs,load.rs,packs/**}`; `crates/pipeline/src/writer.rs` (note SRC-13b's `mark_adult_found` in `put_one`, and X-source-adult's always-adult mask — don't disturb either).

## Do (branch `slice/RP-4` from `origin/main`)
1. `crates/filter/src/flood.rs`: the bounded rate map (per publisher, else per non-DHT source), 1 h window, cap 10,000 with oldest evicted; name signature = normalized title tokens minus episode, quality and group tokens. No unbounded allocation from untrusted publisher strings (cap key length, hash or intern).
2. `crates/filter/src/packs/sp3.rs`: the SP-3 rule in its pack, with a recorded version direction per RP-0/RP-3 (a first version `widens`). Register it with a one-line `mod` in `packs/mod.rs` and the manifest.
3. A new engine entry point that the writer calls with the publisher or source and the signature. SP-3 is applied **inside the engine's precedence**: CSX-5 Override, exceptions, *Always allow*, pack and rule switches, overrides (RP-2), strongest action wins. Prepare's verdict leaves SP-3 out; the writer adds it. `filter::Item` is not changed.
4. Shadow and pending semantics as RP-0/RP-3 define for a new rule on upgraded vs fresh stores.

## Tests (gates from the plan)
- Property tests: the map never exceeds its cap under any input; no single name triggers it.
- A flood fixture; CSX-5 Override, an exception and *Always allow* each stop SP-3.
- SP-3's per-insert writer cost measured separately from the engine budget: ≤ 5% added to the writer's per-insert cost on the reference runner; architecture §3 "Coordinator ingest ≥ 1,000 new verified torrents/s" and "New torrent → searchable < 5 s" still hold. The 50 µs filter budget is not raised.
- No fixed sleeps (use an injected clock for the 1 h window).

## Own
`crates/filter/src/packs/sp3.rs`, `crates/filter/src/flood.rs`, `crates/filter/src/engine.rs`, `crates/filter/src/load.rs`, `crates/pipeline/src/writer.rs`, `crates/filter/tests/flood.rs`, the one-line `packs/mod.rs`/manifest registration, the api golden rows for the new rule, `docs/decisions/2026-11-05-RP-4.md`, and the `gates` entry RP-4 completes in `runner/slices.yaml` (if any).
**Overlaps:** #485 (RP-1b) and #486 (X-filter-perf-1105) touch `packs/mod.rs`/manifest; X-source-adult-2 touches `pipeline/src/lib.rs` and `services.rs`. Whichever merges later brings `main` in; regenerate goldens, never hand-merge. RP-5 follows you (it owns engine.rs, load.rs, writer.rs too).

## Done when
`cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; the filter budget test locally (CI's `filter budget` job decides). `/security-review` on the diff. PR against `main` titled "RP-4: SP-3 flooding: bounded per-publisher near-duplicate rate, judged in the writer", label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, head, and the writer-cost numbers, via the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
