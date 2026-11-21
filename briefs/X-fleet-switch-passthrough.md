# X-fleet-switch-passthrough

Runner brief, 2026-11-21Z. Slice id `X-fleet-switch-passthrough`, branch `slice/X-fleet-switch-passthrough`, PR title `X-fleet-switch-passthrough: <title>`. The plan entry is in `runner/slices.yaml` (#1012).

## Why
`infra/fleet/steps/vm/compose.yaml` passes four crawler switches through to Lodestar as bare names: SAFE_RESTART, COORDINATED_ADMISSION, UTP and ROUTE_BACKOFF. A bare name passes the caller's value and leaves the variable unset when the caller has none. The four switches added since have no line: `LODESTAR_DHT_LOOKUP_END`, `LODESTAR_DHT_PRESET_SLOTS`, `LODESTAR_DHT_HINT_FIRST` and `LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT` (names fixed in `docs/decisions/2026-11-20-runner-records-r5.md`). So a cloud screen or an A/B block can't turn them on. The X-test-vm sampler screen had to work around this.

## What
1. Add the four bare names to compose.yaml's `environment`, in the same style and with the same comment.
2. **`block-lib.sh`:** where the switches are unset (about line 370), unset the new ones too, so an arm never inherits a stray value. Where an arm or treatment maps to its variable (the `case` near line 495), add the new switches only if CR-COL-5 and CR-COL-6's switch sets name a treatment key for them. Read `docs/soak/tools/linux/` and the CR-SPEC decisions, and don't invent keys. If they don't, say so in the decisions file. CR-1c (`PRESET_SLOTS`) is in progress and off by default; pass it through like the others.
3. **Tests in `infra/fleet/tests/test_steps.py` and `test_block.py`:**
   - every switch the config knows is passed through;
   - the unset list matches the pass-through list.
   Ideally derive both lists from one place rather than repeating them, so a fifth switch can't be missed. Use the existing test style.
4. **`docs/operations/cloud-fleet-runbook.md`:** one line saying the restart drill (`docker compose restart lodestar`) needs `LODESTAR_IMAGE` and `DOCKER_GID` set for compose to accept the command. X-test-vm found this.
5. A decisions file `docs/decisions/2026-11-21-X-fleet-switch-passthrough.md`.

## Not in scope
Updating any deployed stack, and any cloud call. Both are the owner's, or the X-test-vm lane's within its runbook.

## Owns
- `infra/fleet/steps/vm/compose.yaml`
- `infra/fleet/steps/vm/block-lib.sh`
- `infra/fleet/tests/test_steps.py`
- `infra/fleet/tests/test_block.py`
- `docs/operations/cloud-fleet-runbook.md`
- the decisions file
- this slice's `slices.yaml` entry

Send me the list before you edit anything else.

## Bars
`runner/standards.md`. Run `python3 -m pytest infra/fleet/tests` (or the repo's own runner for those tests; see `.github/workflows/fleet.yml`) and shellcheck if CI runs it. Push once per round of fixes, and send "ready <pr> <sha>" when CI is green.
