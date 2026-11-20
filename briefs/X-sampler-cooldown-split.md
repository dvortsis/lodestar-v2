# X-sampler-cooldown-split brief (runner, 2026-11-20)

**Slice:** X-sampler-cooldown-split. Opus, core, crawler. Branch `slice/X-sampler-cooldown-split`, PR titled `X-sampler-cooldown-split: <title>`. Follow `.claude/skills/steward/SKILL.md`. Send the runner `ready <sha>` once CI is green.

**Spec:** build-plan §0.5 "Path to 100" item 6 and its test table; `docs/decisions/2026-11-20-path-to-100.md`; `docs/decisions/2026-11-20-path-to-100-cr-spec.md` (r5.1); the slices.yaml entry.

## The problem
The 2026-11-16 AWS crawl: the BEP 51 sampler's 200,000-entry table filled with 6 h timeout cooldowns (scaled further by the fleet politeness scale, WK-10e) about 35 minutes after suppression ended. Sampling then idled for about 5.5 h. Cooldown memory and admission capacity are the same table, so remembering who not to ask stops us asking anyone new.

## What to build
Behind the switch `dht.sampler_cooldown_split` (environment variable `LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT`), default **off**:
1. **Keep cooldown memory apart from admission capacity.** A full cooldown memory must never stop the sampler admitting and sampling nodes that aren't in cooldown.
2. **Never shorten a cooldown.** A node in cooldown is never sampled before its cooldown ends, including the politeness-scaled cooldown and the clamp WK-10e applies. A structure that can answer "maybe in cooldown" for a node that isn't is fine, because a false positive only skips a node. A false negative isn't allowed.
3. **Bounded memory, stated and measured.** One option is a compact, time-bucketed filter for timeout cooldowns. Whatever you choose, state its worst-case bytes in the decisions file and measure them in a test. The budget must sit inside the crawler's existing memory envelope (§1.6.3); cite it.
4. **Restart:** CR-0d's restart proof (`persist.rs`, `proof.rs`, `crates/dht/tests/restart*.rs`) must still hold with the switch on and off. Cooldowns survive a restart at least as well as they do today; never shorter.

## What must not change
- With the switch **off**, behaviour is identical to today. The existing dht and crawler-sim tests and goldens pass unchanged.
- The 200,000 admission capacity, the cooldown durations, the politeness scale and its clamp, the query rate and the outstanding cap stay as they are, with or without the switch.

## Untrusted input (`/security-review`)
- Sample replies and the node ids and addresses in them come from hostile nodes. Neither the new structure nor admission may grow without bound from them, and spoofed or duplicate nodes must not evict real cooldowns early.
- Add property tests: random admit, cooldown, expiry and restart sequences never sample a node in cooldown, and memory stays under the stated bound.

## Metrics
- The plan reports this switch on **sampler-full time** and its worst hours. If no metric exposes the time the sampler spends unable to admit, add one: a counter of seconds, with a closed label set if it has any. Add it to `docs/metrics.md` with a render test.
- Name the switch and any new metric in the decisions file.

## Screen (CR-SIM)
- #974 merged the calibrated simulator scenario. `crates/crawler-sim/tests/sim/**` is **CR-LOOKUP-END's** until its PR merges. Don't edit it before then; the runner tells you when.
- Then: run 8 seeds with the switch off against on, in a scenario long enough to fill the table, if the simulator can model that. Report W, q and y beside stored per minute, plus sampler-full time.
- Keep the change only if output per slot-second and per query don't fall, and sampler-full time falls. A screen never replaces a live run.
- If the simulator can't model a table-filling run within a reasonable test time, say so in the decisions file. Instead, give a deterministic dht-level test (paused clock) that shows the table filling and sampling idling with the switch off, and continuing with it on.

## Owns
- `crates/dht/src/node/sampler.rs`, `persist.rs`, `proof.rs`
- `crates/dht/tests/restart*.rs`, and new `crates/dht/tests/sampler_split*.rs`
- **Shared with CR-LOOKUP-END (#977), additive lines only:** the switch field in `crates/dht/src/node/mod.rs` `Config`, and its line in `crates/lodestar/src/config.rs`. Whichever PR lands second brings `main` in.
- `crates/crawler-sim/tests/sim/**`, after CR-LOOKUP-END merges and the runner says so
- `docs/decisions/2026-11-20-X-sampler-cooldown-split.md`
- `docs/metrics.md` (your rows only)

Ask the runner before touching anything else.

## Tests and rules
- Unit and property tests as above, plus an equivalence test with the switch off.
- No fixed sleeps. In dht tests, use tokio's paused clock as the existing tests do; anywhere else, condition waits with deadlines of at least 10 s.
- Never add tokio `test-util` or `testing` to the `lodestar` crate. No `unsafe`.
- Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the dht, lodestar and crawler-sim tests. CI runs the full workspace. A full `cargo test --workspace` can overflow a session's disk allowance; if it does, say so.
- **Size:** keep it under ~1,500 changed lines. Say why if it goes over.
- **Decisions file:** the structure and its bound, the never-shorten argument, the switch, the metric and the screen numbers. Name any refinement of the plan there (§2.19).

## Owns refined (runner, 2026-11-20T08:51Z)
- Added: `crates/dht/src/node/actor.rs`, two edits only: the `Sampler::new(cfg.sampler_cooldown_split)` construction and ~3 publish lines in `maintain()`. Shared with CR-LOOKUP-END; the second to land brings main in.
- Added: in `crates/dht/src/node/mod.rs`, one `Stats` field and its `StatsSnapshot` line, beside the `Config` field.
- Added: `crates/lodestar/src/metrics.rs`, to render `dht_sample_full_seconds_total` (a counter with no labels) and its render test, plus its `docs/metrics.md` row.
- Restart: on main there is no `persist.rs` and no `tests/restart*.rs`. The proof lives in `node/proof.rs` and `restart.rs` (with `restart/tests.rs`). This slice changes `proof.rs` only and leaves the on-disk format alone. No cooldown may come back shorter after a restart than it does today; the decisions file says how. A format change needs the runner first.

## Hold (runner, 2026-11-20T09:30Z)
This slice's `owns` overlap CR-LOOKUP-END's (`node/mod.rs`, `node/actor.rs`, `lodestar/src/config.rs`, `crates/dht/tests/**`). The owner's rule is that running slices never overlap, and the runner can't grant an exception. So the PR waits until #977 merges. Then bring `main` in (merge, no rebase), re-run the checks and open the PR. The one-line `sampler_cooldown_split: false` additions in `crates/dht/tests/{harvest,network,scrape}.rs` are approved for that point. The parallel question is owner-queue item 30.
