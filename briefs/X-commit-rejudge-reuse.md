# X-commit-rejudge-reuse: bring the commit's second judgment under 50 µs per torrent
**Model:** Opus. This is core content-safety code. Run `/security-review`. The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-06T04:45Z, the follow-up to RP-G (#526, merged at 636ac941).

## Finding
The owner's filter budget is 50 µs per torrent (architecture §20, `crates/filter/tests/props.rs` `budget`). RP-G measured two commit paths that judge a torrent twice, on the reference runner (AMD EPYC 7763, CI `filter budget` job 127699073103):
- **A flooded or cloned commit:** prepare's `Engine::judge`, then the writer's `Engine::judge_at_commit` with SP-3 or SP-4 matching (`crates/pipeline/src/writer.rs`, around lines 1879 and 2262). That's **81.3–82.0 µs** for both.
- **A find from an always-adult source at commit:** `judge_at_commit(.., adult: true)` builds a second `Doc` and judges the item again as Adult (`crates/filter/src/engine.rs`, around lines 309–345). That's **82.3 µs**.

The runner reported both as "reported, not held", and the owner was told this slice brings both under 50 µs.

## Do
1. **Remove the repeated work without changing a single verdict.** The design is yours. Candidates:
   - carry prepare's per-layer result (or its `Doc`) to commit, and re-run only the pack layer with SP-3 or SP-4 matching;
   - build the Adult reading's `Doc` once, from the first one;
   - skip the Adult re-reading when nothing in the Adult view can change the decision.

   Whatever you choose, these rules hold:
   - **The floor and the child-safety core are never skipped or reused across a change of rules.** If the engine's rules, packs, switches, overrides or versions changed between prepare and commit (any snapshot or generation change), commit judges in full, exactly as today.
   - **The precedence in `judge_at_commit`'s doc comment is unchanged:** floor, CSX-5 Override and *Always allow*, core hit, your rules, then the pack layer, where the strongest action wins. Pending versions stay in shadow.
   - **The result is never weaker** than today's `judge_at_commit`.
2. **A differential property test** proves the new path equals today's full re-judgment for every input:
   - flood levels, clone_of, `adult` true and false;
   - an Is adult classification correction;
   - items already Adult;
   - pending versions;
   - pack overrides and exceptions;
   - a rule change between prepare and commit, which must fall back to the full path.

   Keep the old path callable in tests as the oracle.
3. **Budget:**
   - In `props.rs` `budget`, move the two reported cases ("two judgments" and "always-adult at commit") to **held**, both under 50 µs.
   - Keep every held case under 50 µs.
   - Report the reference-runner figures from your PR's CI `filter budget` job, plus your container's scaled figures, kept apart as RP-G's decisions file does.
4. **RP-G's leftover:** add a "releases nothing" test against **real stored holds**, meaning torrents in the store with review rows, not rowless holds. Put it in `crates/filter/tests/packs_gate.rs` or the pipeline tests, whichever holds the store.
5. **Guardrails:** RP-G's `the_core_and_the_adult_floor_hold_under_every_pack_state` (`crates/filter/tests/packs_gate.rs`) must still pass unchanged. Run your own mutations against the new path, for example skipping the floor on reuse or reusing across a version change, and confirm the tests catch them.
6. **Decisions file:** `docs/decisions/2026-11-06-X-commit-rejudge-reuse.md`, with the design, the consistency argument and the numbers.

## Own
- `crates/filter/src/engine.rs` (the judgment path);
- `crates/pipeline/src/writer.rs` (the commit's judgment calls only);
- `crates/filter/tests/props.rs` (`budget` and new property tests);
- `crates/filter/tests/packs_gate.rs` (additions only);
- new tests under `crates/pipeline/tests/` if needed;
- the decisions file;
- your own `runner/slices.yaml` entry, one line in flow style with `deps: []`. Add a `runner/dashboard.yaml` placement only if no rule covers it.

## Done when
These pass:
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-filter` and `-p lodestar-pipeline`, plus the crates that depend on them (per crate if the disk is short; say so);
- the `budget` test run in release with `--ignored`;
- `cd tools/status-connector && npm test`;
- `/security-review`.

Branch `slice/X-commit-rejudge-reuse`. Open the PR titled "X-commit-rejudge-reuse: Judge a commit's second reading in under 50 µs". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. No fixed sleeps. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
