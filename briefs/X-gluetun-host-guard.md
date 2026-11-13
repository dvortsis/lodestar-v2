# Brief: X-gluetun-host-guard (Opus, tester slot 16, cloud testing first §2.62)

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Branch:** `slice/X-gluetun-host-guard` from `origin/main`. **PR title:** `X-gluetun-host-guard: keep the host INPUT guard for the VPN endpoint while the test stack runs`.

## Why
The cloud tester's drill 3 (2026-11-13) stopped gluetun for 60 s and left Lodestar running.
- **What leaked:** the host sent 14 ICMP packets from its own public address to the VPN endpoint on eth0. These were most likely port-unreachable replies to the endpoint's leftover WireGuard UDP.
- **What didn't:** no in-tunnel content, and the bridge's non-endpoint egress was 0.
- **The verdict still fails:** under the plan's rule, this is traffic outside the tunnel.
- **Cause:** #800's host guard (`lodestar-leak-host`, an INPUT DROP for `-s <endpoint> -p udp --sport <port>`, `infra/fleet/steps/vm/leak-test.sh` ~lines 380–386) is installed only for the length of leak-test.sh, not while the stack runs.

## Scope
- **Owned paths:**
  - `infra/fleet/steps/vm/start-block.sh`
  - `infra/fleet/steps/vm/copy-off.sh`
  - `infra/fleet/steps/vm/block-lib.sh`
  - `infra/fleet/steps/vm/leak-test.sh`
  - `infra/fleet/steps/run.py`, only if its step file lists need a change
  - `infra/fleet/tests/test_steps.py`
  - `docs/decisions/2026-11-13-X-gluetun-host-guard.md`

  Ask the runner before touching any other file, including user docs or the companion templates.
- **The change:**
  1. start-block installs the same tagged host INPUT guard once the endpoint is known (shared with leak-test.sh through block-lib.sh, not copied), before gluetun starts, and keeps it for the whole block.
  2. copy-off removes it, along with any stuck copies (clear by tag).
  3. leak-test.sh keeps working with the guard already present: it installs no duplicates, and doesn't remove the block's own guard on exit.
  4. Re-running start-block is idempotent.
- **Validate the endpoint exactly as leak-test.sh does** (root-only `wg0.conf`, IPv4 and port checked) before it reaches iptables.
- **Decisions file:**
  - Say whether the same host ICMP behaviour can happen in a user's real gluetun setup.
  - Say what, if anything, the product should do: a docs note or the templates. Recommend; don't change product files. The runner places any follow-up.
  - Note the privacy impact honestly: the endpoint already knows the host's address, but the plan's rule counts any outside-tunnel packet.
- **Tests** in `infra/fleet/tests/test_steps.py`, in the existing style: the guard is installed by start-block, removed by copy-off, not duplicated by leak-test, and a bad endpoint is refused.
- `/security-review` on your diff: shell quoting, root-only reads, and no secrets or endpoint values in logs or evidence.
- **The re-test on a VM goes through the cloud tester (X-test-vm), not you.** Don't run cloud or AWS calls, terraform, or any fleet step yourself.
- Never write the endpoint's address, the VM's IP or firewall details into the repo or the PR.

## Checks before the push
- `python3 -m pytest infra/fleet/tests` (or the repo's way of running them)
- `shellcheck` on the changed scripts if it's available

## Done
Follow `.claude/skills/steward/SKILL.md`. Push once per round. When CI is green, message the runner "ready <PR> <full sha>". Never merge, never push to main, never force-push.
