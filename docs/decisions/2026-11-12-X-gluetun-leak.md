# X-gluetun-leak decisions

Fixes testing blocker #1: gluetun's kill switch let packets of established flows leave by the host's network when the tunnel went down. The testing session found 61 such packets on a fleet VM on 2026-11-12. Sources were read in gluetun's repository at tag `v3.41.3` (commit `3d1e20c5`, the digest the fleet pins) and at `master` (`236b10ba`, 2026-11-12T04:29Z).

## What gluetun does
- **The cause, confirmed in source.** `enable()` (`internal/firewall/enable.go`) sets DROP policies, then appends `OUTPUT -o lo ACCEPT`, then `acceptEstablishedRelatedTraffic`, which is `-A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT` with no interface (`internal/firewall/iptables.go`). The `-o wg0` accept comes later (`SetVPNConnection`, `vpn.go`). When wg0 goes down, the kernel routes a flow opened through the tunnel out of eth0, still carrying the tunnel's source address, and that first accept passes it.
- **(c) No release fixes it.** `v3.41.3` is the newest tag. `master` still has the unrestricted accept. Commit `2429c0d` (#3158, on `master`, in no tag) adds `flushExistingConnections` (`internal/firewall/flush.go`). That flushes conntrack, or falls back to the `GLUETUN_PUBLIC_ONLY` chain, but only when the firewall is turned on, not when the tunnel drops. A newer version is not the fix.
- **(a) The supported hook is `/iptables/post-rules.txt`.** `runUserPostRules` runs at the end of `enable()`, once, before the VPN starts. It runs each line that starts with `iptables`, `ip6tables` or their `-nft`/`-legacy` forms, and skips every other line, so comments are inert. A missing file is skipped without a word. A failing line makes `enable()` return an error: its deferred `fallbackToDisabled` resets the policies to ACCEPT, and `main.go` then exits. That happens before any VPN or app traffic, but the rules must never fail, and the leak test checks that they loaded.
- **gluetun's image sets `VPN_INTERFACE=tun0`** (`Dockerfile` line 79), and WireGuard takes that value (`internal/configuration/settings/wireguard.go`). Rules that name `wg0` need `VPN_INTERFACE=wg0` set explicitly. The fleet compose and the PIA example set it.

## The fix
- **Three rules per family, all inserted at the top of OUTPUT** (`packaging/companions/gluetun-post-rules.txt`, which explains each line):
  1. `-o wg0 -j CONNMARK --set-xmark 0x800000/0x800000` marks every connection that sends a packet through the tunnel. It changes no verdict.
  2. `! -o wg0 -m connmark --mark 0x800000/0x800000 -j DROP`: a connection that used the tunnel never leaves by another interface.
  3. `! -o wg0 -p icmp --icmp-type port-unreachable -j DROP` (and the ICMPv6 equivalent): this was the VM's other class, port-unreachable replies to the endpoint sent from the host's address.
- **Why a connection mark, not an interface rule alone.** The brief's sketch, `! -o <tun> ESTABLISHED,RELATED -j DROP`, would also drop WireGuard's own UDP to the endpoint, the replies to the web UI and companion traffic on the Docker network, because all of them are established flows on eth0. A mask-free exemption list would need the endpoint and the Docker subnet, which a static file can't know. The mark catches exactly the flows that have used the tunnel. Loopback, the Docker network, inbound UI connections and the WireGuard socket never do, so they keep working.
- **It never loosens gluetun's firewall.** The rules only mark or drop, and a test enforces that (`test_steps.py`: no ACCEPT, RETURN, policy, flush or delete). The mask uses one bit, so a future gluetun connmark (master's `0x567`) keeps its own bits.
- **Checked three ways:**
  - The syntax on iptables 1.8.10 (nft), both families.
  - On real gluetun `v3.41.3` (the pinned digest), started here with a fake tunnel: it loads the read-only file and the three rules head OUTPUT in iptables and ip6tables, and they stay there through gluetun's own VPN restarts. A missing file that Docker turned into a folder stops gluetun with `read /iptables/post-rules.txt: is a directory`.
  - A network-namespace rebuild of the VM's case, with gluetun's v3.41.3 OUTPUT/INPUT rules and a TCP flow opened through `wg0` before `wg0` went down. Without the post-rules, 4–5 PSH/FIN packets left by eth0 with the tunnel's source address. With them, none did. A web-UI request into the namespace and a connection to the Docker network both still worked.
- **How it's mounted.** Compose `configs:` with inline `content:`, which Compose mounts read-only at `/iptables/post-rules.txt`, in the fleet's `compose.yaml` and the PIA example. This needs no new file copied to the VM (`steps/run.py` is unchanged) and no host path. Tests check that both copies equal `packaging/companions/gluetun-post-rules.txt` byte for byte.
- **The PIA example is pinned** to the fleet's digest (it was `:latest`), the version the rules were checked on.

## The leak test
- **New checks:**
  - `post_rules_ipv4` and `post_rules_ipv6`: the DROP rule is present for gluetun's own interface, in whichever iptables backend it uses. Because gluetun silently skips a missing file, the test checks rather than assumes.
  - `no_host_egress_<phase>`: a packet counter in the host's DOCKER-USER chain, just below the endpoint block, matching the compose bridge in and the egress interface out. It must read 0. This is the brief's "nothing from the compose bridge leaves the host's eth0 apart from the endpoint's own UDP", measured where the packets leave rather than on gluetun's veth. An unreadable or unset counter fails.
  - Per-class counts of the veth capture (`captured_tcp/udp/icmp/other_<phase>`), as evidence.
- **The earlier criterion.** The veth capture would also have seen these packets. The new checks make the cause explicit (the post-rules) and add the host-side view the brief asks for.
- **(b) DOCKER-USER.** The test's counter rule only counts and is always removed. A host DROP rule for the fleet VMs would be defence in depth for the operator only, never something an Unraid user is asked to do. It isn't added here, because it would hide a gluetun regression from the leak test. Propose it with the crawl phase if wanted, below the counter.

## Not done here (for the runner)
- **The generated companion templates** (`packaging/companions/{compose,unraid}`) are golden output of `crates/companions` (`tests/templates.rs`, byte for byte), and that test also forbids extra files in those folders. Fixing them needs generator changes in `crates/**`:
  - compose: `configs:` content plus `VPN_INTERFACE=wg0`;
  - Unraid: a read-only Path to a host copy of the file, plus `VPN_INTERFACE=wg0`;
  - correcting the Overview's "Its firewall blocks all traffic while the VPN is down".

  The brief requires asking first, and the runner was asked. The user guide marks template support **(planned)** and gives the manual steps.
- **Defence in depth in Lodestar itself**, binding its sockets to the tunnel interface so a rerouted flow fails at the socket: propose it as its own slice.
