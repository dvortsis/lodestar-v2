# CMP-10: Plugin dependencies on companions (the plugin → companion map, and a structured "Needs X" state in the plugin host and Sources API)

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`, because this gates which plugins start. **Branch:** `slice/CMP-10`, cut from `origin/main` (CMP-7 #606 is in, at f61ffa33). **PR title:** `CMP-10: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`:
  - the §3 table row;
  - §4.2, the shared-file rules: one-line edits to `crates/pipeline/src/lib.rs` and the Sources DTO's `needs` field, and `openapi.json`/`schema.d.ts` regenerated, never hand-merged;
  - §4.3's **CMP-10** bullet. It's the spec;
  - the §6 notes about CR-3.
- `docs/architecture.md` §18.1 and §18.10.
- `crates/companions/src/catalogue.rs` and `adapter.rs` (CMP-0): `CompanionId` and the catalogue entries.
- **The plugin host:**
  - `crates/sources/src/plugin.rs` and `pool.rs` (the `Manifest`, the plugin lifecycle);
  - where `crates/pipeline` starts plugins;
  - the Sources API DTO in `crates/api/src/dto/ops.rs` and the route in `crates/api/src/ops.rs`.
- `runner/standards.md` is the bar.

## Owns
- `crates/companions/src/requires.rs`
- `crates/pipeline/src/requires.rs`
- `crates/pipeline/tests/requires.rs`

**Shared edits allowed:**
- one `mod requires;` line (and its doc bullet) in each of `crates/companions/src/lib.rs` and `crates/pipeline/src/lib.rs`;
- `crates/pipeline/Cargo.toml`: the `lodestar-companions` dependency, plus `Cargo.lock`;
- the Sources DTO: one `needs` field and its type. Regenerate `web/src/lib/api/openapi.json` and `schema.d.ts` with the repo's tool, never by hand;
- the call site where the host decides whether to start a plugin: a minimal hook only;
- new events in the owning crate's `log_events.rs`, with their registrations (the `crates/lodestar` logging catalogue and golden, `event-names.ts`, the en/zh messages), as CMP-0a did. Events carry plugin and companion ids and codes only;
- one `docs/decisions/2026-11-08-CMP-10.md`.

Anything else goes to the runner first.

## The dependency direction (hard rule)
- `crates/pipeline` depends on `crates/companions`, never the reverse. CMP-4b relies on that.
- Check `cargo tree -p lodestar-companions -i lodestar-pipeline` (it must be empty) and `cargo tree -p lodestar-companions | grep -c lodestar-pipeline` (0).
- `crates/companions` already depends on `lodestar-sources-feeds`, which depends on `lodestar-sources`. So no cycle may come from `lodestar-sources` depending on companions either. If the map has to live where `sources` can see it, ask the runner.

## What to build
1. **The map (`companions/src/requires.rs`):** a closed, declared mapping from plugin ids to the companion they need.
   - The Prowlarr plugin needs Prowlarr.
   - The download-client source needs a download client (any of the catalogue's clients, so "one of" rather than "exactly").
   - Leave room for i2pd later, without an entry now.
   - It's a static, testable table keyed by closed ids. A plugin's manifest can't extend or override it at run time.
2. **State per plugin (`pipeline/src/requires.rs`):** `Ok`, `Needs(companion)` or `Unhealthy(companion)`, worked out from what is installed or detected and healthy. Take that as an injected trait or snapshot; don't reach into CMP-5's manager, which doesn't exist yet.
   - A plugin whose companion is missing **doesn't start** and reports `needs`. That's never an error loop, retry storm or log flood: log on transitions only.
   - When the companion arrives or becomes healthy, the plugin starts without a restart of Lodestar.
   - When the companion goes unhealthy, the plugin reports `unhealthy` and pauses, and other plugins are unaffected.
   - A plugin with no entry in the map is unaffected: it is always `Ok` as far as companions go.
3. **The Sources API:** each source row carries `needs` (absent, or `{companion, state}`) for CMP-9b's chip. Admin and non-admin visibility follow the existing Sources DTO rules.
4. **Fail closed:** unknown, unreadable or racing state never starts a gated plugin.

## Tests and gates
- **Unit tests for the map:** every mapped plugin id exists in the plugin registry, and every companion id exists in the built-in catalogue. A test fails if either drifts.
- **Integration (`pipeline/tests/requires.rs`):**
  - missing companion → no start, `needs`, one log line;
  - the companion arrives → it starts;
  - healthy → unhealthy → it pauses, and other plugins keep running;
  - an unmapped plugin is unaffected;
  - no busy loop: assert on the start attempts and the log count over a bounded condition window.
- **A property or table test** over the state transitions.
- **No fixed sleeps.** Waits are conditions with ≥ 10 s deadlines. Tests run on Windows CI.
- **Before pushing:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test -p lodestar-companions -p lodestar-pipeline`, plus `-p lodestar-api` and `-p lodestar` if you touch them;
  - in `web/`: `npm run check` and `npx vitest run` if `schema.d.ts` changes.
- **Size:** about 450 lines per the plan; stay under 1,500, or state why.

## Decisions you may make (§2.33; record each in the decisions file)
- the state type's shape;
- where the start hook sits;
- the DTO field's shape;
- how "one of the download clients" is expressed.

**These go to the runner:** anything that lets a plugin start while its companion is missing or unknown, any reverse dependency, or any change to the plugin host beyond the hook.

## Coordination
- CR-3 isn't running.
- CMP-4b (#608) and CMP-8b (open) edit `crates/companions/src/lib.rs` too, which can mean trivial mod-line conflicts. Bring `main` in only when needed.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and the full head SHA, using the Claude Code Remote MCP `send_message` tool.
