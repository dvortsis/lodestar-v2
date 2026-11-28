# X-leaktest-vpn-held-down (Opus; runner brief 2026-11-28)

**Branch:** `slice/X-leaktest-vpn-held-down`. **PR title:** `X-leaktest-vpn-held-down: <what it fixes>`.

## Why
Round 1 runs `infra/fleet/steps/vm/leak-test.sh` on every arm, before and after every day, through `round1-coordinator.py leak`. A failed leak test stops the arm, so a harness flake can cost a round-1 day (CR-SPEC-R1 §4, `docs/decisions/2026-11-27-CR-SPEC-round1.md`; the run sheet's §4).

X-leaktest-wg0-flake (#988, `docs/decisions/2026-11-20-X-leaktest-wg0-flake.md`) replaced phase B's fixed 3 tries with retries up to `HELD_PHASE_WAIT` (300 s). After that, the cloud lane saw `vpn_interface_held_down` once more (2026-11-22). gluetun restored the interface many times, so most phase-B probes were skipped. 0 packets leaked, and a second run passed. The fix only made the race rarer; it didn't remove its cause.

## Task
1. **Find the cause from the code,** not from a cloud run. Read how phase B takes the interface down and how it decides an attempt was clean. Read how gluetun (the pinned image the harness and round 1 use) reacts to its tunnel interface going down: its health check, its VPN restart loop and its re-creating the interface. Then name the mechanism that keeps phase B from ever getting a clean attempt in a 300 s window.
2. **Fix it in the harness,** so phase B gets a clean attempt whatever gluetun's restore loop does. Examples are holding gluetun's restore during the probe, or taking the path away in a way gluetun doesn't undo. Prefer the smallest change. State in the decisions file why it's sound.
3. **Never loosen the leak verdict.**
   - A probe that gets out still fails at once.
   - The capture and the counter still judge every attempt, discarded ones included.
   - A probe with no clean attempt by the deadline still fails `vpn_interface_held_down`.
   - The ifindex check stays.
   - If the fix changes what phase B proves, say so and ask the runner before you push.
4. **Tests** in `infra/fleet/tests/test_steps.py`, following the existing phase-B tests and fakes:
   - a fake gluetun that restores the interface the way you found, which fails today and passes after the fix;
   - every existing negative still failing.

   Run the whole suite. No fixed sleeps; condition waits have deadlines of at least 10 s.
5. **Decisions file** `docs/decisions/2026-11-28-X-leaktest-vpn-held-down.md`: the cause, the fix, why the verdict is unchanged, and the tests.

## Owns
- `infra/fleet/steps/vm/leak-test.sh`
- `infra/fleet/tests/test_steps.py`
- `docs/operations/cloud-fleet-runbook.md` §3.2, only if the failure's description changes
- `docs/decisions/2026-11-28-X-leaktest-vpn-held-down.md`

Ask the runner before touching anything else. In particular, `round1-coordinator.py`, `analyze_cr.py` and their tests belong to X-cr-round1-spec-fixes, which runs at the same time.

## Not in scope
- No cloud calls, no terraform, and never run the leak test against real infrastructure.
- Never touch the owner's soak host, Windows host or Unraid box.
- No IPs, endpoints, instance ids or host paths in the repo.
- No change to phase A or other steps.

## Deadline
Ready by 2026-11-28T14:00Z. The round-1 rehearsal starts after about 18:00Z and should run the fixed harness. If that's tight, tell the runner by 11:00Z.

## Done
- CI green (ci-ok, web-ok, e2e-ok).
- Self-review per `.claude/skills/steward/SKILL.md`.
- Send the runner `ready <full sha>` with the claude-code-remote send_message tool.
