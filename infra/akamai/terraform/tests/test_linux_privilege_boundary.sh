#!/usr/bin/env bash
# ==============================================================================
# test_linux_privilege_boundary.sh
#
# Verifies the real Linux privilege boundary on Ubuntu / Debian runners:
# 1. Real unprivileged test user ('test-lodestar') with strict sudoers rule.
# 2. Immutable root-owned Compose configuration (/etc/lodestar/vpn/compose.yaml).
# 3. Unprivileged user cannot execute arbitrary sudo commands or Docker directly.
# 4. Helper strictly rejects unauthorized commands (including removed 'install-compose').
# 5. Helper validate-compose succeeds on canonical Compose configuration.
# 6. Helper validate-compose rejects symlinks.
# 7. Helper start-crawler fails closed when Gluetun is missing or not healthy.
# ==============================================================================
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

# Skip gracefully if not running in Linux environment with root or sudo
if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP: test_linux_privilege_boundary.sh requires a Linux environment (current OS: $(uname -s))."
  exit 0
fi

if [[ $EUID -ne 0 ]]; then
  if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    exec sudo -n "$0" "$@"
  else
    echo "SKIP: Root or passwordless sudo required to configure and test Linux privilege boundary."
    exit 0
  fi
fi

TEST_USER="test-lodestar"
SUDOERS_FILE="/etc/sudoers.d/99-test-lodestar-automation"
HELPER_BIN="/usr/local/sbin/lodestar-vpn-helper"
COMPOSE_DIR="/etc/lodestar/vpn"
COMPOSE_FILE="$COMPOSE_DIR/compose.yaml"
BACKUP_DIR="$(mktemp -d /tmp/lodestar-test-backup.XXXXXX)"

cleanup() {
  local rc=$?
  echo "Cleaning up privilege boundary test harness..."
  rm -f "$SUDOERS_FILE"
  if [[ -f "$BACKUP_DIR/helper.bak" ]]; then
    mv "$BACKUP_DIR/helper.bak" "$HELPER_BIN"
  else
    rm -f "$HELPER_BIN"
  fi
  if [[ -f "$BACKUP_DIR/compose.bak" ]]; then
    mv "$BACKUP_DIR/compose.bak" "$COMPOSE_FILE"
  else
    rm -rf "$COMPOSE_DIR"
  fi
  if id -u "$TEST_USER" >/dev/null 2>&1; then
    userdel -r -f "$TEST_USER" 2>/dev/null || true
  fi
  rm -rf "$BACKUP_DIR"
  if [[ $rc -eq 0 ]]; then
    echo "ALL LINUX PRIVILEGE BOUNDARY TESTS PASSED."
  else
    echo "LINUX PRIVILEGE BOUNDARY TESTS FAILED with exit code $rc."
  fi
  exit "$rc"
}
trap cleanup EXIT

echo "=== Step 1: Setting up unprivileged test user: $TEST_USER ==="
if ! id -u "$TEST_USER" >/dev/null 2>&1; then
  useradd -m -s /bin/bash "$TEST_USER"
fi

echo "=== Step 2: Backing up existing helper/compose if present ==="
if [[ -f "$HELPER_BIN" ]]; then
  cp -p "$HELPER_BIN" "$BACKUP_DIR/helper.bak"
fi
if [[ -f "$COMPOSE_FILE" ]]; then
  cp -p "$COMPOSE_FILE" "$BACKUP_DIR/compose.bak"
fi

echo "=== Step 3: Installing canonical helper and root-owned Compose stack ==="
# Extract helper script from cloud-init template or install canonical version
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
# Unindent 6 spaces and unescape \$\$
lines = [line[6:] if line.startswith('      ') else line for line in content.splitlines()]
code = '\n'.join(lines).replace('\$\$', '$')
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
