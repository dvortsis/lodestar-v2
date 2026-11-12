#!/usr/bin/env python3
"""Create, inspect and delete the one interactive Claude test VM on Linode.

docs/decisions/2026-11-12-X-test-vm.md. Separate from the Terraform fleet and
outside the legacy watchdog's scope (its label does not start with `lodestar-`
and it carries no `lodestar`/`test` tags), so nothing else creates or reaps it.

The Linode token is never read here: the session's network proxy adds it to
requests to api.linode.com. Run elsewhere, set LINODE_TOKEN and it is sent as a
Bearer header.

Guard rails, enforced in code:
- only resources whose label starts with PREFIX are ever changed or deleted;
- `create` refuses a label that exists, and more than MAX_VMS PREFIX Linodes;
- only the types in TYPES;
- `delete` needs the exact label typed as --confirm, and verifies a 404.

Usage:
  devvm.py status
  devvm.py create --ssh-pubkey-file KEY.pub --admin-ip A.B.C.D|none --workdir DIR [--role worker --type g6-nanode-1]
  devvm.py set-admin-ip --label claude-dev-YYYYMMDD --admin-ip A.B.C.D
  devvm.py delete --label claude-dev-YYYYMMDD --confirm claude-dev-YYYYMMDD
"""
from __future__ import annotations

import argparse
import base64
import datetime as dt
import ipaddress
import json
import os
import re
import secrets
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

API = "https://api.linode.com/v4"
PREFIX = "claude-dev-"
REGION = "us-east"
TYPE = "g6-dedicated-4"
# Types a test VM may use: the main lane's, and the 1 GiB worker lane's (Lane K).
TYPES = ("g6-dedicated-4", "g6-nanode-1")
MAX_VMS = 3  # a hard ceiling on concurrent claude-dev VMs
# The owner's cap is $50 per cloud for the whole testing period (2026-11-12T16:58Z);
# status warns from here so the coordinator can delete in time.
STOP_USD = 45.0
IMAGE = "linode/ubuntu24.04"
TAGS = ["claude-dev"]
HERE = Path(__file__).resolve().parent


def call(method: str, path: str, body: dict | None = None, ok404: bool = False):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(API + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    tok = os.environ.get("LINODE_TOKEN")
    if tok:
        req.add_header("Authorization", "Bearer " + tok)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            raw = r.read()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        if ok404 and e.code == 404:
            return None
        sys.exit(f"{method} {path}: HTTP {e.code}: {e.read().decode()[:500]}")


def all_pages(path: str) -> list[dict]:
    out, page = [], 1
    while True:
        d = call("GET", f"{path}?page={page}&page_size=500")
        out += d["data"]
        if page >= d["pages"]:
            return out
        page += 1


def ours(items: list[dict]) -> list[dict]:
    return [x for x in items if x["label"].startswith(PREFIX)]


def cmd_status(_a) -> None:
    acct = call("GET", "/account")
    linodes = all_pages("/linode/instances")
    fws = all_pages("/networking/firewalls")
    rates: dict[str, float] = {}
    now = dt.datetime.now(dt.timezone.utc)
    print(f"account {acct['euuid']} uninvoiced ${acct['balance_uninvoiced']:.2f} balance ${acct['balance']:.2f}")
    print(f"linodes on account: {len(linodes)} (ours: {len(ours(linodes))}); firewalls: {len(fws)} (ours: {len(ours(fws))})")
    running = 0.0
    for x in linodes:
        if x["type"] not in rates:
            rates[x["type"]] = call("GET", f"/linode/types/{x['type']}")["price"]["hourly"]
        created = dt.datetime.fromisoformat(x["created"]).replace(tzinfo=dt.timezone.utc)
        hrs = (now - created).total_seconds() / 3600
        est = hrs * rates[x["type"]]
        mine = x["label"].startswith(PREFIX)
        running += est if mine else 0.0
        print(f"  {x['label']} [{'ours' if mine else 'NOT OURS'}] id={x['id']} {x['type']} {x['status']} created={x['created']}Z "
              f"age={hrs:.1f}h est=${est:.2f} ipv4={x['ipv4'][0] if x['ipv4'] else '-'}")
    for f in fws:
        print(f"  firewall {f['label']} id={f['id']} status={f['status']}")
    spent = max(running, float(acct["balance_uninvoiced"]))
    flag = "STOP LINE REACHED: delete the test VMs" if spent >= STOP_USD else "ok"
    print(f"test spend so far (live VMs or uninvoiced, whichever is larger): ${spent:.2f} of the $50 cap, stop line ${STOP_USD:.0f}: {flag}")


def render_user_data(pubkey: str, host_priv: str, host_pub: str) -> str:
    t = (HERE / "cloud-init.yaml.tmpl").read_text()
    indented = "\n".join("    " + ln for ln in host_priv.strip().splitlines())
    t = t.replace("__SSH_PUBKEY__", pubkey.strip())
    t = t.replace("__HOST_KEY_PRIVATE_INDENTED__", indented)
    t = t.replace("__HOST_KEY_PUBLIC__", host_pub.strip())
    assert "__" not in t.split("write_files:")[0], "unfilled placeholder"
    return t


def firewall_rules(admin_ip: str | None) -> dict:
    """Inbound DROP; with an admin address, TCP/22 from that /32 only."""
    if admin_ip is None:
        return {"inbound_policy": "DROP", "outbound_policy": "ACCEPT", "inbound": [], "outbound": []}
    return {
        "inbound_policy": "DROP",
        "outbound_policy": "ACCEPT",
        "inbound": [{
            "label": "ssh-from-owner",
            "action": "ACCEPT",
            "protocol": "TCP",
            "ports": "22",
            "addresses": {"ipv4": [f"{admin_ip}/32"]},
        }],
        "outbound": [],
    }


def check_ip(ip: str) -> str:
    addr = ipaddress.IPv4Address(ip)
    if not addr.is_global:
        sys.exit(f"{ip} is not a public IPv4 address")
    return str(addr)


def cmd_create(a) -> None:
    admin_ip = None if a.admin_ip == "none" else check_ip(a.admin_ip)
    pubkey = Path(a.ssh_pubkey_file).read_text().strip()
    if not pubkey.startswith(("ssh-ed25519 ", "ssh-rsa ", "ecdsa-sha2-")) or "PRIVATE" in pubkey:
        sys.exit("that is not an SSH public key")
    if a.type not in TYPES:
        sys.exit(f"refusing type {a.type}: allowed {TYPES}")
    if a.role and not re.fullmatch(r"[a-z][a-z0-9]{0,15}", a.role):
        sys.exit("role must be lowercase letters and digits")
    label = PREFIX + dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d") + (f"-{a.role}" if a.role else "")
    existing = ours(all_pages("/linode/instances"))
    if any(x["label"] == label for x in existing):
        sys.exit(f"refusing: {label} already exists")
    if len(existing) >= MAX_VMS:
        sys.exit(f"refusing: already {len(existing)} {PREFIX}* Linodes (max {MAX_VMS})")
    work = Path(a.workdir)
    work.mkdir(parents=True, exist_ok=True)
    keyfile = work / "host_ed25519"
    if keyfile.exists():
        keyfile.unlink()
        (work / "host_ed25519.pub").unlink(missing_ok=True)
    subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", label, "-f", str(keyfile)], check=True)
    host_pub = (work / "host_ed25519.pub").read_text()
    fp = subprocess.run(["ssh-keygen", "-lf", str(keyfile) + ".pub"], check=True, capture_output=True, text=True).stdout.strip()
    user_data = render_user_data(pubkey, keyfile.read_text(), host_pub)
    # The private host key now lives only in the user data; drop the local copy.
    keyfile.unlink()

    fw_label = label + "-fw"
    if any(f["label"] == fw_label for f in all_pages("/networking/firewalls")):
        sys.exit(f"refusing: firewall {fw_label} already exists; delete it first")
    fw = call("POST", "/networking/firewalls", {"label": fw_label, "tags": TAGS, "rules": firewall_rules(admin_ip)})
    body = {
        "label": label,
        "region": REGION,
        "type": a.type,
        "image": IMAGE,
        "root_pass": secrets.token_urlsafe(48),  # required by the API; never stored, root login is off
        "authorized_keys": [pubkey],
        "firewall_id": fw["id"],
        "tags": TAGS,
        "backups_enabled": False,
        "booted": True,
        "disk_encryption": "enabled",
        "metadata": {"user_data": base64.b64encode(user_data.encode()).decode()},
    }
    vm = call("POST", "/linode/instances", body)
    print(f"created {label} id={vm['id']} firewall={fw['id']} ipv4={vm['ipv4'][0]}")
    for _ in range(60):
        st = call("GET", f"/linode/instances/{vm['id']}")["status"]
        if st == "running":
            break
        time.sleep(10)
    print(f"status: {st}")
    (work / f"{label}.json").write_text(json.dumps({
        "label": label, "id": vm["id"], "firewall_id": fw["id"], "ipv4": vm["ipv4"][0],
        "ipv6": vm.get("ipv6"), "host_key_fingerprint": fp, "host_key_public": host_pub.strip(),
        "created_utc": vm["created"] + "Z", "type": a.type, "region": REGION,
    }, indent=2))
    print(f"host key fingerprint: {fp}")


def cmd_set_admin_ip(a) -> None:
    if not a.label.startswith(PREFIX):
        sys.exit("label outside our prefix")
    ip = check_ip(a.admin_ip)
    fw = [f for f in all_pages("/networking/firewalls") if f["label"] == a.label + "-fw"]
    if len(fw) != 1:
        sys.exit("firewall not found")
    call("PUT", f"/networking/firewalls/{fw[0]['id']}/rules", firewall_rules(ip))
    print(f"firewall {fw[0]['label']}: SSH now from {ip}/32 only")


def cmd_delete(a) -> None:
    if not a.label.startswith(PREFIX) or a.confirm != a.label:
        sys.exit("refusing: label must start with claude-dev- and --confirm must repeat it exactly")
    vms = [x for x in all_pages("/linode/instances") if x["label"] == a.label]
    for vm in vms:
        call("DELETE", f"/linode/instances/{vm['id']}")
        for _ in range(60):
            if call("GET", f"/linode/instances/{vm['id']}", ok404=True) is None:
                print(f"deleted linode {vm['id']}: verified 404")
                break
            time.sleep(5)
        else:
            sys.exit(f"linode {vm['id']} still present after deletion")
    for f in all_pages("/networking/firewalls"):
        if f["label"] == a.label + "-fw":
            call("DELETE", f"/networking/firewalls/{f['id']}")
            assert call("GET", f"/networking/firewalls/{f['id']}", ok404=True) is None
            print(f"deleted firewall {f['id']}: verified 404")
    left = ours(all_pages("/linode/instances")) + ours(all_pages("/networking/firewalls"))
    print("nothing of ours left" if not left else f"STILL PRESENT: {[x['label'] for x in left]}")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("status")
    c = sub.add_parser("create")
    c.add_argument("--ssh-pubkey-file", required=True)
    c.add_argument("--admin-ip", required=True, help='public IPv4 for SSH, or "none" for no inbound at all')
    c.add_argument("--workdir", required=True, help="outside the repository")
    c.add_argument("--role", default="", help="label suffix, e.g. worker")
    c.add_argument("--type", default=TYPE, choices=TYPES)
    s = sub.add_parser("set-admin-ip")
    s.add_argument("--label", required=True)
    s.add_argument("--admin-ip", required=True)
    d = sub.add_parser("delete")
    d.add_argument("--label", required=True)
    d.add_argument("--confirm", required=True)
    a = p.parse_args()
    {"status": cmd_status, "create": cmd_create, "set-admin-ip": cmd_set_admin_ip, "delete": cmd_delete}[a.cmd](a)


if __name__ == "__main__":
    main()
