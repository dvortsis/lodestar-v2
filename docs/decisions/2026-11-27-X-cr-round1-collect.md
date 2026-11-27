# X-cr-round1-collect decisions

Round 1's collection side: the collector, per-arm stacks, the leak test, the host sampler and the coordinator (`docs/plan/2026-11-27-revised-path-to-100.md` revision 6, §5.2 items 1, 3, 4, 8 and 11, §6.3; owner O1–O4, 2026-11-27). The judging side is X-cr-round1-gate (#1147, merged). Old runs keep their recorded meaning: none of this applies without `--round` (collector) or `LODESTAR_ARM` (leak test), and `gate`, `effect` and the stage-1 run sheet are unchanged (§8 item 8).

## The interface: what the collect side writes and `gate-round` reads
This is the schema `docs/decisions/2026-11-27-X-cr-round1-gate.md` drafted, as merged. Nothing in it changed; the extra fields below are for people and for the runner's diagnosis, and gate-round ignores them.

| File (writer) | Field | Meaning |
|---|---|---|
| `run.json` (collector, `--round`) | `run_id` | `--run-id`; the coordinator passes `r<round>-<attempt>-d<day>-<arm>[-rep<k>]` |
| | `round1` | `{round, attempt, day, arm, repeat}` |
| | `host_identity` | HMAC-SHA256 of the machine ID under the round's salt |
| | `network_identity` | `{uplink_hash, provider, protocol}`: the uplink's public IPv4 read outside any tunnel, hashed as kind `ip`; `pia`, `wireguard` by default |
| | `image_digest` | as before (Docker's repo digest) |
| | `config.effective_switches` | the 13 values of `lodestar_dht_switch{switch}` as booleans, read at the first scrape that has all 13 |
| | `config.switches`, `config.switch_sources` | the gauge's values; source `env` where the environment set the switch, else `default` (compiled). Without the gauge, an unset switch is `null`, source `unset` |
| | `config.switch_env` *(extra)* | each switch's environment value: `true`, `false`, `unset` or `invalid` |
| | `settings.round1.bounds` *(extra)* | the bad bounds the arm was held to |
| `samples.jsonl` (collector) | `egress` | `{read: ok, hash}` or `{read: failed}`: at the first scrape, at every boundary, and on retries within 90 s of a failed boundary read |
| | `api.switches` | the gauge's values at each boundary scrape |
| `metrics/*.prom` (collector) | kept series | adds `dht_switch{switch}`, `dht_hints_captured_total`, `fetch_stream_batches_total`, `dht_target_prefix_queries_total{kind}`, `dht_sample_cooling_reason{reason}` |
| `<day>/archive.json` (coordinator) | `exits` | `{X\|Y\|Z: hash}`, written at bring-up |
| | `arms[arm].egress` | `[{utc, hash}]`: bring-up, every collector reading that succeeded, and one at shutdown |
| | `arms[arm].active_from_utc`, `active_through_utc` | the coordinator's 60 s activity scrapes (below) |
| | `arms[arm].leak_test` | `{before, after}`: `pass`, `fail` or `null` |
| | `events` | `[{class, arms, cause, diagnosis}]`; `arms: []` means every arm |
| | *(extra)* `arms[arm].exit`, `run_id`, `collector`, `leak_tests`, `activity_scrapes`; `shutdown_utc`, `notes`, `repeat` | |
| `<day>/host-samples.json` (`host_samples.py assemble`) | as the gate file says | `interval_seconds` 10, `vcpus`, `backing_device`, `rehearsal_rtt_median_ms`, `samples[]` {`utc`, `cpu`, `steal`, `disk_write_ms`, `disk_inflight`, `uplink_drops_total`, `tunnels{arm: {rtt_ms, lost}}`} |
| | *(extra)* `coverage_by_hour` | per UTC hour, the share of 360 slots each series covers (§5.2's 98 % rule is gate-round's) |
| `manifest.json` (coordinator) | as the gate file says | `declaration`, `comparator`, `days[]` {`attempt`, `day`, `repeat`, `reports`, `archive`, `contention`}, `attempt_ends`; paths relative to the round directory |

## Collector (`docs/soak/tools/linux/collect.py`)
- **`--round N --attempt ID --day D --arm X [--repeat K] [--run-id R] --identity-salt-file F`** makes a comb run a round-1 arm. It needs `--intensity` (the declared preset), refuses `--stop` (the coordinator owns shutdown, §5.2 item 11) and refuses a salt file another user can read or shorter than 16 bytes. It reads the uplink's address at launch and refuses to start without it.
- **Effective settings (§5.2 item 1):** the §5.1 gauge decides. `config_identity`'s "unset means false, source `default`" stays for every run without `--round`.
- **Egress (§5.2 item 4):** the IP echo is read inside the crawler's network namespace: a throwaway thread joins `/proc/<pid>/ns/net` with `setns` and opens its socket there, so the request leaves by the crawler's own route through its tunnel. The echo service's name is resolved by the host's resolver first and the TLS check still uses the name. Only `HMAC-SHA256(salt, "ip:" + address)` is written. Egress and the uplink are both kind `ip`, so a crawler whose traffic bypassed its tunnel shows the uplink's own hash, which the coordinator refuses at bring-up.
- **CR-0w first start:** with the gauge showing `safe_restart` off, a zero beside reason `off` is the steady state (no restart check runs, and since CR-0w sampling starts at once), so it counts as `T_open` after 60 s. With it on, `off` is still only the pre-check moment, and a first start opens on its `first_start` scrape within seconds. Without the gauge the old reading holds (6 h).
- **Bounds:** a round-1 arm's outstanding bound is its declared preset's cap (256, 1,024, 4,096). **Runner-approved addition (2026-11-27):** its peer-request bound (`dht_peer_active + dht_peer_queued`) is the preset's slots (32, 80, 256) when the gauge shows `preset_slots` on, and 32 only when it shows it off; with the gauge unread, the preset's. Today's fixed `> 32` would have stopped every B and A collector three scrapes after their slots filled (`docs/metrics.md`: running plus queued stays within `dht_lookup_slots_capacity`). Runs without `--round` keep 32 and 1,024.

## Per-arm stacks (`infra/fleet/steps/vm/compose.yaml`)
- **Each arm is its own Compose project** (`-p lodestar-<arm>`, so its own bridge) in its own stack directory (its own `secrets/wg0.conf`, `data`, `witness`). `LODESTAR_ARM_SUFFIX=-<arm>` names its containers (`gluetun-b`, `lodestar-b`, `lodestar-docker-proxy-b`) and `LODESTAR_UI_PORT` its loopback port. Unset, both default to today's names and 8090, so the cloud lane's commands need nothing new.

## Leak test (`infra/fleet/steps/vm/leak-test.sh`)
- **`LODESTAR_ARM=<a-z>` with `LODESTAR_UI_PORT`** tests that arm alone: its containers and project, probe containers `lodestar-leak-flow-<arm>` and `-drive-<arm>`, tags `lodestar-leak-guard-<arm>`, `lodestar-leak-egress-<arm>` and `lodestar-leak-host-<arm>` (block-lib's `HOST_TAG`, set after sourcing it, so `block-lib.sh` is unchanged), and its project's bridge for the IPv6 check. Every cleanup path finds rules by its own tag only (`clear_tagged` matches the whole tag), so another arm's are never removed.
- **The endpoint block is scoped to the arm's bridge** (`-i <bridge>`) in arm mode, so it can't cut another arm's tunnel. The recreate guard and host guard stay endpoint-scoped, as today: **the arms must use three different PIA servers**, which the run sheet checks before any leak test. Distinct exits already make that so in practice; it isn't left to chance.
- **An arm refuses to start** without its own port, or while its own Lodestar runs (leak tests run only while no window is open, §6.3).
- **Unset, every command is as before** (one `awk` reads the counter by `index` instead of a regex, with the same tag); the existing tests pass unchanged.

## Host sampler (`docs/soak/tools/linux/host_samples.py`)
- **One line per 10 s UTC slot,** stamped with the slot's start; a slot whose work starts more than 1 s late is skipped, never backfilled. A value it can't read is `null`. So a stall shows as missing, which gate-round counts over the threshold.
- **CPU** is user + nice + system + irq + softirq over all time in the aggregate `/proc/stat` line (guest is already in user); **steal** is its own share. **Disk** is the mean write time per completed write since the last slot on the declared device, and the writes in flight now.
- **Tunnel probe:** one ICMP echo to the arm's declared target from inside its gluetun namespace (`nsenter`, root). A reply is `{rtt_ms, lost: false}`, ping's "no reply" is `{lost: true}`, and anything else (no PID, ping's own error) leaves that arm's reading out of the slot, which gate-round counts as lost. **The target is declared per arm** in the coordinator file before the rehearsal (runner, 2026-11-27), and the rehearsal step checks it answers: PIA doesn't answer every ping (compose.yaml's own note), and a target that doesn't would leave contention `not_judged` on coverage.

## Coordinator (`infra/fleet/steps/vm/round1-coordinator.py`)
- **Start gates (`check-start`, also run by `up`):** no arm starts before the comparator's `timeline.window_end_utc` + 12 h; free space on the data volume covers 1.2 × (every arm's expected 24 h growth + the round directory's size), one check for the host; every arm has a passing leak test before the start; every arm's `data` and `witness` are empty (fresh, §6.3).
- **Exits (`bring-up`):** each arm's egress inside its gluetun namespace, hashed; two arms on one exit, or an arm whose egress is the uplink, is refused.
- **A failed leak test (`leak`)** stops that arm's Lodestar (every arm's with `--host-level`), records an `infrastructure` event, and blocks `check-start`, `up` and `collect` for that arm until a passing `--phase retest`.
- **Activity:** every 60 s, each arm's `/healthz` and `dht_queries_out_total`. An arm is active from the first scrape that is healthy with the counter above the one before, through the last of the unbroken run after it. A failed scrape, a counter that stops rising, or a gap over 180 s (rule 6.6) ends it, and it never restarts, so a peer that drops out mid-day is never "active through".
- **Shutdown (`watch`, §5.2 item 11):** once every collector has written `result.json`, it waits until the latest final boundary scrape of the day + 2 min, reads each arm's egress once more, stops every stack (`docker compose -p lodestar-<arm> stop`), stops the sampler and writes the archive. Collectors run without `--stop` and close at their own final boundary.
- **Early closes become events** so gate-round sees them: a collector reason that says the process failed is `product` with diagnosis `pending` (the runner diagnoses, §6.3); a storage guard is `infrastructure`; anything else is class `unclassified`, which gate-round reads as an unknown event, so the day fails closed until the runner classes it.
- **Manifest (`manifest`, `end-attempt`):** every input by SHA-256, relative to the round directory. It refuses a declaration or report on another image digest, a missing or repeated run ID (the comparator's included), and repeats that don't run 0..k. `end-attempt` writes `attempt_ends` with `days_run` from the days actually listed.
- **Not collected here:** the WireGuard handshake ages that §6.3's "a tunnel without a handshake for more than 180 s" names. The sampler's tunnel probe shows a dead tunnel as loss; a handshake series needs NET_ADMIN in each gluetun and is a follow-up if the runner wants it.

## Grants and scope (runner, 2026-11-27)
- **`infra/fleet/tests/test_steps.py`:** only its Compose ports test, which now reads the interpolated default (`127.0.0.1:8090:8090` with no arm set).
- **`infra/fleet/tests/test_round1_arms.py`** (new): the arm-isolation fixture, on test_steps' stubs.
- **CI note:** the coordinator's tests live in `docs/soak/tools/linux/` (the soak-tools job); a PR that changes only `round1-coordinator.py` doesn't trigger that job today (`.github/scripts/ci_changes.py`, the runner's file).

## Fixtures (§5.2's table, collect side) and where they are
- all-on by compiled default, recorded on; explicit all-off, recorded off; CR-0w first start; failed leak test stops crawling: `docs/soak/tools/linux/test_collect_round1.py`, `test_collect_round1_coordinator.py`.
- one arm's leak test and its failure cleanup leave the other arms intact: `infra/fleet/tests/test_round1_arms.py` (stubs; the real iptables and Docker run on the box's rehearsal).
- staggered starts and finishes, every peer active through the overhang (`pass`); a peer that exits during the overhang (`not_judged` for the later candidate): `test_collect_round1_e2e.py`, through `analyze_cr.py segment --declaration` and `gate-round` on `main`.
