# X-gluetun-leak: close gluetun's kill-switch leak for established flows, and stop overclaiming

**Runner brief, 2026-11-12 (UTC).** Opus, /security-review. Testing blocker #1: the fleet crawl stays blocked until the leak test passes.
- **Branch:** `slice/X-gluetun-leak` from fresh `origin/main`.
- **PR title:** `X-gluetun-leak: Close gluetun's established-flow leak when the tunnel drops`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## The finding (the testing session, session_01MMjhnBD1KHxsn7QRHHmpeM; the VM's diagnosis at 16:58Z)
1. **What left the VM.** During the leak test's tunnel-down phases (`infra/fleet/steps/vm/leak-test.sh`), 61 packets crossed the compose bridge outside wg0, and the host's eth0 saw all of them:
   - 46 were TCP teardown (FIN/PSH/ACK, ACK) to a DoT resolver on :853 and to :443, with the tunnel's 10/8 source address, not NATed;
   - 15 were ICMP port-unreachable to the VPN endpoint, NATed to the host address.
   - There were no new connections and no clear-text DNS.
2. **The cause.** gluetun v3.41.3 (the digest pinned in `infra/fleet/steps/vm/compose.yaml:17`) installs `-A OUTPUT -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT` with no `-o` match, ahead of the `-o wg0` accept. When wg0 drops, established flows route out via eth0 and that rule passes them.
3. **Scope.**
   - the fleet VMs;
   - Lodestar's own companion templates: `packaging/companions/compose/{gluetun,prowlarr-gluetun,qbittorrent-gluetun}.yml` and `packaging/companions/unraid/*gluetun*/my-gluetun.xml`;
   - `infra/akamai/terraform/pia-example/compose.vpn.yaml` (`:latest`, unpinned);
   - every doc that promises the kill switch blocks all traffic: `docs/operations.md` §4.3, `docs/user/**`, the README and the infra READMEs.

## What to do
1. **Find the right fix, from primary sources.** Use gluetun's own repository, releases, wiki and issues, and its source for the firewall rules, as of v3.41.3 and later. Answer these, citing what you read:
   - (c) Does a newer gluetun release restrict the ESTABLISHED accept to the tunnel interface, or drop it on tunnel loss? Give the exact version and commit.
   - (a) Is there a supported setting or a post-rules hook (for example, gluetun's `/iptables/post-rules.txt`) that adds `-I OUTPUT 1 ! -o <tun> -m conntrack --ctstate ESTABLISHED,RELATED -j DROP`, or the equivalent, keeping loopback and the docker-network rules gluetun needs? Check it for IPv6 too (ip6tables).
   - (b) A host DOCKER-USER rule is the operator's own defence-in-depth for the fleet VMs only. It's never something we ask an Unraid user to do.
2. **Prefer, in order:**
   1. a pinned gluetun version that fixes it;
   2. a post-rules file shipped with our templates and fleet compose. It's read-only, mounted, and documented line by line.
   Never loosen gluetun's firewall in any way. If neither works for every template, say so and stop; the runner asks the owner.
3. **Apply the fix** to the fleet compose (keep the digest pinning; `infra/fleet/tests/test_steps.py` enforces it) and to the companion templates. Keep the templates' placeholder digests as they are, if that's the existing convention; check how the catalogue resolves them. Mounting the post-rules file needs a host path: follow the templates' existing volume conventions.
4. **Make the leak test strict enough to catch this.** Assert zero packets from the compose bridge leave via the host's eth0 during tunnel-down, apart from the VPN endpoint's own UDP to bring the tunnel back. If the test's current pass criterion missed this, fix the criterion in `leak-test.sh` and its tests.
5. **Docs.** Correct every overclaim: say exactly what the kill switch covers, with the fix in place and without it. Use the `user-docs` skill for `docs/user/**` and `docs/operations.md`. Write nothing about the test VM's addresses or firewall details.
6. **Hand-off to testing.** Once the branch is pushed, message the testing session (session_01MMjhnBD1KHxsn7QRHHmpeM) with the branch and the exact command to re-run `leak-test.sh`. Put its result, pass or fail with packet counts by class, in the PR description. Don't merge-wait on it: the runner decides whether a VM pass is required before merging.

## Owns
- `infra/fleet/steps/vm/**`, `infra/fleet/tests/test_steps.py`
- `packaging/companions/compose/*gluetun*.yml`, `packaging/companions/unraid/*gluetun*/**`, plus a new post-rules file under `packaging/companions/` if needed
- `infra/akamai/terraform/pia-example/compose.vpn.yaml` and that README's kill-switch lines
- the kill-switch passages in `docs/operations.md`, `docs/user/**` and `README.md`
- `docs/decisions/2026-11-12-X-gluetun-leak.md`

Ask the runner before touching `crates/**`, `packaging/companions/catalogue.json` or the companion detection code. Defence-in-depth in Lodestar itself (binding sockets to the tunnel interface) is a separate slice; propose it in the decisions file.

## Never
- Don't run anything against cloud accounts. No terraform. Don't read AWS_* variables. Don't touch the owner's machines.
- Don't record test-VM IPs, firewall rules or credentials.
- No fixed sleeps in tests.

## Done
- the infra tests (`python3 -m unittest` in `infra/fleet/tests`) and any template checks CI runs on these paths;
- a /security-review on your own diff;
- send the runner `ready <n> <full sha>` once ci-ok is green.

**Session window (owner, 2026-11-12):** sessions are cut back at 00:00Z on 2026-11-13. This is a cloud-testing blocker, so the runner may keep it after the cut; the runner tells you. If it's cut, push your work in progress, open a draft PR and report.
