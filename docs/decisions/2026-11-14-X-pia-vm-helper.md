# X-pia-vm-helper decisions (PIA bridge from #632, part b)

Part (b) of the #632 split (`docs/owner-decisions.md` §2.71; the runner's split and sudo model in `docs/decisions/2026-11-14-runner-trackq-order.md`). Built from `main`; #632 stays the owner's branch and was only read. Flagged to the owner by the runner (§2.19) where a bullet says so.

## What was taken from #632
- **The root-owned helper** `/usr/local/sbin/lodestar-vpn-helper`, written by cloud-init with its fixed verb list. It takes exactly one verb and refuses anything else. Its tunnel gate is #632's ten checks (health, image, handshake, transfer, PIA endpoint and CA, firewall, DNS, egress, ISP, crawler namespace), and `write-config` keeps #632's typed `wg0.conf` schema and PIA server-list and CA binding.
- **Its sudoers rule**, `lodestar ALL=(root) NOPASSWD: /usr/local/sbin/lodestar-vpn-helper`, in `/etc/sudoers.d/90-lodestar-vpn-helper` (root:root 0440).
- **The root-owned stack:** `/etc/lodestar/vpn/compose.yaml` (from `pia-example/compose.vpn.yaml`) and `/etc/lodestar/vpn/ca.rsa.4096.crt`, both root:root 0644. `wg0.conf` lives in `/run/lodestar/vpn` (root 0700, tmpfs), as in #632.
- **`main.tf`:** the two template inputs those files need. Nothing else.
- **The privilege-boundary test** (`tests/test_linux_privilege_boundary.sh`): its real-user, sudoers and network-namespace checks, now behind `--privileged`.

## What was left out, and why
- **#632's narrower sudo model** (sudo for the helper only, `users.sudo` removed). The runner kept main's model (below).
- **#632's `main.tf` removal of `linode_id`** from `fleet_hosts`: the fleet control plane reads it.
- **#632's compose changes that would undo main's:** gluetun v3.40.0 in place of main's v3.41.3, and dropping Lodestar's post-rules (the X-gluetun-leak fix). The compose stays on main's digest and post-rules; a test in `infra/fleet/tests/test_steps.py` already holds it to the fleet's.
- **`cap_drop: ALL` on gluetun:** the fleet's tested gluetun has the default capabilities plus `NET_ADMIN`, and gluetun's health check pings need `NET_RAW`. Dropping them is unverified on v3.41.3, so it waits for a live run.
- **The crawler service in the compose file:** #632's named an image (`ghcr.io/dvortsis/lodestar:v0.5.1`) that isn't Lodestar's published name, by tag, not digest, with every capability dropped (Lodestar needs `CHOWN`, `SETGID` and `SETUID` to hand `/data` over and drop root). It stays documentation-only in `crawler-service.example.yaml`, now with the fleet's capabilities and `service_healthy`. `start-crawler` runs the gate, then refuses until the file has a `crawler` service.
- **Two unused verbs**, `query-dns-vpn` and `verify-dns-route`. #632's bridge never calls them, and `verify-dns-firewall` already checks the route.
- **A second copy of PIA's CA.** `main.tf` reads the file deliver-pia already pins (`infra/pia-bridge/vm/pia-ca.rsa.4096.crt`, `docs/decisions/2026-11-09-OP-2b.md`). It's byte-identical to #632's `pia-example/ca.rsa.4096.crt` and to `ca.rsa.4096.crt` in `github.com/pia-foss/manual-connections` (checked 2026-11-14). SHA-256 of the file: `32e9b1d1433ea97614f2a14c6e358e3f57c0570cc9f6b2ee812699ba696c66ab`. The test pins it.
- **Bootstrap failing on a missing kernel WireGuard** (#632). It would change whether a fleet VM boots at all. The check is in the helper's `preflight` and `start-vpn` instead.
- Everything else in #632 (scripts, the bridge, the PowerShell, `infra/aws/**`, `infra/fleet/**`) belongs to X-pia-bridge-manual and X-pia-bridge-docs, or is dropped for main's (part a).

## The sudo model kept (the runner's decision, flagged to the owner)
- **Main's grant stays:** `users.sudo: "ALL=(ALL) NOPASSWD:ALL"` for `lodestar`, password locked. The fleet steps and deliver-pia keep running as root through `sudo -n`, unchanged. The helper's rule is added beside it. With the broad grant still there, the helper is no boundary today; it becomes one when X-fleet-sudo-narrow drops the grant.
- **The tie test:** the offline check reads every `sudo` use in `infra/fleet/steps/**`, `infra/pia-bridge/**` and, once it exists, `infra/akamai/terraform/scripts/**` (tests excluded), and checks each is allowed without a password by the template's sudoers lines, as sudo would match them (secure_path, `NOPASSWD`, run-as root, arguments). Today that's `bash` (fleet steps and deliver-pia) and `flock` (deliver-pia's clean check). A `sudo` without `-n`, with another option, or in a form the test can't read fails it, so a new privileged command can't slip past. A self-check proves the matcher fails with the helper rule alone. visudo checks the lines too.
- **Follow-up: X-fleet-sudo-narrow** moves every fleet step and deliver-pia behind root-owned fixed-verb helpers, then drops the broad grant. Then the tie test proves nothing still needs it.

## Changes to #632's helper
- **The Ubuntu 24.04 awk fix (§2.71 (b)).** Ubuntu 24.04's `awk` is mawk 1.3.4-20240123. It panics on #632's endpoint pattern `^[0-9]{1,3}(\.[0-9]{1,3}){3}:[0-9]{1,5}$` (`REcompile() - panic: values still on machine stack`), so `write-config` refused every config. Every awk program now avoids `{m,n}` intervals and `^`, using `split` and plain character classes. The test runs the validators under mawk, gawk and busybox awk, whichever are installed, and requires mawk on Ubuntu 24.04.
- **Main's post-rules pass the firewall check.** #632's rule check refused any rule it didn't know, so main's `CONNMARK` post-rule would have failed every gate. It now allows exactly that rule. It also requires post-rules 1 and 2, so a gluetun without the leak fix fails closed.
- **This stack only.** The compose file is project `lodestar-vpn` (and the helper always passes `-p lodestar-vpn`). The gate requires gluetun's compose project label to match, so it never vouches for the fleet's gluetun. Both stacks name their container `gluetun`, so they never run on one VM together.
- **gluetun's digest** is main's v3.41.3 (`fa19cc76…`), and the test ties the helper's, the compose file's and the fleet's together.
- **`write-config` reads at most 4 KiB from stdin,** writes to a `mktemp` file, refuses control characters (the old check missed them, despite its comment), and replaces the target with `mv -T`, so a symlink there is replaced, never followed.
- **Route and DNS lookups use documentation addresses** (`ip route get 192.0.2.1`, `nslookup example.com`), since `ip route get` sends nothing.
- **Kernel WireGuard is required in `start-vpn`** as well as `preflight`, and gluetun is set to `WIREGUARD_IMPLEMENTATION: kernelspace`, since the gate reads the tunnel with the host's `wg` over netlink. IPv6 is turned off inside gluetun (sysctls), which the gate checks.

## Checks
- **Offline (`--offline`, the default):** no root, network, cloud or terraform; runs on stock Ubuntu 24.04 with bash, python3 and python3-yaml. It renders the template with a small renderer (checked byte for byte against Terraform 1.10.5's `templatefile` while building this), parses it, runs `cloud-init schema` where installed, the tie test, visudo, `bash -n`, shellcheck, the verb list, the path and digest ties, the base64 size of the user data against Linode's 65,535 characters (about 58,700 now), and the validators under each awk.
  - The schema's one finding is main's `ssh_genkeytypes: []` (OP-2: generate no other host key). cloud-init reads an empty list as none, so the test allows that line only.
- **Privileged (`--privileged`):** #632's test, rewritten to use documentation addresses (203.0.113.10) and a stubbed PIA. It runs only as root, in CI or with `LODESTAR_ALLOW_PRIVILEGED_TESTS=1`, and restores what it changed. It passed in a disposable Ubuntu 24.04 container while building this.
- **In CI (runner-approved, 2026-11-14):** one step in `ci.yml`'s infra job runs `--offline`. The runner widened this slice's owns by `ci.yml` for that step only; nothing else in the file changed. The job's path filter already fires on `infra/`. Where shellcheck or cloud-init is missing, the offline mode prints a SKIP for that check only; the tie test never skips.
- **Live verification** on a VM (gluetun v3.41.3's exact `iptables -S` shapes, the health check, the crawler gate) goes with X-pia-bridge-manual's first run. The gate fails closed on any rule shape it doesn't know.
