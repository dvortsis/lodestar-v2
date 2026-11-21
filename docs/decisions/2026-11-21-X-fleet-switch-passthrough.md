# X-fleet-switch-passthrough decisions

Runner brief X-fleet-switch-passthrough, 2026-11-21. The fleet VM stack (`infra/fleet/steps/vm/compose.yaml`) passed four crawler switches through to Lodestar by bare name: SAFE_RESTART, COORDINATED_ADMISSION, UTP and ROUTE_BACKOFF. The four r5 switches had no line (`docs/decisions/2026-11-20-runner-records-r5.md`), so a cloud screen or an A/B block couldn't turn them on: LOOKUP_END, PRESET_SLOTS, HINT_FIRST and SAMPLER_COOLDOWN_SPLIT. A repo change only: no deployed stack and no cloud call.

## What changed

- **Every crawler switch passes through, not only the brief's four.**
  - `compose.yaml` now names all eleven of the collector's `SWITCH_ENV` (`docs/soak/tools/linux/collect.py`), in the same bare-name style.
  - That adds the brief's four and three older switches that `crates/lodestar/src/config.rs` also reads with no compose line: RESPONDER_HINTS, STREAMING_PEERS and TARGET_PREFIXES.
  - A bare name passes the caller's value and leaves the variable unset when the caller has none, so every switch still starts at its shipped default.
  - PRESET_SLOTS (CR-1c, in progress, off by default) passes through like the others. Until Lodestar reads it, setting it has no effect.
- **One list in `block-lib.sh`.** `CRAWLER_SWITCHES` names the same eleven variables. `start_lodestar` unsets all of them before it sets the arm, so an arm never inherits a stray value from the caller's environment. Before, it unset three. `SAFE_RESTART` and the treatment's switch are then set as before.
- **The tests tie the lists to one source.** Compose's bare names and `CRAWLER_SWITCHES` must each equal `SWITCH_ENV`'s values, read from `collect.py` with `ast` (not imported). Every `LODESTAR_DHT_*` variable `config.rs` reads must be in `SWITCH_ENV` or in the collector's `DHT_SETTING_ENV` (the port, the enable flag, the witness folder). A switch added to Lodestar, the collector, compose or block-lib alone therefore fails a test.
- **The runbook** (`docs/operations/cloud-fleet-runbook.md` §6) says the restart drill, `docker compose restart lodestar`, needs `LODESTAR_IMAGE` and `DOCKER_GID` set (`compose.yaml` requires both), with the values start-block uses. X-test-vm found this.

## Treatment keys: not added in this slice

- **CR-COL-5 and CR-COL-6 define keys for the new switches:** `lc` (`lookup_end` and `preset_slots` together), `hint` and `split` (`TREATMENT_SWITCHES` in `collect.py`).
- **All three are stacked treatments.** From stage 2 the control arm has every earlier winner on (r5.1's A2, the analyzer's `--winners`), and a segment whose other switches aren't exactly the winners is invalid.
- **The block can't express that yet:**
  - `block.env` has no winners;
  - `start_lodestar` turns on only the treatment's switch, so the control is all defaults;
  - `TREATMENT_ENV` holds one variable, where `lc` needs two;
  - `start-block.sh`, which is outside this slice, accepts only `adm`, `utp` and `route`.
- **What adding the keys alone would do:** a block for them would be valid only while no stage had a winner, and the analyzer would void it after that.
- **So the block keeps `adm|utp|route`.** A follow-up slice can add the stacked treatments end to end:
  - a validated `WINNERS` line in `block.env`;
  - the winners set in both arms;
  - each treatment mapped to its list of variables;
  - `start-block.sh`'s check widened to match.

  This file records the gap; the runner decides on the follow-up.

## Tests

- `infra/fleet/tests/test_steps.py`:
  - `test_every_crawler_switch_passes_through_by_bare_name`;
  - `test_lodestar_in_gluetuns_namespace_unprivileged` now checks the fixed lines, then the full switch set.
- `infra/fleet/tests/test_block.py`:
  - `test_every_crawler_switch_is_unset_before_lodestar_starts`: the array equals `SWITCH_ENV`, and no other `unset LODESTAR_DHT_` list is left;
  - `test_starts_a_block` now sets every switch to `true` in the caller's environment, and Lodestar still starts with only `SAFE_RESTART=true`, the treatment's switch (`false` in the control) and the image.
- **Mutation checks:**
  - dropping `LODESTAR_DHT_LOOKUP_END` from `compose.yaml` fails both compose tests;
  - dropping `LODESTAR_DHT_HINT_FIRST` from `CRAWLER_SWITCHES` fails the array test and `test_starts_a_block`, because the stray `true` reached compose.
- `infra/fleet/run_tests.sh` passes.

## Owns

`infra/fleet/steps/vm/compose.yaml`, `infra/fleet/steps/vm/block-lib.sh`, `infra/fleet/tests/test_steps.py`, `infra/fleet/tests/test_block.py`, `docs/operations/cloud-fleet-runbook.md` and this file. The slice's `runner/slices.yaml` entry and placement came with #1012.
