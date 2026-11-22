# X-leak-test-capture-kill decisions, part 2

Parts 2 and 3 of the runner's X-leak-test-capture-kill slice: the gluetun leak test's failures should always mean a leak, never a busy or confined host. Part 1, the CI flake in the capture stub, is #946 and its own decisions file. Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## 2. A capture that won't stop fails its own line

- **The problem (from the test VM):** the leak test stopped each capture with TERM, then waited without a deadline. A shell that may not signal `tcpdump`, such as an AppArmor-confined one (Ubuntu 24.04 through a snap-installed SSM agent, or a snap terminal), left the wait hanging until the run's 900 s timeout.
- **Now `stop_capture` stops every capture:**
  1. It sends TERM, then waits up to `CAPTURE_STOP_WAIT` seconds (default 15, at least 10) for the capture to exit.
  2. If it's still running, it sends KILL and waits as long again.
  3. If it's still running after that, the run fails `capture_stopped_<name>` (`host`, `inbound`, or the phase) and carries on without waiting.
- **Alive means "still this script's own" (`ours`), from the review:** `/proc` has the PID, and `/proc/<pid>/stat` names this shell as its parent.
  - Not `kill -0`: that's a signal too, and the same confinement can refuse it, which would make a running capture look gone and hang the `wait` after it.
  - A capture that exited early has been reaped, and its PID may belong to another process by now. Checking the parent means that one is judged dead and never signalled, by TERM or by the KILL.
- **A capture that won't stop is remembered (from the review).** Its PID is kept, including when the next capture reuses the variable, and the exit's cleanup makes one more KILL attempt at each, again only if it's still `ours`.
- **Signals go through the `kill` program on `PATH`,** as the test's other tools do (`docker`, `tcpdump`, `iptables`), so the tests' stub can refuse them. The script checks the program is there (`tool_missing_kill`).
- **Behaviour on a host where signals work is unchanged:** TERM stops `tcpdump` at once, and it flushes before it exits.

## 3. Phase B probes run only with the interface down (runner's call)

- **The problem (from the test VM, 2 of 2 loaded runs):** gluetun's own VPN restart brought `wg0` back during phase B before the probes ran, and `vpn_interface_stayed_down` failed. Phase B saw no egress and no ICMP to the endpoint, so it wasn't a leak.
- **The brief's wording is replaced (runner's call).** The brief said "phase B holds the endpoint block until the probe has run", but the endpoint block already holds through all of phase B. What phase B has to prove is that every leak probe ran while the VPN interface was down. A restart after the last probe is no leak evidence either way.
- **Each phase B probe is bracketed:**
  1. Just before it, the interface must be down. It's taken down again if gluetun brought it back.
  2. The probe runs.
  3. Just after it, the interface must still be down.
- **A probe counts only with a clean bracket.** Otherwise that attempt is discarded and the probe runs again, at most three times (`HELD_TRIES`).
- **What phase B reports:**
  - **Every probe got a clean attempt:** `ok vpn_interface_down_during_probes`.
  - **A probe never got one:** its own line reads `skip`, and the run fails `vpn_interface_held_down`. That's a harness failure, distinct from any leak line.
  - **The end-of-phase `vpn_interface_stayed_down` check** is replaced by these.
- **A leak is a leak.** A probe that gets out fails at once, even in an attempt that's being discarded, and isn't run again. The phase's capture and host counter judge every attempt, discarded ones included.
- **The same interface (from the review).** An attempt also counts only if the interface's ifindex is the same just before and just after the probe. A different one means gluetun recreated the interface in between, even if it's down both times, so the attempt is discarded.
- **An interface check that never runs by its deadline** (`IFACE_WAIT`) has already been retried for as long as allowed. The rest of phase B's probes are then skipped without more waiting, and `probe_ran` fails as before.
- **Phase B's own deadline (from the review):** `HELD_PHASE_WAIT` seconds (default 300) from the phase's first probe, well inside the step's 900 s. Past it, no further attempt starts: the rest of the probes are skipped and the phase fails `vpn_interface_held_down`. It never passes.
- **Not chosen (runner's call): pausing gluetun's own VPN restart** during phase B. It would change the system under test.

## Docs

- `docs/operations/cloud-fleet-runbook.md` §3.2 lists `capture_stopped_<name>` and `vpn_interface_held_down` as the harness's own failures, with what to do.
  - **For `capture_stopped_<name>`:** through `/fleet run` (SSH as root) it shouldn't happen, so the reader tells the runner. Run by hand from a snap terminal or an SSM session, it's run again from an SSH root shell. The stuck `tcpdump` keeps running until it's killed from an unconfined shell or the VM reboots (from the review).

## Tests

Each new test fails with its change reverted.

- **`test_a_capture_a_term_leaves_running_is_killed`:** a capture that ignores TERM gets KILL after its deadline, and the run passes.
- **`test_a_capture_that_cant_be_signalled_fails_its_own_line`:** a capture neither signal can stop (the `kill` stub refuses them, for the host capture and for a phase's) fails `capture_stopped_<name>` and nothing else. The exit then makes one more KILL attempt at it. Before this change, the same run hung until the test's 120 s timeout.
- **`test_a_capture_that_already_died_is_never_signalled`:** the phase and host captures exit early; no signal goes to either PID. It fails without the parent check.
- **`test_phase_b_discards_an_attempt_across_a_recreated_interface`:** a new ifindex across a probe discards the attempt, and the probe runs again. It fails without the ifindex comparison.
- **`test_phase_b_ends_at_its_own_deadline_as_a_failure`:** past the phase's deadline no phase B probe runs, and the run fails `vpn_interface_held_down`. It fails without the deadline.
- **`test_phase_b_takes_the_interface_down_again_and_runs_the_probe_again`:** gluetun brings the interface back after two attempts. The interface goes down again each time, and the third attempt counts. The test fails with `HELD_TRIES=1`.
- **`test_phase_b_fails_when_the_interface_wont_stay_down`** and the existing "comes back up and stays up" case: no clean attempt, so the probes are skipped and the run fails `vpn_interface_held_down`, with no leak line.
- **`test_a_leak_in_a_discarded_phase_b_attempt_is_a_leak`:** the probe that got out in a discarded attempt fails.
- **The existing negatives are unchanged.** The never-running interface check still fails `probe_ran`, and now fails `vpn_interface_held_down` too.
- **No fixed sleeps:** every new wait polls a condition against a deadline of at least 10 s.
