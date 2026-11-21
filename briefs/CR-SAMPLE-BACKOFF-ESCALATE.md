# CR-SAMPLE-BACKOFF-ESCALATE

Runner brief, 2026-11-21Z. Slice id `CR-SAMPLE-BACKOFF-ESCALATE`, branch `slice/CR-SAMPLE-BACKOFF-ESCALATE`, PR title `CR-SAMPLE-BACKOFF-ESCALATE: <title>`. The plan entry is in `runner/slices.yaml`. Opus builds it; this is the DHT crawler, so a fresh Opus review follows (no `/security-review` needed unless you touch untrusted-input parsing).

## Why
X-sampler-frontier-starve (#1022, read `docs/decisions/2026-11-21-X-sampler-frontier-starve.md` first, all of it) showed the BEP 51 frontier starving: one lost or failed sample cools a live node for 6 h (`SAMPLE_TIMEOUT_BACKOFF`, `UNSUPPORTED_BACKOFF` in `crates/dht/src/node/actor.rs`). At ordinary loss (about 4–5 % a sample), a live node meets its first failure within about 1.9 h at the earliest, then sits out 6 h, so the sampleable set shrinks steadily (1,544 → 860 samples a minute over 80 minutes in the sim).

## What
1. **Escalate per node, behind its own switch.**
   - **Switch (fixed name, runner):** config key `dht.sample_backoff_escalate`, environment variable `LODESTAR_DHT_SAMPLE_BACKOFF_ESCALATE`, default **off**. Wire it exactly as `dht.sampler_cooldown_split` is wired in `crates/lodestar/src/config.rs` (struct field, default, env parse, `dht_config()`), plus the sim's `Switches`.
   - **With it on:** a node's first failed sample in a row (timeout, error or unsupported reply) cools it for a short first backoff; a second failure in a row cools it for the existing 6 h. An answer resets the streak. With it off, behaviour is byte-for-byte today's: every existing sim test and golden unchanged.
   - **The short first backoff:** choose it from the sim, within these runner bounds: at least the node's own sample interval (300 s in the sim; never shorter than an answer's cooldown) and at most 1 h. Pick the value that best holds the frontier in the sim screen while keeping repeat samples to dead nodes small; put the screen numbers for each value you tried in the decisions file.
   - **Should a second failure escalate only on consecutive failures, or also count unsupported replies differently?** An `unsupported` reply (the node doesn't do BEP 51) may deserve the 6 h at once. Decide from the code and the sim, say why in the decisions file (a runner call under §2.26, name it).
2. **Bounded state.** The streak lives with the node's sampler entry, so it's bounded by the sampler's existing caps; no new unbounded map. State the memory per entry. The rate and outstanding caps, `NODES_PER_REPLY` and every other limit stay unchanged.
3. **Restart.** Re-check CR-0d's restart proof as #1022 lists:
   - the proof bounds restrictions from above (H = `MAX_INTERVAL` + `query_timeout`, S8 sealing at now + H); confirm a shorter first backoff stays inside it;
   - the sim's restart lower-bound model assumes send + 6 h for an unanswered sample (`crates/crawler-sim` README "restart scenario", `restart.rs`); with the switch on, update the model so the check stays a true lower bound, and keep the switch-off model unchanged;
   - the streak isn't in the snapshot, so a restart resets it: say what that costs (at worst one extra short backoff per node after a restart) and whether that's acceptable.
4. **Sim screen.** Extend `sampler_starve` (or add a sibling ignored measurement test following the same pattern) with an arm that has the switch on, at the same loss and error rates as arm A. Report samples a minute over time, totals, failed samples and repeat samples to nodes that never answer, beside arms A and C. The test stays `#[ignore = "measurement: …"]`.
5. **Tests.** Unit tests in `crates/dht` for: first failure → short backoff; second in a row → 6 h; answer resets; switch off → 6 h at once; unsupported per your decision. Mutation-check them. No fixed sleeps; use the existing simulated clock.
6. **Never shorter, except behind this switch.** X-sampler-cooldown-split's "Never shorter" rule stands for everything else. The decisions file says plainly that this switch is the one deliberate exception, and why.
7. **A decisions file** `docs/decisions/2026-11-21-CR-SAMPLE-BACKOFF-ESCALATE.md`, heading with `(#<this PR>)`: the chosen first backoff and the screen numbers, the unsupported call, the restart re-check, and the memory per entry.

## Owns
- `crates/dht/src/node/actor.rs`, `crates/dht/src/node/mod.rs`, `crates/dht/src/sampler.rs` (if the streak lives there; tell me if so)
- `crates/lodestar/src/config.rs`
- `crates/crawler-sim/**` (tests/sim and the restart model; tell me the exact files)
- `docs/decisions/*-CR-SAMPLE-BACKOFF-ESCALATE.md`
- this slice's `runner/slices.yaml` entry only

No other running slice owns these now. Send me the exact file list before you edit, including anything under `crates/dht/tests/**` or docs.

## Bars
`runner/standards.md`; `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; the sim goldens unchanged with the switch off. Never `tokio` test-util on the lodestar crate. No `unsafe`. Push once per round, and send "ready <pr> <sha>" when CI is green. Follow `.claude/skills/steward/SKILL.md`; the runner merges.
