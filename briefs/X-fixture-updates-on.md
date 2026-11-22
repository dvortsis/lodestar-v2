# X-fixture-updates-on (runner brief, 2026-11-22Z)

Kind: core, model opus. Dep X-ui-checks-2 is merged (#1043).

## What
X-ui-checks-2 checks the Updates and Backups screens' rebuild and paused states on mocks only. Add a `serve --fixture` variant with Updates and Backups switched on, so those states run against the real server.
- Possible shapes: a flag (`--fixture-variant updates-on`), or a second fixture manifest.
- **The seeded state is fixed:**
  - an update available from a fixed, in-fixture release record;
  - one finished backup and one scheduled;
  - optionally a paused state, if it can be reached without timing.

## Hard rules
- **Never any network.** The update checker and the backup target must not reach out. Seed their results, or gate their outbound step through `fixture::OFFLINE` / `unless_offline`, as the blocklist refresher does. Add a test that the variant makes no outbound request, for example that the checker's client is never built or is a refusing stub.
- **The existing DIR safety rules hold for the variant too:** reuse only this build's `fixture.json` (including its variant), refuse anything else with exit 3, and default to a temporary folder.
- **No change to the default fixture's contents.** The screens and e2e that use it today must stay byte-identical. Say how you checked.
- **Update signing and verification are never weakened.** If the screen needs a "verified" update, seed a record the real verifier accepts, from a fixture key that exists only in the fixture build (behind `feature = "fixture"`). Or show the state without a verified download. No bypass flag in release builds.
- **Backups write only inside the fixture's own folder.**

## Step 0 (send to the runner before building)
1. How Updates and Backups get their state today (which services, which settings), and how the variant seeds them without network.
2. The variant's interface (a flag or a manifest), and how the e2e suite will select it. Wiring the e2e specs is a follow-up unless it's tiny.
3. Files, an estimate, and whether anything in `lodestar_bench::fixture` changes. If it does, add it to owns at step 0.

## Owns
- `crates/lodestar/src/fixture.rs`
- `crates/lodestar/src/fixture/**`
- `crates/lodestar/src/cli/mod.rs`
- `docs/decisions/*-X-fixture-updates-on.md`
- this slice's own `slices.yaml` entry

Refine them at step 0.

## Tests and process
- **Tests:** unit and integration tests for the variant's state, the offline guarantee, and DIR reuse and refusal across variants. No fixed sleeps; any wait at least 10 s.
- **Review:** this touches the update path, so /security-review applies.
- **Branch and PR:** branch `slice/X-fixture-updates-on`, PR title "X-fixture-updates-on: <title>". Push once per round, then send "ready <pr> <sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
