VERDICT: MERGE

# Review: PR #652 (OP-3a), delta b4cc32b1..bf10bff8 (round 3)

Reviewer: a fresh Opus reviewer, 2026-11-09Z. Local only: no Terraform, AWS, Linode, PIA, SSH or cloud call. The only network use was the Ubuntu mirror, to fetch iproute2 6.1 and check `ip -o link show up dev X` against real links.

## The round-3 asks
| Ask | Status | Evidence |
|---|---|---|
| BLOCKING-1: `resolves` counts dig's `;;` text | **Fixed** | `infra/fleet/steps/vm/leak-test.sh:74`. Inside bash double quotes `\.` stays `\.` and `\$` becomes `$`, so the inner `sh -c` gets `grep -Eq '^[0-9]{1,3}(\.[0-9]{1,3}){3}$'`, single-quoted. The stub (`infra/fleet/tests/test_steps.py:431-432`, `STUB_DIG` :446-453) now runs the script's own pipeline against a stub `dig` that prints the real `;;` lines on stdout and exits 9. There is a new case for `dns_up` failing when only `;;` comes back (:588). |
| LOW-1: phase B interface stays down | **Fixed** | `leak-test.sh:113`, `:180`. `iface_up` passes `"$1"` single-quoted through probe, then probe_in, then `sh -c '"$@"…'`, then `sh -c '…' iface "$iface"`, so `$1` is the interface name (already checked against the regex). It fails closed: a probe that didn't run returns NOT_RUN (222), which `refute` rejects (:40), and `finish` adds `probe_ran`. iproute2 6.1 checked: a down link prints nothing (rc 0, grep fails, so the check passes); a missing link writes an error to stderr with rc 1 (passes, "gone"); an up link prints a line (fails). Stub case :433-434; test "the interface comes back up" (:590). |
| NIT-1: the 255 comment | **Fixed** | `infra/fleet/steps/run.py:12-14` and the decisions file (:42) now give the per-VM lock as the reason. |
| NIT-2: "renewed" when Docker didn't answer | **Fixed** | `infra/pia-bridge/deliver.py:99`, `:152`, `:411`. A failed `docker ps` now exits 32 (`DOCKER_UNKNOWN`), not 30. |
| NIT-3: the marker's leading newline | **Fixed** | `leak-test.sh:65`. The stub demands exactly the new `sh -c` string (test_steps.py:415). The extra blank line in probe stdout is harmless: `echo_ip` strips `\r\n`, and every other probe's stdout is discarded. |

## Exit 32 is consumed correctly
The only consumer is `Delivery._to_target` (`deliver.py:386-413`) and `run` (:447):
- **Not retried:** only rc 255 loops (:392).
- **Not delivered:** 32 isn't in `DELIVERED` (:101).
- **Escalates:** the line ends "...; escalate; /run clean", which fails the `ok` check at :447, so the job returns 1 (test_deliver.py:500-503).
- **Waits:** `lost` is False, which is right, because the session completed.

No other file in `infra/` or `.github/` maps deliver's exit codes. `remote.BUSY_VM` and `run.py` belong to the separate `act` path.

## Mutations (each applied, the suite run, then reverted; `git status` clean afterwards)
- `grep -Eq '…'` back to `grep -q .`: **caught**, 4 failures (tight tunnel, `dns_up` case, phase-B-only firewall, interface-name test).
- The `refute vpn_interface_stayed_down` line removed: **caught**, 2 failures.
- `DOCKER_UNKNOWN` back to `MID_BLOCK` in the bootstrap: **caught**, 1 failure (`test_a_renewal_restarts_the_tunnel_only_outside_a_block`).

## Commands (venv with the fleet's test requirements)
- `./infra/fleet/run_tests.sh`: OK (192 fleet + 38 pia-bridge tests).
- `python infra/aws/fleet-control/build_templates.py --check`: OK. `python -m unittest discover -s infra/aws/fleet-control -p 'test_*.py'`: OK.
- `python3 .github/scripts/test_ci_changes.py`: OK.

## Residual (not blocking)
- `vpn_interface_stayed_down` is sampled once, after the last probe. If the interface goes up and then down again between probes, the check misses it. That needs two gluetun restarts in under a minute with the endpoint dropped, which is unlikely, and the capture still judges any egress. The decisions file already words it as "after the last probe". Optional: re-run `ip link set … down` before each phase-B probe.
- Confirm in the first cloud run that netshoot's Alpine `dig` and `ip` behave as BIND 9.18 and iproute2 6.1 did here.
