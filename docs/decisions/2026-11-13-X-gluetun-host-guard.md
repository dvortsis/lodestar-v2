# X-gluetun-host-guard decisions (#PR)

The cloud tester's drill 3 (2026-11-13) stopped gluetun for 60 s mid-block, with Lodestar still running. The host sent 14 ICMP packets from its own public address to the VPN endpoint on its egress interface. Nothing from inside the tunnel leaked, and the bridge's non-endpoint egress was 0. The verdict still fails, because the plan counts any packet outside the tunnel. This supersedes nothing. It extends the host guard of `2026-11-12-X-gluetun-leak.md` ("The leak test") from the leak test's own run to the whole block.

## The cause
- **The same host-stack path as the leak test's recreate window.** When gluetun stops, Docker deletes the conntrack entries that name the container's IP. The endpoint keeps sending WireGuard UDP for the old session for a while. Those packets match no NAT state, so they go to the host's own INPUT, and the host's kernel answers with "port unreachable" from its public address.
- **Why the guard missed it.** #800's host guard (an INPUT DROP for `-s <endpoint> -p udp --sport <port>`, tagged `lodestar-leak-host`) was held only while `leak-test.sh` ran. During a block, nothing held it.

## The change
- **start-block holds the guard for the whole block.** It reads and validates the endpoint, installs the guard after its no-block checks, and only then runs the leak test, which recreates gluetun. The guard stays on when start-block returns. copy-off removes it before it removes the marker. A guard that won't come off fails `host_guard_released` and leaves the block recorded, so copy-off can run again.
- **One shared implementation, in `block-lib.sh`.** `endpoint_read`, `host_guard_rule`, `host_guard_hold`, `host_guard_release`, `drop_all` and `clear_tagged` are defined once there. leak-test.sh, start-block and copy-off all call them, and leak-test.sh no longer has its own copies.
- **Holding the guard is idempotent and never leaves a gap.** `host_guard_hold` inserts the new copy at the top of INPUT first. Then it removes every other rule with the tag: a stuck copy, a duplicate, or an earlier endpoint's rule. Finally it checks the rule with `-C`. Re-running start-block leaves exactly one rule, and the host is never without one at any point.
- **copy-off removes the guard by tag,** whatever endpoint the rule names. Stuck copies and an earlier endpoint's rules come off too.
- **The leak test under start-block.** start-block passes a fifth argument, `held`. The leak test then checks with `-C` that the guard is present for the same endpoint. If it isn't, the test fails `host_guarded` before gluetun is recreated. It never inserts a copy, removes one or clears by tag, so the block's guard survives the test's exit and its signal paths. On its own, without `held`, the leak test behaves as before: it holds its own copy, through the same `host_guard_hold`, and releases it at the end. Any other fifth argument fails `args_valid`. The DOCKER-USER recreate guard is still the leak test's alone.
- **The endpoint is validated as before, and a little more strictly.** It comes from the root-only `wg0.conf`. It must be IPv4, because the guards are IPv4 rules, with a port from 1 to 65535. The IPv4 check now also refuses an octet over 255. Before, iptables would have rejected such an address at insert time, which failed closed but later. The endpoint is never printed: output is fixed names only, and tests check that the address isn't in the output.
- **A failed start keeps the guard.** If start-block fails before the marker (the leak test failing, say), it leaves the guard on. The rule can't touch the tunnel: a live session's replies are de-NATed and forwarded, never delivered to the host. Keeping it also keeps covering a gluetun left running on the VM. The next start-block replaces it, and a stand-alone leak test or copy-off removes it.
- **Tests.**
  - `test_block.py`: the guard is in before the leak test runs and is held through start-block. copy-off removes it. A re-run leaves exactly one copy and removes leftovers after the new copy is in. A guard that can't be set, or a leftover that won't come off, stops the run before gluetun is recreated. A bad endpoint is refused before any iptables call. A stuck copy fails copy-off and keeps the block.
  - `test_steps.py`: under `held`, the leak test adds and removes nothing, and refuses to run without the guard. An unknown fifth argument is refused. More bad endpoints are covered (an octet out of range, IPv6).
  - `test_block.py` gained an iptables stub and a `wg0.conf` for this. It was outside the brief's owned paths and was added with the runner's OK.

## In a user's own setup
- **The same thing can happen.** When a user's gluetun stops, restarts or is updated, Docker flushes its conntrack in the same way. The VPN server's leftover UDP still reaches the home router's public address. The router's own NAT entry, typically 30 to 180 s for UDP, forwards it to the host. The host's kernel can then answer with "port unreachable", and the router sends that back out to the VPN server outside the tunnel. Whether it does depends on the host's firewall: many NAS and desktop firewalls drop unsolicited inbound UDP silently, and some routers drop the reply.
- **The privacy impact is small, but under the plan's rule it's still a leak.**
  - **The recipient is the VPN server.** It already knows the home address, because that's where the tunnel came from.
  - **The content is the server's own packet.** An ICMP error carries the start of the packet that caused it, here the server's own encrypted WireGuard datagram. It adds nothing about the user's traffic.
  - **What it reveals:** that the host is up and that the session has gone.
  - **Nothing from Lodestar or the user's traffic is in it.** The user guide's sentence "Nothing Lodestar or gluetun sends to the internet goes around the VPN" still holds, because the host's kernel sends these packets, not Lodestar or gluetun.
  - **Still, it counts.** It is a packet outside the tunnel, so the plan's rule counts it.
- **Recommendation: a short docs note, no template change** (for the runner to place).
  - **Templates can't do it.** A gluetun template can't add host iptables rules: the post-rules run inside gluetun's namespace, and the host's INPUT is out of reach.
  - **Don't ask users for host rules.** It would contradict the earlier decision that no Unraid user is asked to add them, and the guide's safe-setup voice (`docs/owner-decisions.md` §2.14).
  - **What the note says:** a sentence in the user guide's chapter on how the VPN protects you. While gluetun restarts, the home computer may tell the VPN server, once, that the old connection is gone. The server already knows the home address, and nothing from Lodestar is in that reply.
- **Not changed here:** no user docs, templates or product files (the brief).

## Follow-ups (for the runner)
- **A VPN renewal mid-block.** `deliver-pia` (renew-vpn) replaces `wg0.conf` but doesn't recreate gluetun. If gluetun restarts after a mid-block renewal, it can come back on a different endpoint, which the block's guard doesn't name. The next start-block re-reads the endpoint, so only the rest of that block is affected. Options:
  - renew-vpn refuses mid-block;
  - renew-vpn re-holds the guard (it can call `host_guard_hold`).

  Either belongs to the slice that owns `infra/pia-bridge`.
- **Between blocks.** gluetun keeps running on the VM after copy-off with no guard, so a stop there isn't covered. Nothing is measured then. If the crawl phase wants standing cover, it can hold the guard from `up` to teardown, as the earlier file's "(b) Host rules" suggests.
