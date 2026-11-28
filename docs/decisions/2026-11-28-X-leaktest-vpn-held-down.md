# X-leaktest-vpn-held-down decisions

The runner's brief (2026-11-28): after X-leaktest-wg0-flake (#988), the cloud lane still saw `vpn_interface_held_down` once (2026-11-22). gluetun restored its interface many times, so most phase-B probes were skipped. 0 packets leaked, and a second run passed. Round 1 runs this test on every arm before and after every day, so a harness flake can cost a day. Calls not marked as the runner's are this PR's; the runner flags them to the owner (`docs/owner-decisions.md` §2.19). Every bullet of `2026-11-20-X-leaktest-wg0-flake.md` stands, "No pause between attempts" included: there's still no wait between attempts. The hold below is added alongside them.

## The cause, from gluetun v3.41.3's source (the pinned image)
- **With the endpoint blocked, gluetun restarts its VPN every few seconds, and each restart makes a new interface.**
  - When a tunnel comes up, gluetun runs a startup health check: a TCP+TLS dial with a 6 s timeout (`internal/healthcheck/checker.go`, `startupCheck`).
  - Phases A and B block the endpoint, so that check always fails.
  - With `HEALTH_RESTART_VPN` at its default (on), each failure restarts the VPN with no backoff (`internal/vpn/tunnelup.go`, `restartVPN`).
  - A restart deletes `wg0` and creates it again, up and with a new ifindex (`internal/wireguard/run.go`). The cycle is about 6 s plus setup.
  - Nothing else in gluetun watches the link. There's no netlink subscription, so the health-check loop is the only thing that restores it.
- **A phase-B attempt takes longer than that cycle, so a clean attempt almost never happens.**
  - A clean attempt needs one interface to stay down, with one ifindex, from the first interface check to the second ifindex read.
  - That span is five to seven probe containers plus the probe itself. `curl -m 8` waits its full 8 s when the firewall drops its packets, so its attempt is well over 6 s on any VM. A `dig +time=3` attempt is borderline: it fits only when containers start quickly.
  - Retrying until `HELD_PHASE_WAIT` (#988) only gave more chances to land between two restarts. Short probes on a fast VM sometimes did, which is why the failure became rarer but didn't go away.

## The fix
- **A retry holds gluetun still** (runner, accepted for every probe that tests kernel state). From the retry's first interface check to its second ifindex read, `docker pause` freezes gluetun's processes. `docker unpause` resumes them before the verdict.
  - The probes still run in gluetun's network namespace. moby's `getNetworkedContainer` checks only `IsRunning` and `IsRestarting`, and a paused container still counts as running, so a container can join a paused one's namespace. A local daemon here (Docker 29, no real infrastructure) showed the same, and showed `exec` refused while paused.
  - A frozen gluetun can't run its restart, so the retry gets its clean attempt, whatever the restart loop's timing.
  - It's the smallest change that removes the cause. The other options were worse:
    - turning `HEALTH_RESTART_VPN` off would change the production stack;
    - stopping the VPN through gluetun's control server would delete the interface instead of taking it down (see the DNS bullet below);
    - steering routes with an `ip rule` would replace what the bracket checks.
- **Each probe's first attempt still runs with gluetun live,** as before. A phase with no restarts never pauses, and gluetun's own reactions (its DNS forwarder, its health checks) still face every probe once.
- **`dns_blocked_ifdown` is never held** (runner). It asks gluetun's own DNS forwarder at 127.0.0.1, and its point is that the forwarder, running with the interface down, doesn't leak.
  - A paused forwarder proves nothing, and a query it took in while paused could leave through a new tunnel once it resumes.
  - So every attempt of this probe runs live, as before this PR (`held --live`). A clean attempt still needs the interface down before and after, with the same ifindex, and a probe with none by the deadline still fails `vpn_interface_held_down`.
  - So this probe is exactly where it was. When the restart lands in every live attempt (on a slow host), it still fails `vpn_interface_held_down`.
  - **It runs last in phase B** (runner). Its live retries can use up the phase's deadline, and they'd leave the probes after it with only their first attempts. So phase B runs the held probes first (`dns_direct_blocked`, then the UDP probe), and `dns_blocked` gets whatever time is left. Phase A's order is unchanged.
    - The direct query goes to 1.1.1.1 and never reaches gluetun's forwarder, so the name `dns_blocked` asks is still new to the forwarder's cache.
    - No verdict changes: DNS still needs a clean live attempt or fails `vpn_interface_held_down`.
  - **The alternative, if the live DNS probe keeps losing on the VMs** (for the runner). Stop the VPN for that probe through gluetun's control server, with `PUT /v1/vpn/status`, which is unprotected by default in v3.41.3 (`internal/server/middlewares/auth/middleware.go`).
    - A stop runs the VPN loop's `cleanup` and cancels the VPN. That deletes `wg0` and stops the health checker, so no restart loop runs.
    - The VPN cleanup doesn't touch the DNS loop, so the forwarder keeps running live.
    - The probe would then run with no tunnel interface at all, instead of one taken down. The bracket reads that as down with the same (empty) ifindex.
    - That changes what the probe proves, so it isn't in this PR.
- **The report says which kind passed** (runner). After phase B's verdict line, each probe that passed gets `count clean_held_<probe> 1` or `count clean_live_<probe> 1` (`udp_ifdown` for the capture-only probe). These are counts only. A held retry whose pause was refused runs live, and is counted live.
- **Failures fail closed.**
  - **A refused pause:** the retry runs with gluetun live, as before this PR, and the bracket judges it.
  - **A pause that returned an error but froze gluetun anyway:** it's resumed at once.
  - **A gluetun that won't resume** (checked through `docker inspect`) ends the phase. No further attempt runs, and the phase fails `vpn_interface_held_down`.
    - Docker refuses `docker exec` into a paused container, so the post-rule checks after the phase fail too.
    - The recovery's recreate should then replace the frozen container. That's shown only with the stub, not against real Docker.
  - **A signal mid-retry:** the exit trap resumes gluetun. If that fails, it prints `fail gluetun_resumed`, beside `guards_released`.
- **Time:** each probe now needs about two attempts: a live one that's discarded, then a held one. With six probes, phase B stays inside #988's budget, and a phase with no restarts is as before. A pause lasts one attempt, about 10–20 s.

## What phase B proves, and why the verdict is unchanged
- **The same about the kill switch.** gluetun's firewall and its post-rules are kernel netfilter state, which stays in force while its processes are frozen. So a held retry still tests exactly what phase B is for: with the VPN interface down, only gluetun's firewall stands between the probes and `eth0`.
  - A gluetun with its firewall off still leaks on every attempt.
  - A gluetun without its post-rules still leaks the open flow, and the capture and the counter see it.
- **Fewer meetings with a live gluetun.** Before, a probe met a live, restarting gluetun on every attempt, and a leak on any of them failed at once. Now each held probe meets it once, on its first attempt. That's acceptable because the capture and the counter run through the live stretches between holds, so gluetun's own traffic there is still judged.
- **What a held attempt doesn't prove:** anything about gluetun's own processes during that attempt, meaning its DNS forwarder and its health checks. That's why `dns_blocked_ifdown`, the one probe aimed at them, is never held (runner). The other probes test kernel state: the firewall, the post-rules and the routes.
- **The verdict rules are as before:**
  - a probe that gets out fails at once, on any attempt, held or live;
  - the capture and the host counter run across every attempt and judge them all;
  - a probe with no clean attempt by the deadline is `skip` and fails `vpn_interface_held_down`;
  - the bracket and its ifindex check are as before.

## Tests (`infra/fleet/tests/test_steps.py`)
- **The fake gluetun** (`GLUETUN_RESTARTS`) does what v3.41.3 does with the endpoint blocked: while it isn't paused, a restart (the interface made again, up, with a new ifindex) lands in every live attempt whose probe outlasts the cycle (`curl -m 8`, `nc -w 5`). With `=all` it lands in every live attempt, as on a slow host.
  - The stub's `docker pause` and `docker unpause` behave as Docker's do: `exec` into a paused container is refused, and unpausing one that isn't paused fails.
  - A recreate replaces a paused gluetun, and the stub records it so the passing tests can show it never happened.
- **New tests:**
  - `test_phase_b_holds_gluetun_still_for_a_retry` fails before the fix (`vpn_interface_held_down`) and passes after it. It checks:
    - one pause per retry, each covering a whole attempt;
    - that every first attempt runs live;
    - the `clean_held` and `clean_live` counts.
  - `test_dns_blocked_is_never_held`: no `dig` to the forwarder ever runs under a pause, and every attempt it makes comes after the last held one. On a slow host, where every live attempt meets a restart, it fails `vpn_interface_held_down` as before, while `dns_direct_blocked` and the UDP probe still pass on held attempts.
  - A refused pause, and one that froze gluetun anyway, both still fail `vpn_interface_held_down`.
  - A gluetun that won't resume ends the phase as a failure, and the post-rule checks fail with it.
  - A leak while gluetun is held fails at once, and the capture judges it.
  - A signal mid-hold resumes gluetun.
- **Existing negatives:** every one still fails. `IFACE_REUP`, which models an interface that comes back even with gluetun held, still ends in `vpn_interface_held_down`. The whole suite passes.
