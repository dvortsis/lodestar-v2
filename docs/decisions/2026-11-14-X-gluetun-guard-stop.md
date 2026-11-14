# X-gluetun-guard-stop decisions (#881)

The test VM's re-test of the VPN leg on main 82797c01 (X-test-vm coordinator, 2026-11-14) found ICMP to the VPN endpoint leaving the host's egress interface from its public address. The host guard from #841 was held and never matched. This slice closes that window and fixes four problems the same run hit in the VM scripts (`infra/fleet/steps/vm`).

## The stop-window leak
- **Two kinds of packet left:**
  - "host unreachable" sent from 10 to 58 s after `docker stop gluetun`: conntrack kept forwarding the endpoint's leftover WireGuard UDP towards the stopped container's bridge address, and the host answered;
  - one "port unreachable" from the new gluetun container 0.1 s after `docker start`, before its firewall was up.

  Both take FORWARD or OUTPUT, never INPUT, so the INPUT guard couldn't match them.
- **What this supersedes:** `2026-11-13-X-gluetun-host-guard.md` says the endpoint's leftover packets match no NAT state and go to the host's INPUT. That's true once Docker has flushed the container's conntrack entries. Until then, leftover packets still match the old entry, are de-NATed towards the gone container's bridge address and take FORWARD, where the host answers "host unreachable". This file adds that case; the INPUT rule stays for the other.
- **The host guard is now two rules,** both tagged `lodestar-leak-host`, held and released together everywhere the guard was: start-block, the leak test's own copy, `guard_resume` (the outage watcher, the outage-watcher step and the boot guard) and copy-off. The rules are:
  - the INPUT DROP for the endpoint's UDP, unchanged;
  - a DROP for any ICMP to the endpoint in **mangle POSTROUTING**.
- **Why mangle POSTROUTING:**
  - It sees both what the host sends and what it forwards. It runs before source NAT, but the destination is the endpoint either way, so the match is the same. One rule covers the host's own "host unreachable" and a starting container's "port unreachable".
  - Docker never writes to the mangle table. The rule therefore holds before Docker starts (the boot guard runs then, when `DOCKER-USER` doesn't exist yet), and it stays above anything Docker adds.
  - It can't touch the tunnel. The tunnel is UDP, and nothing legitimate sends ICMP to the VPN endpoint outside it. Path-MTU errors for the container's own packets go to the container, not to the endpoint. A "fragmentation needed" for the endpoint's packets towards the container would only arise with a bridge MTU below the uplink's; the two are equal here, and WireGuard doesn't set DF on its outer packets.
- **Alternatives considered:**
  - Dropping the endpoint's UDP in raw PREROUTING would also drop the live tunnel's replies. It would have to be added and removed around every stop, including a crash that no script sees.
  - A rule in FORWARD or `DOCKER-USER` misses the host's own ICMP, which takes OUTPUT. `DOCKER-USER` also doesn't exist at boot.
  - Flushing the endpoint's conntrack entries when gluetun stops only works for stops a script makes, not a crash or Docker's own restart. The new gluetun's start-up ICMP is also not a conntrack matter.
- **A missing ICMP rule is a dropped guard,** like a missing INPUT rule. It's put back and recorded as `host_guard_dropped`, and one that can't be held marks the block damaged and stops gluetun.
- **A block started before this change** can still run this library: the outage-watcher step and arm-switch ship a fresh copy, and arm-switch re-stages it for the watcher. Such a block never held the ICMP rule, so a missing one isn't a drop there: it's added and nothing is recorded. start-block marks a block that held the rule from its start (`$BLOCK/guard-icmp`), and only there does a missing ICMP rule count as dropped.
- **The leak test now judges this on the host:**
  - A capture on the host's egress interface of any ICMP to the endpoint runs from before the first recreate to the end. It must read zero (`no_host_icmp_to_endpoint`), and a capture that died or can't be read fails.
  - The test ends with a stop window: `docker stop gluetun` while the endpoint still sends the old session, then a 75 s observation window (`STOP_WINDOW`; the leak lasted up to 58 s), then `docker start gluetun` without the recreate guard, as an operator's stop or a crash would happen. The tunnel must come back (`tunnel_back_after_stop`), and gluetun's post-rules must be back in both families.
  - **The window is driven, and checked.** The original leak happened with Lodestar's traffic in the tunnel. An idle tunnel gives the endpoint nothing to resend, and would read zero on any rules.
    - So a slow download through the tunnel (a throwaway container in gluetun's namespace, no capabilities) runs through the stop, and the far side keeps resending.
    - A second capture on the host's egress counts the endpoint's packets to the host during the window. With none, `stop_window_exercised` fails: the window proved nothing, which is never a pass. With no handshake it's skipped.
  - Without the ICMP rule the host judge fails. The unit test that removes the rule at the stop shows the judge catching it. A VM run on today's rules isn't part of this slice (no VM runs from this session): the coordinator's re-test after merge records one run with the rule and, by removing it by hand, one without.
- **The leak test takes about two minutes longer** in a normal run: the stop, the window, the start and the health wait.
  - The added worst case is about 7 minutes: stop 60 s, window 75 s, start 60 s, health 120 s, and the interface check's deadline with a probe in flight. A loaded host could push a whole run past 600 s.
  - start-block's inner limit on the leak test is raised from 600 to 900 s, within start-block's own 2,400 s.
  - The standalone leak-test step's 600 s is in `steps/run.py`, outside this slice. The runner is asked to raise it to 900 s.

## Data folder modes
- **The problem:** Lodestar starts as root with only CHOWN, SETGID and SETUID (compose `cap_drop: ALL`), so root can't override file modes. On every start it opens `/data` to check the owner. A 0700 folder owned by 65532 refuses that on every start after the first: `PUID/PGID: opening /data: Permission denied`.
- **The fix:** start-block, and arm-switch for a block started before this change, now:
  - create (or fix) the data and witness folders as 65532-owned, mode 0755;
  - close the stack folder to others (`o-rwx`, checked).
- **Why others may read the folders:**
  - Others' read and execute is what lets the reduced-capability root open the folder.
  - Closing the stack folder keeps every host account but root and the stack's owner from reaching the folders. Only the containers that mount them see them, because a bind mount doesn't need its host parents.
  - Files Lodestar writes inside keep their own modes.
- **Alternatives:**
  - An ACL for uid 0 depends on the `acl` package on the VM. A future chmod of the folder would also turn it off through the ACL mask.
  - Handing the folder to root before each start makes Lodestar walk and re-chown the whole tree on every start.
- **Product follow-up:** checking the folder's owner before opening it (it is readable without opening, through `symlink_metadata` or an `O_PATH` descriptor) would let a 0700 folder work in the product too. That's a product change, out of scope here.

## A locally built image
- **The new key:** start-block takes `LOCAL_IMAGE_ID` (the 64 hex characters of `docker image inspect -f {{.Id}}`) in place of `IMAGE_DIGEST`, for test runs.
- **How it's pinned:** Lodestar then runs `sha256:<id>`, content-pinned like the digest. The image is never pulled; the start stops (`lodestar_image`) unless an image with exactly that ID is present.
- **Exactly one pin:** start-block and `block.env` need exactly one of the two. The registry digest stays the default and the control side's only form. Neither script takes a pin from the environment.
- **Test runs only, and void as evidence:**
  - Such a block's evidence has no registry digest (the scrubber records `IMAGE_DIGEST`, and the collector finds no repo digest), so the analyzer's `image_pinned_by_digest` check voids its segments. That's right for a local build.
  - Recording the local ID in the evidence would be a change to `scrub.py`, outside this slice.
  - That `docker compose up` takes `image: sha256:<id>` as a local ID without trying to pull it is for the coordinator's first local-image run to confirm.

## The interface check under load
- **The failure:** `vpn_interface_stayed_down` failed twice during a 4-core image build, with nothing captured. Its probe container couldn't start inside the probe's 40 s, and a probe that never ran counts as a failure.
- **The fix:** the check now retries a probe that never ran, until a deadline of `IFACE_WAIT` seconds (120 by default, never less than 10). A probe that ran gives the verdict. Past the deadline it still fails, and `probe_ran` fails too. The retries poll once a second; nothing waits a fixed time.

## Lodestar after gluetun comes back
- **The problem:** a gluetun that restarts (same container, new network namespace) leaves a running Lodestar in the old namespace. Its API stays unreachable until Lodestar restarts.
- **The fix, and its limits:**
  - The scripts compare the two containers' namespace inodes (through their main processes) after Lodestar starts.
  - start-block and arm-switch restart Lodestar when they differ, then check again (`network_parent`). At those points Lodestar has just started, so this only catches a gluetun that restarted in the seconds between.
  - The leak test never runs with Lodestar up (it refuses), so it has nothing to restart.
  - The case the test VM hit, gluetun coming back while a segment runs, is the outage watcher's to handle. That file isn't part of this slice, and it never touches Lodestar today, because an outage already makes the segment invalid. Whether it should restart Lodestar is the runner's call.
- **Follow-up (product):** Lodestar in a shared namespace has no way to rejoin a restarted gluetun on its own. Whether the product, the Unraid template or the compose file should restart it is a separate decision.

## Tests
- **The leak test (`tests/test_steps.py`):**
  - a stop window that passes with the rule and fails without it;
  - a host capture that died or can't be read;
  - a stop or start that fails;
  - an ICMP rule that can't be held, in a standalone run and under start-block, and one that won't come off (the INPUT rule still does);
  - signals mid-recreate, which release both rules (or keep both when start-block holds them);
  - an idle stop window, and a download that won't start, both failing `stop_window_exercised`;
  - a probe that starts slowly, and one that never runs by the deadline;
  - the post-rules back after the start.
- **The block steps (`tests/test_block.py`):**
  - the folders' owner and modes, including a fix-up at arm-switch;
  - the local image's pin, a missing local image, and exactly one pin;
  - the namespace restart, done when needed and only then;
  - both guard rules held for the block, and released at copy-off, including a stuck one;
  - a dropped ICMP rule re-held and recorded, and an older block's missing one added without a record;
  - a check error never read as "gone".
