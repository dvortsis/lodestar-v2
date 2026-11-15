#!/usr/bin/env bash
# Bootstraps an AWS Ubuntu 24.04 test instance (X-test-vm, the owner's
# 2026-11-15 decision in docs/decisions/2026-11-12-X-test-vm.md) the way
# cloud-init.yaml.tmpl bootstraps a Linode: the same packages, the same
# `lodestar` user and the same devvm-bootstrap.sh, so infra/dev-vm/linux-lane.md
# applies unchanged. It runs as root through SSM Run Command (AWS-RunShellScript).
# There is no SSH on AWS: the security group allows no inbound traffic, so no
# key is installed.
#
#   bash aws-linux-bootstrap.sh <hostname>
#
# It also makes the instance metadata service root-only, so the instance role
# (which can read the PIA secret) is out of reach of the `lodestar` user and of
# every container. Launch with HttpTokens=required and HttpPutResponseHopLimit=1.
set -Eeuo pipefail
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 2; }
name=${1:?usage: aws-linux-bootstrap.sh <hostname>}
[[ $name =~ ^claude-dev-[0-9]{8}(-[a-z0-9]{1,16})?$ ]] || { echo "hostname must be claude-dev-<yyyymmdd>[-<role>]" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
. /etc/os-release
[[ $ID == ubuntu && $VERSION_ID == 24.04 ]] || { echo "expected Ubuntu 24.04" >&2; exit 1; }
export DEBIAN_FRONTEND=noninteractive

# Metadata service: root only. The SSM agent and the AWS CLI run as root.
cat >/etc/systemd/system/lodestar-imds-root-only.service <<'EOF'
[Unit]
Description=Instance metadata reachable by root only (X-test-vm)
Before=network-pre.target docker.service
Wants=network-pre.target
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -c 'iptables -C OUTPUT -d 169.254.169.254 -m owner ! --uid-owner 0 -j REJECT 2>/dev/null || iptables -I OUTPUT -d 169.254.169.254 -m owner ! --uid-owner 0 -j REJECT'
[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now lodestar-imds-root-only.service

hostnamectl set-hostname "$name"
grep -q " $name\$" /etc/hosts || echo "127.0.1.1 $name" >>/etc/hosts

# The template's packages, user and bootstrap script. Its SSH placeholders get
# inert values; the SSH parts of the template are not used here.
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
python3 -I - "$here/cloud-init.yaml.tmpl" "$work" <<'EOF'
import sys, yaml, pathlib
src, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
t = src.read_text()
for k, v in {"__SSH_PUBKEY__": "unused", "__HOST_KEY_PRIVATE_INDENTED__": "    unused",
             "__HOST_KEY_PUBLIC__": "unused", "__SWAP_SIZE__": "8G"}.items():
    t = t.replace(k, v)
doc = yaml.safe_load(t)
(out / "packages").write_text("\n".join(doc["packages"]) + "\n")
[script] = [f["content"] for f in doc["write_files"] if f["path"] == "/usr/local/sbin/devvm-bootstrap.sh"]
(out / "devvm-bootstrap.sh").write_text(script)
[user] = [u for u in doc["users"] if isinstance(u, dict) and u.get("name") == "lodestar"]
(out / "sudo").write_text(user["sudo"] + "\n")
EOF
apt-get update -y
apt-get upgrade -y
xargs -a "$work/packages" apt-get install -y
id lodestar >/dev/null 2>&1 || useradd -m -s /bin/bash -c "Lodestar Test Administrator" -G sudo lodestar
passwd -l lodestar >/dev/null
printf 'lodestar %s' "$(cat "$work/sudo")" >/etc/sudoers.d/90-lodestar
chmod 0440 /etc/sudoers.d/90-lodestar
visudo -cf /etc/sudoers.d/90-lodestar >/dev/null
# The AWS CLI, for pia-login.sh --aws-secret (root only, through the instance role).
command -v aws >/dev/null || snap install aws-cli --classic
install -m 0700 "$work/devvm-bootstrap.sh" /usr/local/sbin/devvm-bootstrap.sh
bash /usr/local/sbin/devvm-bootstrap.sh
cat /var/lib/devvm-bootstrap.status
