# Dashboard plan file and repo rules (owner-requested, 2026-11-02)

Follows `2026-11-02-status-connector.md`. The owner asked for the dashboard to be as automatic as possible, and for repo rules that keep it current with every change. Nothing in the crawler, store, search or UI changes.

## Decisions
- **`runner/dashboard.yaml` holds everything GitHub can't show.** It replaces the hand-written copy of the plan that lived inside the connector (`src/meta.json` and the placement rules in `plan.ts`): phases, workstreams, placement rules, outside conditions, the owner queue, gates, roadmap steps, notes, the crawler rate and the soak timeline. The connector reads it from `main` on every refresh, so a plan change is a normal PR with no redeploy.
- **Progress still comes only from GitHub:** a branch `slice/<id>`, an open PR titled `<id>: …`, and its merge. The runner's `state.json` is a fallback.
- **Conditions that GitHub can answer answer themselves.** `auto: tag:v0.5.1` and `auto: branch:release/v0.5.1` are read from GitHub, so tagging the release releases the work that waited on it with no edit. Owner decisions stay manual (`met: true`), because they are recorded in prose.
- **Gates follow their slice** (`slice: G-m5` is passed once that slice is done) so a gate can't be marked passed while its slice is open.
- **Repo rules** (`CLAUDE.md` "Keeping the dashboard current", `runner/README.md`, the steward skill, `docs/handoff.md`), narrowed after the runner's review so the file doesn't become a conflict magnet: a slice PR edits only the `placement` rule for its own new slice or epic and the `gates` entry it completes; the runner alone edits `notes`, `owner_queue`, `conditions` and the rest. Both run `npm test` in `tools/status-connector`.
- **One source for whether a condition holds: `conditions.<key>.met` in `dashboard.yaml`.** The runner also writes "holds" in a comment on the `external:` entry in `slices.yaml`, and the two had already disagreed. Rather than add a structured field to the runner's file, `test/consistency.mjs` reads those comments and fails when one ends in `: holds` while `dashboard.yaml` has `met: false`. Setting `met: true` is the runner's one-line edit when it records the answer.
- **A check enforces it.** `test/consistency.mjs` fails when `dashboard.yaml` and `slices.yaml` disagree. CI runs it on any PR touching either file, and again on every push to `main` that does, so two PRs that are green alone but drift together go red on `main` at once. Deploying is a separate workflow that runs only when the connector changes, because the Worker reads the plan files live. `docs/build-plan.md` is not checked against `dashboard.yaml`, because nothing can compare prose with YAML: a PR that changes it without the YAML gets a warning, not a failure. The check fails on drift against `slices.yaml` only. The dashboard also shows the same problems as a warning, so drift is visible even if a PR slips through.
- **Not made a required check.** Whether `Status connector / test` joins the required checks is the owner's call (`docs/owner-decisions.md`), like the Web and E2E checks.
- **Slices that appear without a placement rule** land in "Planned follow-ups" and are reported, instead of being hidden or failing the dashboard.

## Also from the review
- A regex placement pattern that doesn't compile matches nothing and is reported, not thrown on every refresh. A pinned id or a described condition that `slices.yaml` doesn't have or use is reported too.
- If GitHub can't answer the tag and branch lookups, an `auto:` condition keeps its value from the file instead of flipping to unmet.

## Not decided here
- Whether `dashboard.yaml` should be generated from `docs/build-plan.md` (it is still written by hand).
- The dashboard page itself is a Claude artifact, not a repository file. Its tooltips, filters and layout live there.
