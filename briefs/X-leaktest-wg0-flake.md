# X-leaktest-wg0-flake (Opus; runner brief 2026-11-20)

## Why
The cloud test lane's VPN leak test (`infra/fleet/steps/vm/leak-test.sh`) failed once with `vpn_interface_held_down` on the gluetun re-test of #950: gluetun kept bringing `wg0` back up during phase B, so a probe never got a clean "down before and down after" attempt. 0 packets leaked, and a re-run passed. It's a harness failure, not a leak. A harness that fails at random teaches people to ignore the leak test, so it's worth fixing.

## Scope
1. **Phase B retries.** Today `held()` gives each probe at most `HELD_TRIES=3` attempts. Let a probe keep retrying, with the interface taken down again first, until the phase's own deadline (`held_until`, `HELD_PHASE_WAIT`, default 300 s), instead of a fixed 3. A short pause between discarded attempts is fine as a condition wait (for example, wait until `iface_seen_up` reads down or the deadline passes), never a fixed sleep chosen to make the test pass. Pick the retry and pause policy yourself and record it in the decisions file.
2. **Never loosen the leak verdict.** A probe that gets out still fails at once, whatever the interface did. The capture and the counter still judge every attempt, discarded ones included. A probe that gets no clean attempt by the deadline still fails `vpn_interface_held_down`. The ifindex check, which spots gluetun recreating the interface, stays as it is.
3. **Say why it failed.** When `vpn_interface_held_down` fails, print one line with how many attempts each skipped probe had and how many times the interface came back up. The line holds counts only: no addresses, endpoints or interface details beyond the name.
4. **Fold in X-test-vm-runbook-step6.** In `infra/dev-vm/linux-aws.md` step 6, using `/opt/lodestar-run` as both `leak-test.sh`'s work folder and the stack fails `compose_installed`. Tell the reader to use a separate work copy, with the exact commands.
5. **Tests.** In `infra/fleet/tests/test_steps.py`, following the existing phase B tests (`test_phase_b_ends_at_its_own_deadline_as_a_failure`, `test_phase_b_fails_when_the_interface_wont_stay_down`):
   - an interface that comes back up 4 or more times, then stays down, now passes;
   - one that never stays down until the deadline still fails `vpn_interface_held_down`;
   - a probe that gets out during a discarded attempt still fails as a leak;
   - the new count line appears on failure.
   Run the whole test_steps.py suite.

## Owns
- `infra/fleet/steps/vm/leak-test.sh`
- `infra/fleet/tests/test_steps.py`
- `infra/dev-vm/linux-aws.md`
- `docs/operations/cloud-fleet-runbook.md`, only if §3.2's description of `vpn_interface_held_down` changes
- `docs/decisions/2026-11-20-X-leaktest-wg0-flake.md`
- this slice's own entries in `runner/slices.yaml` and `runner/dashboard.yaml`, per CLAUDE.md

## Not in scope
- No cloud calls. Never run the leak test against real infrastructure, and never run terraform. Tests use the existing fakes.
- No change to phase A, the post-rules checks, or any other step.

## Done
- CI green (ci-ok, web-ok, e2e-ok).
- Self-review per the steward skill.
- Send the runner "ready <pr> <full sha>".
