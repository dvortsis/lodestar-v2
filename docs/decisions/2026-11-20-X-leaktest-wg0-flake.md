# X-leaktest-wg0-flake decisions

The runner's brief (2026-11-20): the cloud lane's VPN leak test failed once with `vpn_interface_held_down` on #950's gluetun re-test. gluetun kept bringing `wg0` back during phase B, so a probe never got a clean attempt. 0 packets leaked, and a re-run passed. It was a harness failure, not a leak. Calls not marked as the runner's are this PR's; the runner flags them to the owner (`docs/owner-decisions.md` §2.19). This supersedes the "at most three times (`HELD_TRIES`)" bullet of `2026-11-15-X-leak-test-capture-kill-2.md`; the rest of that file stands.

- **A phase B probe retries until the phase's own deadline, not three times** (runner). `HELD_TRIES` is gone. A discarded attempt runs again, with the interface taken down first, while the time budget below allows. **Why:** gluetun's restart loop can bring the interface back more than three times in a row while the phase still has minutes left. The deadline is phase-wide: a probe that uses it up leaves the rest skipped with 0 attempts, and the phase fails all the same.
- **The time budget** (runner, review of #988 at 195fa505). Retrying to the deadline makes phase B use all of it whenever wg0 flaps or won't go down, so the phase must end on time with a verdict, not run into the step's 900 s kill.
  - **An attempt starts only while the time left covers one:** at least `HELD_ATTEMPT_MIN` (30 s), or the slowest attempt seen so far in the phase, whichever is longer. Otherwise the probe is skipped and fails `vpn_interface_held_down` at once. A first attempt that wouldn't fit never starts.
  - **Phase B's interface checks wait no longer than the time left** before `held_until`, never under their 10 s floor (`IFACE_WAIT` stays the limit outside the phase).
  - **No deadline set fails closed:** `held()` with an empty `held_until` sets `held_dead` instead of retrying without a limit.
  - **`HELD_PHASE_WAIT` drops from 300 s to 180 s.** The step's worst-case sum, counting each wait at its own limit and about 2 s per container start:

    | Part | Limit |
    |---|---|
    | Before phase B: gluetun's post-rules loop (30 s), `wait_healthy` (120 s), two capture starts (20 s), about 20 container starts (40 s) | about 210 s |
    | Phase B: the deadline, plus one attempt that runs slower than any before it (about 30 s) | about 210 s |
    | After phase B: two capture stops (30 s), the recovery's post-rules loop and `wait_healthy` (150 s), `docker stop` (10 s), the stop window (75 s), `wait_healthy` after the start (120 s), the host capture's stop (15 s), about 10 container starts (20 s) | about 420 s |
    | **Total** | **about 840 s of 900** |

    With 300 s the same sum is about 960 s, over the limit. A healthy run, where each `wait_healthy` takes 10–30 s, needs about 550 s. A container that hangs to its own 40 s `timeout` every time can still exceed the step's limit, but then the probes were never measurable.
- **An interface that won't go down at all now uses the whole deadline** (from the self-review). Before, three failed `ip link set … down` attempts ended the phase; now the loop keeps trying, about three container starts per round, while the budget allows, then fails `vpn_interface_held_down` as before. That's slower to fail on a VM where the command is refused, but the result is the same, and telling "refused" from "gluetun brought it back between the two checks" isn't possible from inside the loop.
- **No pause between attempts.** Each attempt starts with an interface check, and taking the interface down is checked again before the probe runs. On the VM each check and probe is a container start of a second or more, so the loop is paced by the work itself. A wait added here would either be a fixed sleep, which the brief rules out, or a wait on a condition that the next attempt already checks. **Why not wait for gluetun to settle:** nothing gluetun exposes says its restart loop has finished, and taking the interface down at once is what phase B needs.
- **The verdict is unchanged** (the brief):
  - A probe that gets out fails at once, on any attempt, discarded ones included, and isn't run again.
  - The phase's capture and host counter run across every attempt and judge them all.
  - A probe with no clean attempt by the deadline is still `skip <probe>` and fails `vpn_interface_held_down`.
  - The ifindex check (a recreated interface discards the attempt) is as before.
- **The reason is printed as counts, just before `fail vpn_interface_held_down`:** `count held_attempts_<probe> <n>` for each skipped probe (`udp_ifdown` for the capture-only UDP probe), then `count held_iface_returns_ifdown <n>`, the number of times an interface check found the interface back up after reading it down. One return counts once, however many checks see it, and a first check that reads it up isn't counted, since it was never seen down. **Why several lines, not the brief's one:** the step output grammar (`remote.LINE`) holds one name and one number per line and drops anything else, so a single line couldn't carry the counts. They're counts only: no address, endpoint or interface detail. `docs/operations/cloud-fleet-runbook.md` §3.2 names them.
- **X-test-vm-runbook-step6, folded in:** `infra/dev-vm/linux-aws.md` step 6 now gives `leak-test.sh` its own work folder (`/opt/lodestar-leaktest`) with the exact commands. The test installs its work folder's `compose.yaml` into the stack, so using `/opt/lodestar-run` for both makes the copy fail at `compose_installed`. The fleet's step bootstrap already uses a fresh folder per run.
