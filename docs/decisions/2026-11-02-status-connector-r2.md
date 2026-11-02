# Dashboard plan file and repo rules (owner-requested, 2026-11-02)

Follows `2026-11-02-status-connector.md`. The owner asked for the dashboard to be as automatic as possible, and for repo rules that keep it current with every change. Nothing in the crawler, store, search or UI changes.

## Decisions
- **`runner/dashboard.yaml` holds everything GitHub can't show.** It replaces the hand-written copy of the plan that lived inside the connector (`src/meta.json` and the placement rules in `plan.ts`): phases, workstreams, placement rules, outside conditions, the owner queue, gates, roadmap steps, notes, the crawler rate and the soak timeline. The connector reads it from `main` on every refresh, so a plan change is a normal PR with no redeploy.
- **Progress still comes only from GitHub:** a branch `slice/<id>`, an open PR titled `<id>: …`, and its merge. The runner's `state.json` is a fallback.
- **Conditions that GitHub can answer answer themselves.** `auto: tag:v0.5.1` and `auto: branch:release/v0.5.1` are read from GitHub, so tagging the release releases the work that waited on it with no edit. Owner decisions stay manual (`met: true`), because they are recorded in prose.
- **Gates follow their slice** (`slice: G-m5` is passed once that slice is done) so a gate can't be marked passed while its slice is open.
- **Repo rules** (`CLAUDE.md` "Keeping the dashboard current", `runner/README.md`, the steward skill, `docs/handoff.md`): a PR that changes the plan's shape updates `runner/dashboard.yaml` in the same PR and runs `npm test` in `tools/status-connector`.
- **A check enforces it.** `test/consistency.mjs` fails when `dashboard.yaml` and `slices.yaml` disagree. CI runs it on any PR touching either file. A PR that changes `docs/build-plan.md` without `runner/dashboard.yaml` gets a warning, not a failure, because some edits are wording only. The dashboard also shows the same problems as a warning, so drift is visible even if a PR slips through.
- **Not made a required check.** Whether `Status connector / test` joins the required checks is the owner's call (`docs/owner-decisions.md`), like the Web and E2E checks.
- **Slices that appear without a placement rule** land in "Planned follow-ups" and are reported, instead of being hidden or failing the dashboard.

## Not decided here
- Whether `dashboard.yaml` should be generated from `docs/build-plan.md` (it is still written by hand).
- The dashboard page itself is a Claude artifact, not a repository file. Its tooltips, filters and layout live there.
