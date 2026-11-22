# X-leaktest-wg0-flake decisions

The runner's brief (2026-11-20): the cloud lane's VPN leak test failed once with `vpn_interface_held_down` on #950's gluetun re-test. gluetun kept bringing `wg0` back during phase B, so a probe never got a clean attempt. 0 packets leaked, and a re-run passed. It was a harness failure, not a leak. Calls not marked as the runner's are this PR's; the runner flags them to the owner (`docs/owner-decisions.md` §2.19). This supersedes the "at most three times (`HELD_TRIES`)" bullet of `2026-11-15-X-leak-test-capture-kill-2.md`; the rest of that file stands.

- **A phase B probe retries until the phase's own deadline, not three times** (runner). `HELD_TRIES` is gone. A discarded attempt runs again, with the interface taken down first, while the time budget below allows. **Why:** gluetun's restart loop can bring the interface back more than three times in a row while the phase still has minutes left. The deadline is phase-wide: a probe that uses it up leaves the rest skipped with 0 attempts, and the phase fails all the same.
- **The time budget** (runner, review of #988 at 195fa505). Retrying to the deadline makes phase B use all of it whenever wg0 flaps or won't go down, so the phase must end on time with a verdict, not run into the step's 900 s kill.
  - **A probe's first attempt always runs** (runner, delta review at 5435100d). The budget and the deadline bound the retries only, so a phase B with no flaps behaves as it did before this slice. **Why:** six probes of five container starts each, plus about 29 s of probe limits, can pass 180 s on a loaded VM. With every attempt gated, the last probe would fail `vpn_interface_held_down` on a VM whose wg0 stayed down.
  - **A retry starts only while the time left covers one:** at least `HELD_ATTEMPT_MIN` (30 s), or the slowest attempt seen so far in the phase, whichever is longer. Otherwise the probe is skipped and fails `vpn_interface_held_down` at once.
  - **A retry's interface checks wait no longer than the time left** before `held_until`, never under their 10 s floor. A first attempt's checks keep `IFACE_WAIT`, as before.
  - **No deadline set fails closed:** `held()` with an empty `held_until` sets `held_dead` instead of retrying without a limit.
  - **The numbers reject a leading zero** (`^(0|[1-9][0-9]{0,3})$`, and `IFACE_WAIT`'s likewise). `(( ))` reads `09` as octal and errors, which would have turned the budget off.
  - **`HELD_PHASE_WAIT` drops from 300 s to 180 s.** The step's sum, with every wait at its own limit and about 2 s per container start:

    | Part                                                                              | Limit             |
    | --------------------------------------------------------------------------------- | ----------------- |
    | **Before phase B: about 325 s**                                                   |                   |
    | gluetun's post-rules loop: 30 rounds of a 1 s sleep and three `docker` calls      | about 60 s        |
    | `wait_healthy`: 60 rounds of `docker inspect` and a 2 s sleep                     | about 120 s       |
    | The host and phase-A capture starts                                               | 20 s              |
    | The tunnel-up probes: `echo_ip` 10 s, TCP 5 s, DNS 5 s, ping about 6 s, IPv6 10 s | about 36 s        |
    | Phase A's blocked probes                                                          | about 29 s        |
    | Stopping and parsing the phase-A capture                                          | about 20 s        |
    | About 20 container starts                                                         | 40 s              |
    | **Phase B**                                                                       |                   |
    | With flaps: the deadline, plus one retry slower than any before it                | about 210 s       |
    | With no flaps: six first attempts                                                 | about 90 s        |
    | **After phase B: about 475 s**                                                    |                   |
    | Stopping and parsing the phase-B capture                                          | about 20 s        |
    | The recovery: post-rules loop 60 s, `wait_healthy` 120 s, `echo_ip` 15 s          | about 195 s       |
    | `docker stop`                                                                     | 10 s              |
    | The inbound capture start                                                         | 10 s              |
    | The stop window                                                                   | 75 s              |
    | `docker start`                                                                    | about 5 s         |
    | `wait_healthy` after the start                                                    | about 120 s       |
    | Stopping and parsing the host capture                                             | about 20 s        |
    | About 10 container starts                                                         | 20 s              |
    | **Total, every wait at its limit, with flaps**                                    | **about 1,010 s** |
    - **No setting fits that worst case inside 900 s.** It needs all three `wait_healthy` calls to run out, which already fails the run, and the step's 900 s kill fails closed.
    - **A healthy run fits with margin.** Each `wait_healthy` takes 10–30 s and the post-rules loop a few seconds, so a run takes about 450–550 s before and after phase B. With phase B's 210 s for flaps, that is at most about 760 s, inside 900.
    - **With 300 s, the healthy case would come to about 880 s,** too close to 900.
- **An interface that won't go down at all now uses the whole deadline** (from the self-review). Before, three failed `ip link set … down` attempts ended the phase; now the loop keeps trying, about three container starts per round, while the budget allows, then fails `vpn_interface_held_down` as before. That's slower to fail on a VM where the command is refused, but the result is the same, and telling "refused" from "gluetun brought it back between the two checks" isn't possible from inside the loop.
- **No pause between attempts.** Each attempt starts with an interface check, and taking the interface down is checked again before the probe runs. On the VM each check and probe is a container start of a second or more, so the loop is paced by the work itself. A wait added here would either be a fixed sleep, which the brief rules out, or a wait on a condition that the next attempt already checks. **Why not wait for gluetun to settle:** nothing gluetun exposes says its restart loop has finished, and taking the interface down at once is what phase B needs.
- **The verdict is unchanged** (the brief):
  - A probe that gets out fails at once, on any attempt, discarded ones included, and isn't run again.
  - The phase's capture and host counter run across every attempt and judge them all.
  - A probe with no clean attempt by the deadline is still `skip <probe>` and fails `vpn_interface_held_down`.
  - The ifindex check (a recreated interface discards the attempt) is as before.
- **The reason is printed as counts, just before `fail vpn_interface_held_down`:** `count held_attempts_<probe> <n>` for each skipped probe (`udp_ifdown` for the capture-only UDP probe), then `count held_iface_returns_ifdown <n>`, the number of times an interface check found the interface back up after reading it down. One return counts once, however many checks see it, and a first check that reads it up isn't counted, since it was never seen down. **Why several lines, not the brief's one:** the step output grammar (`remote.LINE`) holds one name and one number per line and drops anything else, so a single line couldn't carry the counts. They're counts only: no address, endpoint or interface detail. `docs/operations/cloud-fleet-runbook.md` §3.2 names them.
- **X-test-vm-runbook-step6, folded in:** `infra/dev-vm/linux-aws.md` step 6 now gives `leak-test.sh` its own work folder (`/opt/lodestar-leaktest`) with the exact commands. The test installs its work folder's `compose.yaml` into the stack, so using `/opt/lodestar-run` for both makes the copy fail at `compose_installed`. The fleet's step bootstrap already uses a fresh folder per run.
