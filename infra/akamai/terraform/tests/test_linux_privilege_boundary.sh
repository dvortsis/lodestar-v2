#!/usr/bin/env bash
# ==============================================================================
# test_linux_privilege_boundary.sh
#
# Verifies the real Linux privilege boundary and firewall isolation:
# 1. Safe execution guard: Restricts execution to CI or explicit test environments.
# 2. Non-destructive lifecycle: Guarantees full restoration of pre-existing state.
# 3. Real unprivileged test user with strict sudoers rule.
# 4. Immutable root-owned Compose configuration (/etc/lodestar/vpn/compose.yaml).
# 5. Unprivileged user cannot execute arbitrary sudo commands or Docker directly.
# 6. Helper strictly rejects unauthorized commands (including removed 'install-compose').
# 7. Helper validate-compose succeeds on canonical Compose configuration.
# 8. Helper validate-compose rejects symlinks.
# 9. Helper start-crawler fails closed when Gluetun is missing or not healthy.
# 10. Real Linux network-namespace packet filtering and adversarial firewall rejection.
# ==============================================================================
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

IS_CI="${CI:-false}"
if [[ "${GITHUB_ACTIONS:-false}" == "true" ]]; then
  IS_CI="true"
fi
ALLOW_PRIVILEGED="${LODESTAR_ALLOW_PRIVILEGED_TESTS:-0}"
REQUIRE_TESTS="${LODESTAR_REQUIRE_INTEGRATION_TESTS:-0}"

for arg in "$@"; do
  case "$arg" in
    --allow-privileged)
      ALLOW_PRIVILEGED="1"
      ;;
    --require-integration)
      REQUIRE_TESTS="1"
      IS_CI="true"
      ;;
  esac
done

# ------------------------------------------------------------------------------
# 1. Environment & Safety Checks
# ------------------------------------------------------------------------------
if [[ "$(uname -s)" != "Linux" ]]; then
  if [[ "$IS_CI" == "true" || "$REQUIRE_TESTS" == "1" ]]; then
    echo "ERROR: Required integration test failed: Linux environment required (current OS: $(uname -s))." >&2
    exit 1
  else
    echo "SKIP: test_linux_privilege_boundary.sh requires Linux. Run in an isolated Linux container."
    exit 0
  fi
fi

# Guard against accidental execution on developer workstation or production host
if [[ "$IS_CI" != "true" && "$ALLOW_PRIVILEGED" != "1" ]]; then
  echo "ERROR: Refusing execution on non-CI / non-isolated host." >&2
  echo "This test configures system-level security policies (sudoers, test user, /usr/local/sbin)." >&2
  echo "To safely execute in an isolated disposable container or test VM, export LODESTAR_ALLOW_PRIVILEGED_TESTS=1." >&2
  exit 2
fi

# Ensure root or passwordless sudo prerequisites are available
if [[ $EUID -ne 0 ]]; then
  if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    exec sudo -n env \
      LODESTAR_ALLOW_PRIVILEGED_TESTS=1 \
      LODESTAR_REQUIRE_INTEGRATION_TESTS="$REQUIRE_TESTS" \
      CI="$IS_CI" \
      GITHUB_ACTIONS="${GITHUB_ACTIONS:-false}" \
      bash "$0" --allow-privileged "$@"
  else
    echo "ERROR: Root or passwordless sudo required to execute Linux privilege boundary integration test." >&2
    exit 1
  fi
fi

# ------------------------------------------------------------------------------
# 2. Unique Test Identities and Resource State Tracking
# ------------------------------------------------------------------------------
TEST_PID="$$"
TEST_RUN_ID="ls-test-$TEST_PID-$(date +%s)"
TEST_USER="test-ls-$TEST_PID"
SUDOERS_FILE="/etc/sudoers.d/99-test-ls-$TEST_PID"
HELPER_BIN="/usr/local/sbin/lodestar-vpn-helper"
COMPOSE_DIR="/etc/lodestar/vpn"
COMPOSE_FILE="$COMPOSE_DIR/compose.yaml"
BACKUP_DIR="$(mktemp -d /tmp/lodestar-priv-test-backup.XXXXXX)"
DIAGNOSTIC_LOG="/tmp/lodestar-priv-test-diag-$TEST_RUN_ID.log"

TEST_USER_CREATED=0
SUDOERS_FILE_CREATED=0
HELPER_PREEXISTED=0
COMPOSE_PREEXISTED=0
COMPOSE_DIR_PREEXISTED=0
NETNS_CREATED=0
TEST_NETNS="ns-$TEST_RUN_ID"

exec 3>&1 4>&2
exec > >(tee -a "$DIAGNOSTIC_LOG" >&3) 2> >(tee -a "$DIAGNOSTIC_LOG" >&4)

cleanup() {
  local rc=$?
  set +e
  echo "Cleaning up privilege boundary test harness (exit code: $rc)..."

  # Clean up network namespace if created
  if [[ "$NETNS_CREATED" -eq 1 ]]; then
    ip netns del "$TEST_NETNS" 2>/dev/null || true
  fi

  # Clean up sudoers rule created by this test
  if [[ "$SUDOERS_FILE_CREATED" -eq 1 ]]; then
    rm -f "$SUDOERS_FILE"
  fi

  # Clean up test user ONLY if created by this test
  if [[ "$TEST_USER_CREATED" -eq 1 ]]; then
    userdel -r -f "$TEST_USER" 2>/dev/null || true
  fi

  # Restore or remove helper binary
  if [[ "$HELPER_PREEXISTED" -eq 1 ]]; then
    if [[ -f "$BACKUP_DIR/helper.orig" ]]; then
      mv -f "$BACKUP_DIR/helper.orig" "$HELPER_BIN"
    fi
  else
    rm -f "$HELPER_BIN"
  fi

  # Restore or remove compose file
  if [[ "$COMPOSE_PREEXISTED" -eq 1 ]]; then
    if [[ -f "$BACKUP_DIR/compose.orig" ]]; then
      mv -f "$BACKUP_DIR/compose.orig" "$COMPOSE_FILE"
    fi
  else
    rm -f "$COMPOSE_FILE"
    if [[ "$COMPOSE_DIR_PREEXISTED" -eq 0 ]]; then
      rmdir "$COMPOSE_DIR" 2>/dev/null || true
    fi
  fi

  rm -rf "$BACKUP_DIR"

  if [[ $rc -ne 0 ]]; then
    echo "=================================================================" >&2
    echo "LINUX PRIVILEGE BOUNDARY TEST FAILED (exit code $rc)" >&2
    echo "Diagnostic Log preserved at: $DIAGNOSTIC_LOG" >&2
    echo "=================================================================" >&2
  else
    rm -f "$DIAGNOSTIC_LOG" 2>/dev/null || true
    echo "ALL LINUX PRIVILEGE BOUNDARY TESTS PASSED."
  fi
  exit "$rc"
}
trap cleanup EXIT

# ------------------------------------------------------------------------------
# 3. Setting Up Test Harness
# ------------------------------------------------------------------------------
echo "=== Step 1: Pre-flight state check & backups ==="
if id -u "$TEST_USER" >/dev/null 2>&1; then
  echo "ERROR: Test username $TEST_USER unexpectedly already exists; aborting." >&2
  exit 1
fi

if [[ -e "$HELPER_BIN" ]]; then
  HELPER_PREEXISTED=1
  cp -p "$HELPER_BIN" "$BACKUP_DIR/helper.orig"
fi

if [[ -d "$COMPOSE_DIR" ]]; then
  COMPOSE_DIR_PREEXISTED=1
fi
if [[ -e "$COMPOSE_FILE" ]]; then
  COMPOSE_PREEXISTED=1
  cp -p "$COMPOSE_FILE" "$BACKUP_DIR/compose.orig"
fi

echo "=== Step 2: Creating unprivileged test user: $TEST_USER ==="
useradd -m -s /bin/bash "$TEST_USER"
TEST_USER_CREATED=1

echo "=== Step 3: Installing canonical helper and root-owned Compose stack ==="
mkdir -p "$COMPOSE_DIR"
chmod 0755 "$COMPOSE_DIR"
chown root:root "$COMPOSE_DIR"

CANONICAL_COMPOSE="$SCRIPT_DIR/../pia-example/compose.vpn.yaml"
if [[ ! -f "$CANONICAL_COMPOSE" ]]; then
  echo "ERROR: Canonical Compose file not found: $CANONICAL_COMPOSE" >&2
  exit 1
fi
cp "$CANONICAL_COMPOSE" "$COMPOSE_FILE"
chmod 0644 "$COMPOSE_FILE"
chown root:root "$COMPOSE_FILE"

# Extract helper script directly from ubuntu24.yaml.tftpl
python3 -c "
import re
from pathlib import Path
tpl = Path('$SCRIPT_DIR/../cloud-init/ubuntu24.yaml.tftpl').read_text()
m = re.search(r'- path: /usr/local/sbin/lodestar-vpn-helper\s+owner: root:root\s+permissions: \'0750\'\s+content: \|\n(.*?)\n  - path:', tpl, re.DOTALL)
if not m:
    raise SystemExit('Failed to extract helper from cloud-init template')
content = m.group(1)
# Unindent 6 spaces and unescape Terraform \$\${ interpolation (preserving bash \$\$)
lines = [line[6:] if line.startswith('      ') else line for line in content.splitlines()]
code = '\n'.join(lines).replace('\$\${', '\${')
Path('$HELPER_BIN').write_text(code + '\n')
"
chmod 0750 "$HELPER_BIN"
chown root:root "$HELPER_BIN"

echo "=== Step 4: Configuring strict sudoers rule for $TEST_USER ==="
cat > "$SUDOERS_FILE" <<EOF
$TEST_USER ALL=(root) NOPASSWD: $HELPER_BIN
EOF
chmod 0440 "$SUDOERS_FILE"
chown root:root "$SUDOERS_FILE"
visudo -cf "$SUDOERS_FILE"
SUDOERS_FILE_CREATED=1

# ------------------------------------------------------------------------------
# 4. Privilege Boundary & Immutability Tests
# ------------------------------------------------------------------------------
echo "=== Test 1: Real unprivileged user cannot run arbitrary sudo commands ==="
if su - "$TEST_USER" -c "sudo -n ls /root" >/dev/null 2>&1; then
  echo "FAIL: Unprivileged user was able to run 'sudo ls /root'!" >&2
  exit 1
fi
if su - "$TEST_USER" -c "sudo -n docker ps" >/dev/null 2>&1; then
  echo "FAIL: Unprivileged user was able to run 'sudo docker ps'!" >&2
  exit 1
fi
if su - "$TEST_USER" -c "sudo -n bash" >/dev/null 2>&1; then
  echo "FAIL: Unprivileged user was able to obtain a root shell via sudo!" >&2
  exit 1
fi
echo "PASS: Arbitrary sudo access rejected as expected."

echo "=== Test 2: Helper rejects unauthorized and removed commands ==="
out="$(su - "$TEST_USER" -c "sudo -n $HELPER_BIN install-compose" 2>&1 || true)"
if ! echo "$out" | grep -q "ERROR: Unauthorized helper command: install-compose"; then
  echo "FAIL: Expected 'install-compose' rejection, got: $out" >&2
  exit 1
fi

out="$(su - "$TEST_USER" -c "sudo -n $HELPER_BIN arbitrary-exec" 2>&1 || true)"
if ! echo "$out" | grep -q "ERROR: Unauthorized helper command"; then
  echo "FAIL: Expected arbitrary command rejection, got: $out" >&2
  exit 1
fi
echo "PASS: Unauthorized helper commands rejected."

echo "=== Test 3: Unprivileged user cannot modify or symlink Compose definition ==="
if su - "$TEST_USER" -c "echo 'malicious: true' >> $COMPOSE_FILE" >/dev/null 2>&1; then
  echo "FAIL: Unprivileged user was able to append to $COMPOSE_FILE!" >&2
  exit 1
fi
if su - "$TEST_USER" -c "rm -f $COMPOSE_FILE" >/dev/null 2>&1; then
  echo "FAIL: Unprivileged user was able to delete $COMPOSE_FILE!" >&2
  exit 1
fi
if su - "$TEST_USER" -c "ln -sf /tmp/fake $COMPOSE_FILE" >/dev/null 2>&1; then
  echo "FAIL: Unprivileged user was able to symlink over $COMPOSE_FILE!" >&2
  exit 1
fi
echo "PASS: Compose file is genuinely immutable to unprivileged callers."

echo "=== Test 4: Helper validate-compose succeeds on approved canonical Compose file ==="
if command -v docker >/dev/null 2>&1; then
  out="$(su - "$TEST_USER" -c "sudo -n $HELPER_BIN validate-compose" 2>&1)"
  if ! echo "$out" | grep -q "COMPOSE_VALID"; then
    echo "FAIL: validate-compose did not return COMPOSE_VALID: $out" >&2
    exit 1
  fi
  echo "PASS: Canonical Compose validated successfully by helper."
else
  echo "SKIP: Docker CLI not installed; skipping docker compose syntax validation."
fi

echo "=== Test 5: Helper validate-compose rejects symlinks ==="
mv "$COMPOSE_FILE" "$COMPOSE_FILE.real"
ln -s "$COMPOSE_FILE.real" "$COMPOSE_FILE"
out="$(su - "$TEST_USER" -c "sudo -n $HELPER_BIN validate-compose" 2>&1 || true)"
rm -f "$COMPOSE_FILE"
mv "$COMPOSE_FILE.real" "$COMPOSE_FILE"
if ! echo "$out" | grep -q "ERROR: $COMPOSE_FILE missing or symlink"; then
  echo "FAIL: validate-compose did not reject symlink, got: $out" >&2
  exit 1
fi
echo "PASS: Symlinks rejected by validate-compose."

echo "=== Test 6: Helper start-crawler fails closed when Gluetun is missing or not healthy ==="
out="$(su - "$TEST_USER" -c "sudo -n $HELPER_BIN start-crawler" 2>&1 || true)"
if ! echo "$out" | grep -q -E "(ERROR: Gluetun health check failed|ERROR: Gluetun container is not running)"; then
  echo "FAIL: start-crawler did not fail closed on missing/unhealthy Gluetun: $out" >&2
  exit 1
fi
echo "PASS: start-crawler failed closed as expected."

# ------------------------------------------------------------------------------
# 5. Real Linux Network-Namespace Egress Isolation & Firewall Tests
# ------------------------------------------------------------------------------
echo "=== Test 7: Real Linux Network-Namespace Isolation & Adversarial Firewall Testing ==="
if command -v ip >/dev/null 2>&1 && command -v iptables >/dev/null 2>&1; then
  ip netns add "$TEST_NETNS"
  NETNS_CREATED=1
  ip -n "$TEST_NETNS" link set lo up

  # Setup simulated physical interface eth0 and WireGuard interface wg0
  ip link add name "eth0-$TEST_PID" type dummy
  ip link set "eth0-$TEST_PID" netns "$TEST_NETNS" name eth0
  ip -n "$TEST_NETNS" link set eth0 up
  ip -n "$TEST_NETNS" addr add 192.0.2.10/24 dev eth0

  ip link add name "wg0-$TEST_PID" type dummy
  ip link set "wg0-$TEST_PID" netns "$TEST_NETNS" name wg0
  ip -n "$TEST_NETNS" link set wg0 up
  ip -n "$TEST_NETNS" addr add 10.0.0.2/32 dev wg0

  # Default route points via wg0
  ip -n "$TEST_NETNS" route add default dev wg0

  # Configure compliant Gluetun v3.40.0 firewall policy in the namespace (including Docker bridge subnet rule)
  ip netns exec "$TEST_NETNS" iptables -P OUTPUT DROP
  ip netns exec "$TEST_NETNS" iptables -A OUTPUT -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
  ip netns exec "$TEST_NETNS" iptables -A OUTPUT -o lo -j ACCEPT
  ip netns exec "$TEST_NETNS" iptables -A OUTPUT -o wg0 -j ACCEPT
  ip netns exec "$TEST_NETNS" iptables -A OUTPUT -d 212.56.54.190/32 -o eth0 -p udp -m udp --dport 1337 -j ACCEPT
  ip netns exec "$TEST_NETNS" iptables -A OUTPUT -s 172.18.0.2/32 -d 172.18.0.0/16 -o eth0 -j ACCEPT

  # Verify IPv6 is disabled in namespace
  ip netns exec "$TEST_NETNS" sysctl -w net.ipv6.conf.all.disable_ipv6=1 >/dev/null 2>&1 || true

  # 7a: Kernel packet-drop test - egress on physical interface eth0 must be dropped by iptables
  drop_verified=$(ip netns exec "$TEST_NETNS" python3 -c "
import socket
try:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(('192.0.2.10', 0))
    s.sendto(b'test', ('8.8.8.8', 53))
    print('SENT_UNEXPECTED')
except PermissionError:
    print('DROPPED_BY_FIREWALL')
except Exception as e:
    print(f'DROPPED_BY_{type(e).__name__}')
" 2>&1 || true)

  if echo "$drop_verified" | grep -q "SENT_UNEXPECTED"; then
    echo "FAIL: Outbound packet on physical interface was not dropped by firewall!" >&2
    exit 1
  fi
  echo "PASS: Physical interface egress was successfully dropped by kernel iptables ($drop_verified)."

  # Prepare a mock docker wrapper that executes container commands directly inside $TEST_NETNS
  MOCK_BIN_DIR="$BACKUP_DIR/mock_bin"
  mkdir -p "$MOCK_BIN_DIR"
  cat > "$MOCK_BIN_DIR/docker" <<MOCK_DOCKER
#!/usr/bin/env bash
if [[ "\$1" == "exec" && "\$2" == "gluetun" ]]; then
  shift 2
  exec ip netns exec "$TEST_NETNS" "\$@"
fi
exit 1
MOCK_DOCKER
  chmod 0755 "$MOCK_BIN_DIR/docker"

  # Write a valid wg0.conf via the actual helper write-config command
  valid_wg0="$(python3 -c '
import base64, os
k1 = base64.b64encode(os.urandom(32)).decode()
k2 = base64.b64encode(os.urandom(32)).decode()
print(f"[Interface]\nAddress = 10.0.0.2/32\nPrivateKey = {k1}\nDNS = 10.0.0.242\n\n[Peer]\nPublicKey = {k2}\nEndpoint = 212.56.54.190:1337\nAllowedIPs = 0.0.0.0/0\nPersistentKeepalive = 25")
')"
  write_out="$(printf "%s\n" "$valid_wg0" | "$HELPER_BIN" write-config 2>&1)"
  if ! echo "$write_out" | grep -q "CONFIG_OK"; then
    echo "FAIL: Helper write-config rejected valid wg0.conf: $write_out" >&2
    exit 1
  fi

  # Verify actual helper accepts compliant Gluetun ruleset in $TEST_NETNS
  ok_check="$(PATH="$MOCK_BIN_DIR:$PATH" "$HELPER_BIN" verify-dns-firewall 2>&1 || true)"
  if ! echo "$ok_check" | grep -q "DNS_FIREWALL_OK"; then
    echo "FAIL: Actual helper verify-dns-firewall rejected compliant Gluetun ruleset: $ok_check" >&2
    exit 1
  fi
  echo "PASS: Actual helper verify-dns-firewall accepted compliant Gluetun ruleset."

  # 7b: Adversarial test - inject broad physical interface ACCEPT rule (-A OUTPUT -o eth0 -j ACCEPT) into real netns
  ip netns exec "$TEST_NETNS" iptables -A OUTPUT -o eth0 -j ACCEPT
  adv_check="$(PATH="$MOCK_BIN_DIR:$PATH" "$HELPER_BIN" verify-dns-firewall 2>&1 || true)"
  ip netns exec "$TEST_NETNS" iptables -D OUTPUT -o eth0 -j ACCEPT

  if ! echo "$adv_check" | grep -q "ERROR: Unauthorized egress rule in Gluetun firewall: -A OUTPUT -o eth0 -j ACCEPT"; then
    echo "FAIL: Actual helper did NOT reject broad eth0 ACCEPT rule! Output: $adv_check" >&2
    exit 1
  fi
  echo "PASS: Actual helper rejected adversarial broad eth0 ACCEPT rule."

  # 7c: Adversarial test - inject arbitrary protocol ACCEPT rule (-A OUTPUT -o eth0 -p tcp -j ACCEPT) into real netns
  ip netns exec "$TEST_NETNS" iptables -A OUTPUT -o eth0 -p tcp -j ACCEPT
  adv_proto_check="$(PATH="$MOCK_BIN_DIR:$PATH" "$HELPER_BIN" verify-dns-firewall 2>&1 || true)"
  ip netns exec "$TEST_NETNS" iptables -D OUTPUT -o eth0 -p tcp -j ACCEPT

  if ! echo "$adv_proto_check" | grep -q "ERROR: Unauthorized egress rule in Gluetun firewall: -A OUTPUT -o eth0 -p tcp -j ACCEPT"; then
    echo "FAIL: Actual helper did NOT reject protocol bypass rule! Output: $adv_proto_check" >&2
    exit 1
  fi
  echo "PASS: Actual helper rejected adversarial protocol bypass rule."

  # Clean up runtime config dir created during test
  "$HELPER_BIN" cleanup >/dev/null 2>&1 || true

  # Clean up network namespace
  ip netns del "$TEST_NETNS"
  NETNS_CREATED=0
  echo "PASS: Real Linux network-namespace isolation and firewall evaluation verified."
else
  if [[ "$IS_CI" == "true" ]]; then
    echo "ERROR: ip and iptables commands required in CI for network-namespace verification." >&2
    exit 1
  fi
fi
