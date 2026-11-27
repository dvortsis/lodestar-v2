# X-dht-switch-identity: a read-only gauge of the crawler's effective switch values

**From:** the runner, 2026-11-27T17:55Z. **Model:** Opus. **Branch:** `slice/X-dht-switch-identity`. **PR title:** `X-dht-switch-identity: Report the crawler switches' effective values as a gauge`.

**Why (owner, 2026-11-27):** the owner approved the revised path to 100 (O1–O4), `docs/plan/2026-11-27-revised-path-to-100.md` revision 6. It is on branch `claude/gifted-feynman-siwhjc` (commit 4ee28954e) and not yet on `main`; read it with `git fetch origin claude/gifted-feynman-siwhjc` and `git show FETCH_HEAD:docs/plan/2026-11-27-revised-path-to-100.md`. This slice is its §5.1, and it's on the critical path: round 1's collector (§5.2 item 1, slice X-cr-round1-harness, running in parallel) reads this gauge to record what each arm actually ran. Today the binary exposes no runtime value for the 12 switches, so an environment dump can't show an image's compiled defaults (plan §5 table). **Target: PR ready by 2026-11-28T12:00Z.**

## What to build
1. **A read-only gauge, set once at start,** of the effective value (0 or 1) of each of the 12 crawler switches plus `safe_restart`. "Effective" means after the compiled default, the config file and the environment are combined: the value the crawler actually uses. Find the 12 in `crates/lodestar/src/config.rs` (the `[dht]` switches: `coordinated_admission`, `responder_hints`, `streaming_peers`, `target_prefixes`, `utp`, `lookup_end`, `preset_slots`, `hint_first`, `sampler_cooldown_split`, `sample_backoff_escalate`, `soft_timeout`, `route_backoff`; check the names against the code and the plan §6.1 table, and say in the PR if they differ).
   - One metric family with a `switch` label is the expected shape (e.g. `lodestar_dht_switch{switch="utp"} 1`); follow `docs/metrics.md`'s naming and label rules and pick the name there. Label values are a closed set from code, never from input.
   - Read the value from the same resolved config the crawler is built from, so the gauge can't disagree with behaviour. Don't re-parse the environment separately.
   - Also report the effective intensity preset only if it's already a resolved value in the same place and cheap to add; otherwise leave it out (the collector reads the preset from the settings API).
2. **`docs/metrics.md`:** a catalogue entry for the new family (meaning, labels, when set, that it never changes while the process runs).
3. **Tests:** the gauge matches the effective config with a switch set (a) by environment, (b) by config file, (c) by compiled default, including one switch on by default and one off. Use the existing config and metrics test patterns; remember the lodestar crate is bin-only (`cargo test -p lodestar --bin lodestar …` for unit tests).
4. **A decisions file** `docs/decisions/2026-11-27-X-dht-switch-identity.md`: the metric's name and shape, and anything you decided.
5. **`runner/slices.yaml` and `runner/dashboard.yaml`:** add this slice's own entry and its placement rule only (CLAUDE.md "Keeping the dashboard current"), then run `cd tools/status-connector && npm test`.

## Limits
- **Owns:** `crates/lodestar/src/config.rs` (read access to the resolved values; no behaviour change), `crates/lodestar/src/metrics.rs`, the lodestar crate's metrics wiring where the gauge is registered, a new test file under `crates/lodestar/tests/` if needed, `docs/metrics.md`, `docs/decisions/*-X-dht-switch-identity.md`, and this slice's `runner/` entries. If you need another file, ask the runner first.
- **No behaviour change:** no switch default, preset or crawler behaviour changes. Values only.
- **No secrets or paths** in labels or values.
- Don't touch `docs/soak/tools/**`, `infra/**` or the plan files: X-cr-round1-harness owns those. Tell that slice's session your metric name as soon as you've chosen it, through the runner (message session_01EYhgh4JX3AmHzKXPjYCoUg); the runner passes it on.
- Follow `runner/standards.md` and `.claude/skills/steward/SKILL.md`. Run fmt, clippy (`-D warnings`) and the lodestar tests before pushing. Push once per round of fixes.
- When CI is green, message the runner `ready <PR> <full sha>`. The runner merges; you never do.
