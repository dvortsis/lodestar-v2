# PR #632: controlled kernel WireGuard integration test

Prepared 2026-11-09 for `bcf434743c1219192972bf810d86b55b6236afeb`.
This is an approval-ready procedure, not authorization to execute it. No crawler
is part of this test. All cloud writes, remote connections, privileged probes,
fault injection, restart, reboot, and deletion are human-operated after approval.

## Current evidence and blockers

- CI run [42664993313](https://github.com/dvortsis/lodestar-v2/actions/runs/42664993313)
  attempts 1 and 2 contain 85 job records: 84 successful, one skipped
  (`reporting overhead`). Both complete log archives were downloaded and examined.
  No recorded job failed. Attempt 1 never created `ci-ok`; attempt 2 ran it
  successfully with exclusively successful dependency results.
- The workflow-level error in both attempts is **Internal server error**.
  Correlation IDs: attempt 1 `db1bd289-68c2-4767-82fb-878de78bf796`; attempt 2
  `c0a69671-9f46-489d-9938-0fb4d99a963c`. A cache restore egress-limit warning
  was non-fatal. There is no demonstrated source defect explaining the workflow
  conclusion; do not weaken the gate or edit the workflow to hide it.
- A targeted retry of job `129357335818` was requested using
  `gh run rerun 42664993313 --job 129357335818`. GitHub schedules dependency
  work and also copies previously successful same-commit results into a new
  attempt: job IDs and `run_attempt` alone do not prove a new execution.
  In attempt 3, Ubuntu retains its attempt-2 execution timestamps, whereas
  `infra` actually reran at 20:56–20:58 UTC and Windows started at 20:56 UTC.
  **Final attempt 3: COMPLETED / SUCCESS**, 2026-11-09 21:39:25 UTC (14:39:25
  America/Denver). It has 50 successful job records and one skipped optional
  reporting job. Required results are completed SUCCESS:
  [infra](https://github.com/dvortsis/lodestar-v2/actions/runs/42664993313/job/129433792714),
  [Windows](https://github.com/dvortsis/lodestar-v2/actions/runs/42664993313/job/129433792864),
  [Ubuntu (retained same-commit result)](https://github.com/dvortsis/lodestar-v2/actions/runs/42664993313/job/129433795814),
  and [ci-ok](https://github.com/dvortsis/lodestar-v2/actions/runs/42664993313/job/129457755632).
  No repository code change was needed. The observable cause of the earlier
  failures is GitHub's workflow-level internal error; its deeper server-side
  cause is not exposed in repository logs. The skipped job is not a passing test.
- Live inventory is **UNKNOWN**, not empty. `LINODE_TOKEN`,
  `LODESTAR_TF_STATE_BUCKET`, and `LODESTAR_TF_STATE_REGION` are unset in the
  inspection process. The observer credential process reports an expired or
  uninitialized session. No initialized S3 backend metadata exists locally.
  The local `terraform.tfstate` is a zero-byte file, not evidence of an idle fleet.
- Existing AWS/Linode instances, firewalls, volumes, reserved addresses, the
  shared fleet manifest, application lease, spend ledger, and live reaper
  configuration have not been verified. No PIA secret was retrieved.
- GitHub's `cloud-fleet` environment exists but returned no configured secrets
  or variables; `pia-delivery` returned 404. Repository secret/variable lists
  were also empty in this inspection. These are prerequisite observations,
  **not** proof that any live resources, IAM roles, or watchdogs are absent.
  Successful scheduled fleet workflow runs likewise do not establish live inventory.

## Approval package

Approve an exact target ID, source SHA, resource delta, admin `/32`, SSH key
fingerprint, test window, spend ceiling, privileged maintenance access path,
fault duration, rollback, and cleanup set. Recommended window: four hours;
compute-only ceiling for a new Nanode: eight billed hours ($0.06). Set a separate
all-in ceiling, for example $1, including any transfer/tax; it is an approval
limit, not an automatically enforced cloud spending cap.

Separate approvals cover:

1. Read-only inventory/state access and observer MFA refresh, with no credentials
   pasted into chat or logs. Do not grant an agent administrator access.
2. Reuse of one specifically approved disposable VM **or**, only after the idle
   proof below, creation of one Nanode and its dedicated firewall in shared state.
   No other instances, volumes, backups, reserved IPs, or AWS compute are needed.
3. Human remote access and the PIA bridge's restricted secret read, PIA
   authentication/key registration, and live Gluetun startup. No configuration
   export to disk, `--start-crawler`, `StartCrawler`, or `vpn-crawler`.
4. Human privileged diagnostics, a temporary **additional DROP rule**, bounded
   interruption, and Gluetun restart. VM reboot is a separately approved phase.
5. VPN/runtime cleanup and deletion of only the approved disposable resource set.
   Pre-existing fleet resources and shared S3 buckets/objects are retained.

The [fleet skill](../../.agents/skills/lodestar-fleet/SKILL.md) is located at
`.agents/skills/lodestar-fleet/SKILL.md` from the repository root. Its operating
rule is: **“An agent only reads.”** Do not add a test backdoor to the helper,
grant arbitrary sudo, enable root SSH, disable the firewall, or widen ingress.

## 1. Establish the authoritative fleet and ownership before any deployment

For the owner-run read-only access and report setup, use PowerShell 7.2+:

```powershell
& 'C:\src\Github\lodestar-v2\infra\akamai\terraform\scripts\Initialize-ReadOnlyFleetAccess.ps1'
```

This refreshes the observer session interactively, prompts for existing backend
settings and a hidden read-only Linode PAT, checks the token's response-header
scopes, and reads inventory plus selected shared-state metadata. Include
`linodes:read_only`, `firewall:read_only`, `volumes:read_only`, and
`ips:read_only` on the short-lived PAT; account/images read-only scopes may also
be included. It refuses tokens with write scopes. An existing state-read AWS
profile needs S3 `GetObject` and metadata-read access; the observer role is not
assumed to have those permissions, and this script grants none.

An observer role may depend on a separate console-login source profile (here,
`lodestar-observer` uses `lodestar-agent`). If that existing source login is
expired, the script asks for `LOGIN` before invoking `aws login` on that source
profile, then performs observer role MFA separately. Choose the existing source
identity in the browser and decline a prompt to switch identities. The script
checks that `login_session` did not change; it never logs in on the observer
role/default profile, creates a new profile, or changes IAM permissions.

The credential-free resource metadata report is
`infra/akamai/terraform/logs/read-only-fleet-report.json` (git-ignored).
Failed reads remain unavailable, and the report never authorizes provisioning.
The PAT is not saved to an environment variable or file. Backend settings apply
only to the owner's current terminal, not automatically to the agent. No
Terraform initialization, state migration, SSH key creation, PIA read, or cloud
write runs. Tell the agent when the report is ready; do not paste the token.

In an owner-controlled PowerShell terminal:

```powershell
Set-Location C:\src\Github\lodestar-v2\infra\akamai\terraform
git rev-parse HEAD # must be bcf434743c1219192972bf810d86b55b6236afeb
# Set the following from existing private operational configuration, not chat:
# LODESTAR_TF_STATE_BUCKET, LODESTAR_TF_STATE_REGION, AWS_PROFILE,
# CONTROL_BUCKET, and a short-lived LINODE_TOKEN with read-only scopes for inspection.
# The agent's observer profile is not assumed to have S3 state-read permissions.
. ./scripts/StateBackend.ps1
$backend = Get-StateBackendArgs
terraform init -input=false -reconfigure @backend
if ($LASTEXITCODE -ne 0) { throw 'Shared state unavailable: STOP' }
terraform workspace show # expected shared workspace: confirm with the owner
terraform state list
if ($LASTEXITCODE -ne 0) { throw 'State unavailable: STOP; do not create a fleet' }
```

`-reconfigure` reconnects to the existing backend; do not use `-migrate-state`,
`state push`, a new workspace, a different state key, `-lock=false`, or
`force-unlock`. Verify bucket/region/key against the control plane, not merely
against an environment variable. The fixed Terraform key is
`terraform/lodestar.tfstate`.

An authorized state reader must also inspect **privately**, without printing
whole state or manifest documents:

- State bucket: `fleet/fleet.json`, `fleet/spend-ledger.json`.
- Control bucket: `locks/fleet.json`, `locks/fleet-mismatch.json`.
- Record fleet/tombstone status, pinned commit, resource IDs, creation/expiry,
  monthly spend, active lease owner/expiry, and any mismatch flag. Terraform state
  can contain SSH host private keys: never attach it to a PR or run log.
- Read all pages from Linode `GET /v4/linode/instances`,
  `/networking/firewalls`, `/volumes`, and `/networking/ips`; record the before
  set privately. Read firewall device attachments/rules for candidate targets.
  Insufficient scope on any inventory is **unavailable**, never zero resources.
  If the dedicated inventory token needs more read-only scopes, approve only
  those reads; do not substitute a read-write token in an agent session.
- Check read-only account settings, region/type/image availability, quotas,
  and `interfaces_for_new_linodes`. Abort for `linode_only`: this template uses
  `legacy_config` for firewall attachment. Do not change account policy as a workaround.
- Inspect existing fleet workflows/reapers and reserve an owner-maintained
  exclusive maintenance window. No concurrent `up`, `down`, reconciliation,
  firewall automation, or redeploy may act on this target during testing. Keep
  emergency expiry cleanup enabled. A Terraform state lock alone does not fence
  the control plane's application lease.
  Its lease TTL is 90 minutes: it cannot cover a four-hour maintenance window
  without an already supported renewal/ownership procedure. Finish cleanup
  before lease expiry or use that reviewed procedure; if neither is available,
  STOP. Do not invent a lock takeover, extend another owner's lease, or force-unlock.

For paginated inventory in the owner's inspection terminal, use this read-only
pattern. Keep the returned objects private; authentication errors stop the gate:

```powershell
function Get-AllLinodePages([string]$Path) {
    $items = [System.Collections.Generic.List[object]]::new()
    $page = 1
    do {
        $result = Invoke-RestMethod -Method Get -ErrorAction Stop -TimeoutSec 30 `
            -Uri "https://api.linode.com/v4/${Path}?page_size=100&page=$page" `
            -Headers @{Authorization="Bearer $env:LINODE_TOKEN"}
        foreach ($item in $result.data) { $items.Add($item) }
        $page++
    } while ($page -le $result.pages)
    return ,$items.ToArray()
}
$beforeLinodes = Get-AllLinodePages 'linode/instances'
$beforeFirewalls = Get-AllLinodePages 'networking/firewalls'
$beforeVolumes = Get-AllLinodePages 'volumes'
$beforeAddresses = Get-AllLinodePages 'networking/ips'
# Match these private ID sets against the shared state and fleet manifest.
# Record counts/status for the report; do not publish full objects.
```

**Decision gate:**

- If state/manifest/inventory disagree, a read fails, a lease is active, or the
  expiry budget is insufficient: STOP. Obtain a reconciled, owner-approved view.
- If a fleet exists: do not provision or change Terraform desired counts. Select
  an explicitly approved disposable member for reuse; preserve its baseline
  resources and fleet metadata. If none is suitable, defer the test.
- A new one-VM test requires positive idle evidence from **all** authoritative
  sources: readable empty Terraform resource inventory, a readable fleet
  tombstone, and no live Lodestar Linodes or orphaned test infrastructure.
  A missing fleet manifest requires owner reconciliation, not an inferred tombstone.
- Re-check this gate immediately before any approved apply. Do not run a command
  that would recreate watchdog-deleted members of a recorded fleet.

## 2. Prepare exactly one target and verify prerequisites

For an already-approved idle shared state only, proposed non-secret tfvars are:

```hcl
region                 = "us-east"
image                  = "linode/ubuntu24.04"
deployment_id          = "pia632"
deploy_main            = false
worker_count           = 1
max_hourly_compute_usd = 0.0075
ssh_public_key_path    = "<existing-owner-public-key-path>"
ssh_admin_ipv4_cidr    = "<approved-current-owner-ipv4>/32"
```

Human-run `./scripts/Preflight.ps1`, then `./scripts/Deploy.ps1` only after the
resource/charge approval. Expected idle-state plan: **one**
`linode_instance.workers[0]` and **one** `linode_firewall.workers[0]`, no main,
no other changes/replacements/deletions. Review the saved plan privately and
type `DEPLOY 1 LINODES` only for that exact delta. Abort on anything else.
Expected labels are `lodestar-worker-01-pia632` and `lodestar-worker-fw-pia632`.
Record their assigned IDs privately after creation; labels alone are not
sufficient deletion authority.

The automatic `/fleet up screen` route is **not** this one-Nanode procedure:
it creates main+worker, pins an ancestor of `main`, and maintains its own
manifest/ledger/lease. This unmerged PR commit must not be smuggled through that
ancestry gate. Do not hand-edit `fleet.json` or bypass the controller. If the
controller owns the environment, agree an owner-run maintenance path first;
until that is resolved, this deployment remains blocked.

Before booting a billable target, confirm an existing authorized **human root
maintenance console/path** can run the privileged diagnostics below. The
`lodestar` SSH account has only the constrained non-interactive helper grant;
do not assume it can run arbitrary `sudo docker`, `nsenter`, or `iptables`.
If no root maintenance path exists, STOP and agree one explicitly; this plan
does not create or expand privileged access.

On the approved target, the human checks:

- `cloud-init status --wait` completes successfully; inspect bootstrap logs for
  package/kernel errors. Existing fleet VMs must attest to the PR's helper,
  Compose, and CA files; cloud-init changes do not automatically update an
  existing VM. Any in-place update needs a separate reviewed approval.
- Pin the SSH host key from trusted fleet metadata or an authenticated provider
  console. Add only that verified key to the owner's known_hosts. An unverified
  `ssh-keyscan` or `StrictHostKeyChecking=no/accept-new` is not acceptable.
- Root-owned helper mode `0750`, Compose mode `0644`, correct CA, Docker/Compose,
  host `wg`, `nsenter`, `ip`, `iptables`, `curl`, `python3`, `tcpdump`, `dig`,
  `setpriv`, and `timeout`. Diagnostic package installation, if necessary,
  requires the target-change approval. No host module unloading or blacklist changes.
- Helper `preflight` and `validate-compose` succeed. Record kernel version and
  positive `/sys/module/wireguard` evidence after the helper's host load check.
- Confirm the pinned Gluetun digest, `WIREGUARD_IMPLEMENTATION=kernelspace`,
  `cap_drop: ALL`, `cap_add: NET_ADMIN` only, no published ports,
  `no-new-privileges:true`, and IPv6-disabled sysctls. The crawler must be absent.
  Require an otherwise disposable target with no unrelated workload containers;
  do not stop or repurpose another workload to make the test fit.
- Operator Python has `cryptography` and the bridge dependencies. The bridge
  defaults to AWS profile `lodestar-admin`, validates `LODESTAR_AWS_ACCOUNT_ID`,
  then assumes `LodestarPiaSecretReader` in `us-east-1`. The owner refreshes MFA
  and confirms the narrow role/secret prerequisite privately. `verify-aws`
  actually retrieves `lodestar/pia-vpn`; do not treat it as a harmless metadata check.

## 3. Provision and verify the live tunnel (no crawler)

Owner-controlled PowerShell, with the approved account ID and key path set:

```powershell
$target = '<approved-target-ipv4>'
$sshKey = '<existing-owner-private-key-path>'
# The known_hosts pin must already be verified. No key material is logged.
./scripts/Deploy-PiaVpn.ps1 -Target $target -SshKey $sshKey `
    -AwsAccountId $env:LODESTAR_AWS_ACCOUNT_ID -Region us-east -Action Provision
if ($LASTEXITCODE -ne 0) { throw 'Live provision failed: STOP and clean up' }
```

Despite its name, **Provision runs the whole VPN workflow**: preflight, restricted
secret read, PIA authentication/key registration, tmpfs transfer, Gluetun startup,
and verification. It does not merely copy a config. Do not supply `-StartCrawler`.
Allow up to three minutes for bootstrap/tunnel convergence; capture a failure
as a failure before deciding on another attempt. No infinite retry loop.

From the `lodestar` SSH account, the human may run the approved helper commands:

```bash
sudo -n /usr/local/sbin/lodestar-vpn-helper preflight
sudo -n /usr/local/sbin/lodestar-vpn-helper verify-permissions
sudo -n /usr/local/sbin/lodestar-vpn-helper verify-tunnel
sudo -n /usr/local/sbin/lodestar-vpn-helper verify-dns-firewall
sudo -n /usr/local/sbin/lodestar-vpn-helper verify-dns-route
sudo -n /usr/local/sbin/lodestar-vpn-helper query-dns
```

**Positive acceptance:** healthy pinned Gluetun, one configured/observed matching
peer+endpoint, actual kernel latest-handshake age between 0 and 120 seconds,
nonzero RX and TX increasing after fresh probes, `status=protected`, and VPN
public IPv4 different from host public IPv4 with a verified non-Akamai owner.
Config is a regular root-owned `0600` file in a root-only `0700` directory on
`/run` tmpfs. No PIA login, AWS token, or private key appears in evidence.

## 4. Independent data-plane probes and leak checks

Use the existing approved **human root maintenance session**, not a widened
helper grant. The following Bash setup runs on the target. Keep shell tracing
off; evidence contains results and packet counters, never config contents.

```bash
set -Eeuo pipefail
set +x
test "$(id -u)" -eq 0
C=/etc/lodestar/vpn/compose.yaml
H=/usr/local/sbin/lodestar-vpn-helper
assert_no_crawler() {
  test -z "$(docker ps -aq --filter name='^/lodestar_crawler$')"
}
refresh_ns() {
  pid=$(docker inspect -f '{{if .State.Running}}{{.State.Pid}}{{else}}0{{end}}' gluetun)
  test "$pid" -gt 0
  phy=$(nsenter -t "$pid" -n ip -o link show |
    awk -F': ' '{sub(/@.*/, "", $2); if ($2 !~ /^(lo|wg0)$/) print $2}')
  test "$(printf '%s\n' "$phy" | wc -l)" -eq 1
  test -n "$phy"
}
assert_no_crawler
refresh_ns
test "$(nsenter -t "$pid" -n wg show wg0 peers | wc -l)" -eq 1
# Public echo endpoint used by the production gate; pin DNS once on the host.
ECHO_IP=$(getent ahostsv4 checkip.amazonaws.com | awk 'NR==1 {print $1}')
test -n "$ECHO_IP"
host_ip=$(curl --noproxy '*' --fail --silent --show-error --max-time 8 \
  --resolve "checkip.amazonaws.com:443:$ECHO_IP" https://checkip.amazonaws.com)
vpn_probe() {
  refresh_ns
  nsenter -t "$pid" -n setpriv --reuid 65534 --regid 65534 --clear-groups \
    --bounding-set=-all --inh-caps=-all --ambient-caps=-all \
    curl --noproxy '*' --fail --silent --show-error --connect-timeout 2 --max-time 5 \
    --resolve "checkip.amazonaws.com:443:$ECHO_IP" https://checkip.amazonaws.com
}
vpn_ip=$(vpn_probe)
test -n "$vpn_ip" && test "$vpn_ip" != "$host_ip"
nsenter -t "$pid" -n wg show wg0 latest-handshakes |
  awk -v now="$(date +%s)" '{age=now-$2; print "handshake_age_seconds=" age;
    if ($2 <= 0 || age < 0 || age > 120) exit 1}'
nsenter -t "$pid" -n wg show wg0 transfer | awk '{print "rx=" $2, "tx=" $3}'
read -r rx_before tx_before < <(nsenter -t "$pid" -n wg show wg0 transfer | awk '{print $2, $3}')
nsenter -t "$pid" -n ip route get "$ECHO_IP" # must route through wg0
nsenter -t "$pid" -n dig @127.0.0.1 example.com A +tries=1 +time=5
nsenter -t "$pid" -n cat /proc/sys/net/ipv6/conf/all/disable_ipv6 # must be 1
nsenter -t "$pid" -n cat /proc/sys/net/ipv6/conf/default/disable_ipv6 # must be 1
nsenter -t "$pid" -n ip -6 route show default # must be empty
```

Record `iptables -nvxL OUTPUT` and `iptables -S OUTPUT` inside this network
namespace before and after the probes. The helper must accept the entire exact
allowlist, not just selected ports. A public resolver reached **through wg0** is
not a DNS leak; traffic reaching that resolver through the physical interface is.
Do not demand that every `query-dns-vpn` query fail just because it uses 8.8.8.8.

Run a bounded, header-only observation inside Gluetun's network namespace:

```bash
refresh_ns
baseline_pid=$pid
timeout 120s nsenter -t "$pid" -n tcpdump -l -nn -q -i "$phy" -Q out \
  "(host $ECHO_IP and tcp port 443) or port 53 or tcp port 853 or ip6" \
  > /run/pia632-forbidden-packets.txt 2>/run/pia632-capture-status.txt &
capture_pid=$!
sleep 1
kill -0 "$capture_pid" # a failed capture setup must not count as zero leaks
```

During that capture, repeat ten fresh `vpn_probe` requests (each must return the
VPN IP), exercise the internal resolver with unique query names, and perform
the physical-interface adversarial probes below. Verify tcpdump's final status
and **zero packets** matching those public destinations on the physical
interface. Also observe all physical-interface DNS traffic (TCP/UDP 53 and TCP
853) during the internal queries; require zero public DNS/DoT packets outside
the encrypted WireGuard transport. Host management traffic is outside the VPN
namespace and must not be confused with protected application traffic.

For a reproducible physical-interface test, run this root-only host Python probe
in the same capture window. First snapshot the namespace's OUTPUT counters:

```bash
nsenter -t "$pid" -n iptables-save -c > /run/pia632-firewall-before.txt
mark=$(nsenter -t "$pid" -n wg show wg0 fwmark)
nsenter -t "$pid" -n python3 - "$phy" "$ECHO_IP" "$mark" <<'PY'
import ipaddress
import json
import socket
import sys

interface, echo_ip, mark_text = sys.argv[1:]
assert ipaddress.IPv4Address(echo_ip).is_global
try:
    mark = int(mark_text, 0)
    if mark <= 0:
        raise ValueError("no WireGuard outer-packet mark")
except ValueError:
    raise SystemExit("TEST INVALID: cannot select verified physical routing mark")

for address, port, kind in [
    (echo_ip, 443, socket.SOCK_STREAM),
    ("8.8.8.8", 53, socket.SOCK_STREAM),
    ("1.1.1.1", 853, socket.SOCK_STREAM),
    ("8.8.8.8", 53, socket.SOCK_DGRAM),
]:
    with socket.socket(socket.AF_INET, kind) as connection:
        connection.settimeout(2)
        try:
            connection.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE,
                                  interface.encode() + b"\0")
            connection.setsockopt(socket.SOL_SOCKET, socket.SO_MARK, mark)
        except (OSError, AttributeError):
            raise SystemExit("TEST INVALID: physical socket setup failed")
        result = {"port": port, "protocol": "tcp" if kind == socket.SOCK_STREAM else "udp"}
        try:
            if kind == socket.SOCK_STREAM:
                connection.connect((address, port))
                raise SystemExit("FAIL: forbidden physical TCP connection succeeded")
            connection.sendto(b"pia632-blocking-probe", (address, port))
            result["udp_send_returned"] = True
        except OSError as error:
            result["errno"] = error.errno
        print(json.dumps(result))
PY
nsenter -t "$pid" -n iptables-save -c > /run/pia632-firewall-after.txt
```

The UDP payload is a tiny blocking-test marker, not credentials. An unexpected
UDP send return is not automatically PASS or FAIL: the capture and drop-counter
evidence determine whether a packet escaped. Socket/routing setup errors are
INVALID. Evaluate the combined evidence as follows:

1. Discover `wg show wg0 fwmark`, parse the actual nonzero mark (do not guess it),
   and bind the socket with `SO_BINDTODEVICE` to the discovered physical interface.
2. Apply that mark using `SO_MARK`, so routing selects the normal outer/physical
   path rather than failing before OUTPUT because of the WireGuard policy route.
   This changes only test socket attributes; it does not change any firewall,
   route, container capability, or system setting.
3. Try fresh TCP connects to `$ECHO_IP:443`, `8.8.8.8:53`, `1.1.1.1:853`, and
   one UDP send to `8.8.8.8:53`, each with a two-second deadline.
4. Distinguish socket setup failure from network blocking. Permission errors
   while setting either socket option mean **TEST INVALID**, not PASS.
5. Require an increased OUTPUT DROP/REJECT counter consistent with these
   probes, zero forbidden physical packets, and no TCP connection success.
   Timeout or UDP `sendto` return status alone is insufficient evidence.

IPv6 acceptance additionally requires an AF_INET6 TCP connection to a literal
public IPv6 address (for example Cloudflare `2606:4700:4700::1111:443`) to fail
in this namespace, with zero physical IPv6 egress. The physical-interface DNS
and packet observations are mandatory; a helper success token alone is insufficient.

Use the following additional probes and finish the capture window. Every fresh
IPv4 request must return the VPN address; IPv6 must fail after socket creation:

```bash
for sample in $(seq 1 10); do
  observed=$(vpn_probe)
  test "$observed" = "$vpn_ip"
done
refresh_ns
test "$pid" = "$baseline_pid" # capture must refer to the namespace actually probed
read -r rx_after tx_after < <(nsenter -t "$pid" -n wg show wg0 transfer | awk '{print $2, $3}')
test "$rx_after" -gt "$rx_before" && test "$tx_after" -gt "$tx_before"
nsenter -t "$pid" -n dig @127.0.0.1 "pia632-$(date +%s).example.com" A +tries=1 +time=3
nsenter -t "$pid" -n python3 - <<'PY'
import socket
connection = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
connection.settimeout(2)
try:
    connection.connect(("2606:4700:4700::1111", 443))
except OSError as error:
    print("ipv6_connect_blocked", error.errno)
else:
    raise SystemExit("FAIL: IPv6 connection succeeded")
finally:
    connection.close()
PY
kill -0 "$capture_pid" # all probes must finish while the observed capture is alive
capture_rc=0
wait "$capture_pid" || capture_rc=$?
test "$capture_rc" -eq 124 # expected bounded tcpdump timeout, not setup failure
test ! -s /run/pia632-forbidden-packets.txt
grep -q '^0 packets dropped by kernel$' /run/pia632-capture-status.txt
```

Confirm from the before/after snapshots that the default OUTPUT DROP counter or
an exact OUTPUT DROP/REJECT rule increased for these physical probes. Do not
accept a routing error with no firewall-counter evidence as packet-filter proof.

Never run `wg showconf`, `wg ... dump`, `cat wg0.conf`, unrestricted `docker
inspect`, environment dumps, or packet payload captures. Record only peer-match
booleans, handshake age, byte counters, routing, firewall counters, and probe outcomes.

## 5. Bounded tunnel interruption and fail-closed behavior

Approval covers only an **additional host DROP rule** for the already observed
container-IP / PIA endpoint / UDP-port tuple. Do not alter Gluetun rules or
disable its health check. The human root operator reads only `Endpoint` from
the runtime config, confirms it equals `nsenter -n wg show wg0 endpoints`,
discovers the container IPv4 from `ip -4 addr` inside its namespace, and privately
sets `container_ip`, `endpoint_ip`, and `endpoint_port` to those verified values.
Abort if they cannot be validated as IPv4/UDP-port values or match the host IP.

```bash
refresh_ns
observed_endpoint=$(nsenter -t "$pid" -n wg show wg0 endpoints | awk 'NR==1 {print $2}')
configured_endpoint=$(awk -F= '/^[[:space:]]*Endpoint[[:space:]]*=/ {
  gsub(/[[:space:]]/, "", $2); print $2; exit
}' /run/lodestar/vpn/wg0.conf)
test -n "$observed_endpoint" && test "$observed_endpoint" = "$configured_endpoint"
endpoint_ip=${observed_endpoint%:*}
endpoint_port=${observed_endpoint##*:}
container_ip=$(nsenter -t "$pid" -n ip -o -4 addr show dev "$phy" |
  awk '{split($4, parts, "/"); print parts[1]}')
python3 - "$container_ip" "$endpoint_ip" "$endpoint_port" "$host_ip" <<'PY'
import ipaddress
import sys
container, endpoint, port, host = sys.argv[1:]
container_address = ipaddress.IPv4Address(container)
endpoint_address = ipaddress.IPv4Address(endpoint)
assert not container_address.is_loopback and not container_address.is_unspecified
assert endpoint_address.is_global and endpoint != host
assert 1 <= int(port) <= 65535
PY
tag="pia632-outage-$(date +%s)"
rule=(-s "$container_ip/32" -d "$endpoint_ip/32" -p udp --dport "$endpoint_port" \
  -m comment --comment "$tag" -j DROP)
iptables -S DOCKER-USER >/dev/null # abort if the Docker backend has no such chain
restore_test_drop() {
  if iptables -C DOCKER-USER "${rule[@]}" 2>/dev/null; then
    iptables -D DOCKER-USER "${rule[@]}"
  fi
}
trap restore_test_drop EXIT HUP INT TERM
# An independent, transient systemd timer restores the exact rule after 240 s.
# It does not persist configuration. Verify the timer before inserting the rule.
unit="pia632-restore-$(date +%s)"
systemd-run --unit="$unit" --on-active=240s /usr/sbin/iptables -D DOCKER-USER "${rule[@]}"
systemctl is-active --quiet "$unit.timer"
iptables -I DOCKER-USER 1 "${rule[@]}"
```

Require the DROP rule's counters to increase, proving it is hitting actual
WireGuard transport; otherwise the interruption test is invalid. Throughout
the fault, sample fresh, bounded echo probes and physical packet observations.
Every post-interruption application request must fail, without returning either
the host IP or another direct egress address. Never rely solely on the container's
health state; observe actual traffic. If container identity/IP changes, restore
the rule and invalidate that trial rather than probing a stale namespace.

Hold the fault for 150 seconds, leaving margin before automatic rollback.
After the handshake would be older than 120 seconds, `timeout 30s "$H"
verify-tunnel` must return nonzero. Confirm no crawler exists throughout. Do
**not** call `start-crawler` as a negative test: a defect could launch it.

The bounded sample loop is:

```bash
fault_pid=$pid
fault_end=$(( $(date +%s) + 150 ))
fault_capture=/run/pia632-outage-packets.txt
timeout 180s nsenter -t "$pid" -n tcpdump -l -nn -q -i "$phy" -Q out \
  "(host $ECHO_IP and tcp port 443) or port 53 or tcp port 853 or ip6" \
  > "$fault_capture" 2>/run/pia632-outage-capture-status.txt &
fault_capture_pid=$!
sleep 1
kill -0 "$fault_capture_pid"
while test "$(date +%s)" -lt "$fault_end"; do
  refresh_ns
  test "$pid" = "$fault_pid" # stale namespace makes this trial INVALID
  assert_no_crawler
  if observed=$(vpn_probe 2>/dev/null); then
    echo 'FAIL: fresh application request succeeded during the outage' >&2
    exit 1
  fi
  sleep 1
done
if timeout 30s "$H" verify-tunnel >/run/pia632-outage-gate.txt 2>&1; then
  echo 'FAIL: protected gate succeeded during the outage' >&2
  exit 1
else
  gate_rc=$?
  test "$gate_rc" -ne 124 # a timeout is INVALID, not proof of gate rejection
fi
iptables -nvxL DOCKER-USER # the exact added rule must have nonzero packet hits
fault_capture_rc=0
wait "$fault_capture_pid" || fault_capture_rc=$?
test "$fault_capture_rc" -eq 124
test ! -s "$fault_capture"
grep -q '^0 packets dropped by kernel$' /run/pia632-outage-capture-status.txt
```

The capture finishes at 180 seconds, still before the 240-second independent
rollback. If an approved timeout/abort interrupts this sequence, the timer and
shell trap still remove only the additional test DROP rule. Stop and collect
evidence on any unexpected result; do not label a broken probe as safe blocking.

Restore with `restore_test_drop`, verify that the exact rule is gone, stop the
now-unneeded transient timer, and remove the shell trap. Never flush a chain.
The watchdog timer's removal command may report a missing rule if the trap
already restored it; verify actual rule absence rather than ignoring other errors.

```bash
restore_test_drop
if iptables -C DOCKER-USER "${rule[@]}" 2>/dev/null; then
  echo 'FAIL: test DROP rule still exists' >&2
  exit 1
fi
systemctl stop "$unit.timer"
trap - EXIT HUP INT TERM
```

Within three minutes, require a fresh kernel handshake, increasing RX/TX,
healthy Gluetun, successful complete helper gate, and fresh VPN-only echo probes.
Use a bounded gate poll, then repeat the independent positive/leak probes:

```bash
timeout 180s bash -c 'until "$1" verify-tunnel; do sleep 5; done' _ "$H"
observed=$(vpn_probe)
test "$observed" = "$vpn_ip" && test "$observed" != "$host_ip"
```

If any request escapes directly, abort, stop the VPN stack, preserve redacted
evidence, and clean up the approved cloud resource set.

## 6. Restart recovery and optional reboot

Human root session, with no outage rule remaining:

```bash
assert_no_crawler
test -f /run/lodestar/vpn/wg0.conf
docker compose -f "$C" restart gluetun
refresh_ns # never reuse the old PID
timeout 180s bash -c 'until "$1" verify-tunnel; do sleep 5; done' _ "$H"
# Require all independent positive/leak checks again after this gate passes.
"$H" verify-permissions
"$H" verify-tunnel
assert_no_crawler
```

Pass only after the same RAM-backed configuration still exists with correct
permissions, a real fresh handshake has returned, and independent probes/leak
checks pass again. A restart command's zero exit code is not recovery evidence.

If VM reboot was separately approved, record the boot ID, reboot from the
approved console, and reconnect using the same verified host-key pin. Confirm
the boot ID changed and the old runtime config is **not a regular file** after
boot. No working VPN or crawler should appear before fresh provisioning. Record
any unexpected bind-mount directory or restart behavior as a test result; do not
relax the gate to accommodate it. Run the canonical helper cleanup, then run
`Provision` again to negotiate a fresh ephemeral config; repeat every positive
and leak check. Reboot is not required for the container-restart acceptance case.

## 7. Cleanup and independent deletion proof

Always run the approved cleanup, including after a failed test:

```powershell
./scripts/Deploy-PiaVpn.ps1 -Target $target -SshKey $sshKey -Action Cleanup
```

Human root inspection then requires:

- No Gluetun or crawler containers, no test-created Compose network, no remaining
  `pia632-outage-*` rule, no transient rollback timer, and no test probe/capture
  process. Remove only the test's `/run/pia632-*` evidence files after collecting
  approved redacted results.
- `/run/lodestar/vpn` absent; no copied configuration, PIA login, or AWS session
  on the VM. Do not publish container logs or inspect output without redaction.
- A reused pre-existing VM gets only this runtime cleanup: do not destroy its
  instance, firewall, state, or fleet manifest.

For a new, **exclusively test-owned** shared-state deployment, the owner reviews
a destroy plan containing only the approved VM and firewall, reconfirms IDs
against the before-set, and runs `./scripts/Destroy.ps1`, typing
`DESTROY LODESTAR`. If state contains pre-existing resources, **do not run this
whole-state destroy wrapper**. Stop for a scoped, separately reviewed cleanup.

Do not finish at an accepted DELETE request: wait for `GET /linode/instances/<id>`
and `GET /networking/firewalls/<id>` to return 404, successful shared
`terraform state list` to contain none of the test resources, and all-page
inventories to show no test-created volumes, reserved IPs, firewall attachments,
or other chargeable remnants. Compare ID sets, not counts alone. Retain the
existing versioned S3 state/backend, control objects, audit evidence, and all
pre-existing resources. If the control plane owned the fleet record, its owner
must complete the supported `down`/tombstone/ledger flow; do not hand-edit it.

Powered-off instances still bill. The reapers' actual deletion paths were
documented as unverified and do not remove all detached resources: they are a
backup, not the cleanup acceptance test.

## Cost and final acceptance record

Public API and [Akamai CPU pricing](https://www.akamai.com/cloud/pricing/cpu),
checked 2026-11-09: Nanode `g6-nanode-1` $0.0075/hour ($5/month cap);
main `g6-standard-4` $0.072/hour ($48/month cap). Hourly charges round upward
and continue until deletion; see [billing](https://www.akamai.com/cloud/pricing).

- One new Nanode: 4 billed hours **$0.030**, 8 **$0.060**, 24 **$0.180**,
  44 **$0.330**, 48 **$0.360**, compute only.
- Existing one-main/one-worker `screen` fleet: $0.0795/hour; retaining the whole
  fleet for four more hours costs **$0.318** compute, even if only the worker
  is tested. Do not call reuse free. Actual existing burn is unknown until inventory.
- No new AWS VM, backup, volume, or reserved IP is proposed. Verify the firewall
  and any pre-existing service charges in the account before approval.
  [Cloud Firewall itself is free](https://www.akamai.com/products/cloud-firewall).
- Use tiny probes, no crawler or throughput load. Record transfer before/after;
  outbound allowance is pooled and prorated by active hours, not the full
  advertised monthly TB allowance for a four-hour VM. Any overage, existing PIA
  subscription, tax, AWS secret/API/backend requests, and retained existing
  infrastructure are outside these compute totals. See
  [network transfer billing](https://techdocs.akamai.com/cloud-computing/docs/network-transfer-usage-and-costs).

Record PASS/FAIL/INVALID/NOT RUN separately for: source/file attestation,
kernel preflight, handshake/peer binding, RX/TX growth, public egress/ownership,
internal DNS, physical DNS blocking, IPv6 blocking, marked physical-interface
packet blocking with counters/capture, interruption/rollback, recovery,
container restart, optional reboot/re-provision, no crawler, credential cleanup,
and independently confirmed cloud deletion. Include timestamps, source SHA,
test-owned resource ID set (private), command exit codes, and evidence location.
**Overall PASS requires every mandatory case and full approved cleanup to pass.**
NOT RUN or INVALID is never PASS; optional reboot must retain its actual status.
Collect each phase's redacted evidence privately before repeating a phase or
deleting the VM; do not overwrite the only baseline or outage record.
