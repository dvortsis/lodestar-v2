# X-fleet-stacked-arms decisions (#1049)

Runner brief, 2026-11-22Z. CR-AB-1's later stages each test one switch on top of the winners already kept (the owner asked for stacking on winners). Until now, a fleet A/B block's arms differed only in their treatment, and only `adm`, `utp` and `route` could be one.

## The treatment keys and their switches
- **Six keys:** `adm`, `utp`, `route`, `lc`, `hint` and `split`. These are the collector's `TREATMENT_SWITCHES` (`docs/soak/tools/linux/collect.py`) in `CRAWLER_SWITCHES`' variable names. `block-lib.sh`'s `switches_of` is the one table:

| Key | Variables |
|---|---|
| `adm` | `LODESTAR_DHT_COORDINATED_ADMISSION` |
| `utp` | `LODESTAR_DHT_UTP` |
| `route` | `LODESTAR_DHT_ROUTE_BACKOFF` |
| `lc` | `LODESTAR_DHT_LOOKUP_END`, `LODESTAR_DHT_PRESET_SLOTS` |
| `hint` | `LODESTAR_DHT_HINT_FIRST` |
| `split` | `LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT` |

- `lc` is the only key with two variables: lookup_end and CR-1c's preset_slots, which the collector already turns on together.
- **Every variable is a boolean** in `crates/lodestar/src/config.rs`, `PRESET_SLOTS` included, so each arm sets it `true` or `false`.
- `sample_backoff_escalate` has no key, so it stays off.
- **`lc`, `hint` and `split` become TREATMENT values too** (runner).
- **Tests check that the key lists agree:** the table matches the collector's `TREATMENT_SWITCHES`, and every variable is in `CRAWLER_SWITCHES`. The key list is the same in `block-lib.sh`, `rules.py` and `scrub.py`. These tests are parse-based, as in X-fleet-switch-passthrough.

## WINNERS
- **The vocabulary is the treatment keys** (runner). A rules file's `winners` lists earlier stages' winning keys. It's optional, and empty or absent means none, so stage 1's rules are unchanged.
- **The keys are joined by `-` in the step arguments and in `block.env`** (`WINNERS=adm-utp`). The arguments' shape (`remote.ARG_VALUE`, `[a-z0-9_-]`) has no comma, and keeping one spelling everywhere leaves nothing to translate. An empty `WINNERS=` line also means none.
- **Both arms get the winners' variables, set to `true`;** only the treatment's variables differ by arm (`arm_env`). A test renders both arms' environments through `arm-switch` and diffs them: the only difference is the treatment's variables, for `lc` on two winners, `hint` on `lc`, and `adm` and `split` with none.
- **What's refused, before anything starts:**
  - an unknown key;
  - a winner twice (`stacking_repeated_winner`);
  - a winner that is the treatment, or shares a variable with it (`stacking_winner_is_treatment`).
  - `rules.py` refuses the same in the rules file, and `start-block.sh` refuses them again, on its arguments, before the marker.
  - `load_block_env` re-checks them on every read and, as with the image pin, never takes `WINNERS` from the environment.
- **For the analysis,** the runbook (3.3.1) maps each key to `analyze_cr.py --winners` names (`lc` → `lookup_end`, `preset_slots`). A test keeps that table equal to `block-lib.sh`'s.

## Copy-off evidence (`scrub.py`)
- **It stays an allowlist** (runner):
  - `treatment` is exactly one of the six keys;
  - `winners` is a list of distinct keys, checked as a whole, so one bad entry drops the whole list;
  - anything else, free text included, is dropped.
- **A report's `identity.switches` now keeps stage 2's and 3's switches too** (`lookup_end`, `preset_slots`, `hint_first`, `sampler_cooldown_split`, `sample_backoff_escalate`), all as booleans. A stacked block's evidence then shows its winners on. Before, it dropped them.

## Scope (runner)
- `infra/fleet/steps/rules.py` and `infra/fleet/steps/vm/scrub.py` are in this slice. Without them, nothing could launch a stacked block or keep its evidence.
- Repo only: no cloud calls, no terraform and no deployed stack, and no new switch names.
