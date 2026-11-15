# X-leak-test-capture-kill (Opus, fleet test-VM tooling)

Runner, 2026-11-15. Three robustness fixes to the gluetun leak test, one slice. All are in the harness, none in Lodestar. The point is a leak test whose failures always mean a leak, never a busy host.

## 1. CI flake (priority: it reds unrelated PRs)
`infra/fleet/tests/test_steps.py::VMScripts::test_the_vpn_interface_comes_from_gluetuns_config` fails `fail stop_window_exercised` then `fail leak_test` under CPU load. The runner reproduced it on main 8b596aa9: 5/5 pass idle, 1/5 fail with 4 busy loops on 4 cores. It came in with #881 (X-gluetun-guard-stop). The judge is `judge_inbound_capture` in `infra/fleet/steps/vm/leak-test.sh` (~:395-404): either the inbound capture stub is already dead at `kill -0`, or its pcap counts 0 packets.
- Find whether the race is in the test stub (the fake capture exits or writes late) or in the script (it judges before the capture has flushed). Fix the real side. If the script can misjudge on a slow real host, fix the script.
- Prove it: the test passes 20/20 under the same load (4 busy loops per core count), and fails when the stop window really has no endpoint packets (the existing negative tests stay).
- No fixed sleeps. Waits are on conditions with deadlines of at least 10 s.

## 2. Capture kill (X-test-vm finding)
After TERM, the leak test waits for its capture with a deadline, then sends SIGKILL. A capture it can't kill is reported as its own failure line, not as a 900 s timeout. Cause: an AppArmor-confined shell (Ubuntu 24.04 via snap SSM, and snap terminals) can't signal tcpdump.

## 3. F4: `vpn_interface_stayed_down` on a loaded host (X-test-vm, 2 of 2 loaded runs)
gluetun's own VPN restart brought `wg0` back during phase B, before the probe ran. Phase B had 0 egress and 0 ICMP to the endpoint, so it wasn't a leak. Runner's call: phase B holds the endpoint block until the probe has run. The assertion stays strict; don't accept a late re-handshake instead.

## Owns
`infra/fleet/steps/vm/leak-test.sh`, `infra/fleet/tests/test_steps.py` (the leak-test tests and their stubs), the leak-test section of `docs/operations/` test-VM runbook if it describes these lines, and your own `runner/slices.yaml` entry and placement rule. Not `block-lib.sh`, `compose.yaml` or anything X-gluetun-restart-recover owns.

## Done
`./infra/fleet/run_tests.sh -v` green (deps: `pip install --no-deps --require-hashes -r infra/fleet/requirements-test.txt` in a venv), plus the 20× loaded run reported in the PR. PR titled `X-leak-test-capture-kill: Make the leak test fail only on a leak`; tell the runner "ready <sha>". Steward skill; never merge.
