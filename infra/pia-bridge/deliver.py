"""deliver-pia: the PIA login to the fleet's Linodes, unattended (OP-2b; plan §4).

The owner's broker, made unattended: LodestarPiaDelivery (GitHub OIDC, the
`pia-delivery` environment, fleet.yml at main) reads two named secrets and
fleet.json, nothing else. The Linodes get no AWS credentials.

    python3 infra/pia-bridge/deliver.py        (the deliver-pia job)

What it does, in order (each step refuses rather than guess):
1. The gates again: a first attempt, the author, edit, PR and stop checks
   through the same parser; `up` delivers to every role, `run renew-vpn <vm>`
   to that one. Dry-run stops here.
2. PIA_SERVERS: a distinct region per role, masked.
3. fleet.json (ExpectedBucketOwner on every S3 call): targets, host keys and
   firewall IDs come only from it, and its pinned commit must be on main.
4. The two secrets, each value masked as soon as it's read.
5. This runner's /32 on the targets' firewalls, labelled job-<run>-<attempt>,
   after sweeping stale job rules; it must sit inside GitHub's Actions ranges.
6. Per target: SSH with the pinned host key, the VM step and its inputs as a
   tar on stdin, remote output discarded; then a second SSH checks that
   /run/lodestar-pia is gone.
7. Always: its own rule closed again.

Nothing private is printed: lines and comments are fixed strings plus role
names. The fleet lock is held around this job by the cloud-fleet jobs before
and after it (this role can't reach the control bucket).
"""

from __future__ import annotations

import io
import ipaddress
import json
import os
import re
import shlex
import subprocess
import sys
import tarfile
import tempfile
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "fleet"))

import audit  # noqa: E402
import limits as limits_mod  # noqa: E402
from ancestry import is_ancestor  # noqa: E402
from command import Refused  # noqa: E402
from gates import Gated, gate  # noqa: E402
from linode import Linode  # noqa: E402
from state import FLEET_KEY, is_tombstone, validate  # noqa: E402
from timefmt import parse_ts as parse_time  # noqa: E402

PIA_SECRET = "lodestar/pia-vpn"
FIREWALL_SECRET = "lodestar/akamai-firewall-token"
STEP = HERE / "vm" / "pia-wireguard.sh"
CA = HERE / "vm" / "pia-ca.rsa.4096.crt"
RUN_DIR = "/run/lodestar-pia"
WG_CONF = "/home/lodestar/lodestar/secrets/wg0.conf"  # pia-example's compose mounts ./secrets/wg0.conf
SSH_USER = "lodestar"  # root login is off (cloud-init); the step runs under sudo -n
REGION_ID = re.compile(r"[a-z0-9_-]{1,64}\Z")
SSH_ATTEMPTS_SECONDS = 120  # the firewall rule can take a little while to apply
SSH_TIMEOUT = 300  # one session: the VM step's three PIA calls are 30 s each
LIVE_ENV = ("AWS_PIA_ROLE_ARN", "STATE_BUCKET", "STATE_BUCKET_OWNER", "FLEET_SSH_KEY", "PIA_SERVERS",
            "GITHUB_TOKEN", "GITHUB_REPOSITORY", "ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN")


class Refusal(Exception):
    """A fixed, printable reason. Never carries a value."""


def bootstrap(run_dir: str = RUN_DIR, out: str = WG_CONF) -> str:
    """The remote side, run as root: unpack stdin into a root-only directory in
    /run, run the VM step, and delete the directory on EXIT, HUP, INT and TERM."""
    return "\n".join([
        "set -eu",
        "umask 077",
        f"d={shlex.quote(run_dir)}",
        'rm -rf -- "$d"',
        'mkdir -m 0700 -- "$d"',
        "trap 'rm -rf -- \"$d\"' EXIT",
        "trap 'exit 129' HUP",
        "trap 'exit 130' INT",
        "trap 'exit 143' TERM",
        'tar -x -C "$d" --no-same-owner -f -',
        f'bash "$d/pia-wireguard.sh" "$d" {shlex.quote(out)}',
    ])


def remote_command(run_dir: str = RUN_DIR, out: str = WG_CONF) -> str:
    return "sudo -n bash -c " + shlex.quote(bootstrap(run_dir, out))


def clean_check_command(run_dir: str = RUN_DIR) -> str:
    return "sudo -n test ! -e " + shlex.quote(run_dir)


def payload(login: dict, region: str) -> bytes:
    """The VM step's inputs as an in-memory tar: nothing touches the runner's disk."""
    files = [("pia-wireguard.sh", STEP.read_bytes(), 0o700), ("pia-ca.crt", CA.read_bytes(), 0o600),
             ("login.json", json.dumps({"username": login["username"], "password": login["password"]}).encode(), 0o600),
             ("region", region.encode(), 0o600)]
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for name, data, mode in files:
            info = tarfile.TarInfo(name)
            info.size, info.mode, info.uid, info.gid, info.mtime = len(data), mode, 0, 0, 0
            tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def mask(value: str, emit: Callable[[str], None]) -> None:
    """GitHub's add-mask, one line at a time (a mask can't span lines), with the
    workflow-command escaping so the masked text is the value itself."""
    for line in str(value).splitlines() or [str(value)]:
        if line.strip():
            emit("::add-mask::" + line.replace("%", "%25").replace("\r", "%0D"))


def targets_for(env: dict, limits: dict) -> list[str] | None:
    """The roles this run delivers to, from the comment through the same gates
    as act. None: nothing to deliver (or silently gated)."""
    cmd = gate(env, limits)  # Gated and Refused propagate
    if cmd.verb == "up":
        return sorted(limits["presets"][cmd.preset]["roles"])
    if cmd.verb == "run" and cmd.step == "renew-vpn":
        return [cmd.vm]
    return None


def pia_servers(raw: str, roles: list[str], all_roles: list[str]) -> dict[str, str]:
    """{"main": "<region id>", "worker": "<region id>"}: one per role, all distinct."""
    try:
        servers = json.loads(raw)
    except ValueError:
        raise Refusal("PIA_SERVERS isn't JSON") from None
    if not isinstance(servers, dict) or set(servers) != set(all_roles):
        raise Refusal("PIA_SERVERS must name exactly one region per role")
    if not all(isinstance(v, str) and REGION_ID.match(v) for v in servers.values()):
        raise Refusal("PIA_SERVERS has a malformed region")
    if len(set(servers.values())) != len(servers):
        raise Refusal("PIA_SERVERS gives two roles the same region")
    return {r: servers[r] for r in roles}


def pia_login(secret_string: str) -> dict:
    try:
        doc = json.loads(secret_string)
    except ValueError:
        raise Refusal("the PIA secret isn't JSON") from None
    if not (isinstance(doc, dict) and all(isinstance(doc.get(k), str) and doc[k] for k in ("username", "password"))):
        raise Refusal("the PIA secret needs a username and a password")
    return {"username": doc["username"], "password": doc["password"]}


def read_fleet(s3, env: dict, limits: dict, roles: list[str], now: datetime, repo: str) -> dict:
    try:
        body = s3.get_object(Bucket=env["STATE_BUCKET"], Key=FLEET_KEY,
                             ExpectedBucketOwner=env["STATE_BUCKET_OWNER"].strip())["Body"].read()
        fleet = json.loads(body)
    except Exception:
        raise Refusal("fleet.json couldn't be read") from None
    if is_tombstone(fleet):
        raise Refusal("no fleet")
    try:
        validate(fleet, limits)
    except ValueError:
        raise Refusal("fleet.json is invalid") from None
    if now >= parse_time(fleet["expires_at"]):
        raise Refusal("the fleet is past its expiry")
    for r in roles:
        slot = fleet["roles"][r]
        if not all(slot.get(k) for k in ("address", "host_key", "fingerprint", "firewall_id")):
            raise Refusal("a target's slot in fleet.json isn't filled")
        if fingerprint(slot["host_key"]) != slot["fingerprint"]:
            raise Refusal("a target's host key doesn't match its recorded fingerprint")
    if not is_ancestor(fleet["commit"], "origin/main", repo):
        raise Refusal("the fleet's pinned commit is not an ancestor of main")
    return fleet


def fingerprint(public_key: str) -> str:
    from apply import fingerprint as fp  # noqa: PLC0415 - one definition (OP-2)
    return fp(public_key)


class Delivery:
    """One run of deliver-pia, with every outside call injected."""

    def __init__(self, env: dict, *, linode: Linode, ssh: Callable, my_ip: Callable[[], str],
                 meta: Callable[[], dict], lookup_run: Callable[[str], dict | None], report: Callable[[str], None],
                 emit: Callable[[str], None], clock: Callable[[], datetime], sleep: Callable[[float], None]):
        self.env, self.linode, self.ssh, self.my_ip, self.meta = env, linode, ssh, my_ip, meta
        self.lookup_run, self.report, self.emit, self.clock, self.sleep = lookup_run, report, emit, clock, sleep
        self.run_id, self.attempt = env["GITHUB_RUN_ID"], env["GITHUB_RUN_ATTEMPT"]

    def _write_rules(self, fids: list[str], change: Callable[[dict], dict]) -> None:
        from apply import writable  # noqa: PLC0415
        for fid in fids:
            self.linode.put_firewall_rules(fid, writable(change(self.linode.firewall_rules(fid))))

    def open_rules(self, fids: list[str]) -> None:
        ranges = audit.actions_ranges(self.meta())
        ip = self.my_ip().strip()
        mask(ip, self.emit)
        try:
            cidr = str(ipaddress.IPv4Network(f"{ip}/32"))
        except ValueError:
            raise Refusal("this runner's address isn't an IPv4 address") from None
        if not audit.in_ranges(cidr, ranges):
            raise Refusal("this runner's address isn't in GitHub's Actions ranges")
        now = self.clock()
        # Read every rule set first (a failed read writes nothing), then sweep and open.
        plans = [(fid, self.linode.firewall_rules(fid)) for fid in fids]
        from apply import writable  # noqa: PLC0415
        for fid, rules in plans:
            swept, _, _ = audit.sweep(rules, self.lookup_run, now)
            self.linode.put_firewall_rules(fid, writable(audit.open_rule(swept, self.run_id, self.attempt, cidr, ranges)))

    def close_rules(self, fids: list[str]) -> bool:
        ok = True
        for fid in fids:
            try:
                self._write_rules([fid], lambda r: audit.close_rule(r, self.run_id, self.attempt))
            except Exception:  # noqa: BLE001 - every firewall gets its try
                ok = False
        return ok

    def to_target(self, role: str, slot: dict, login: dict, region: str, key: str, workdir: Path) -> str:
        kh = workdir / f"known_hosts-{role}"
        kh.write_text(f"{slot['address']} {slot['host_key'].strip()}\n")
        base = ["ssh", "-F", "/dev/null", "-i", key, "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
                "-o", "StrictHostKeyChecking=yes", "-o", f"UserKnownHostsFile={kh}",
                "-o", "GlobalKnownHostsFile=/dev/null", "-o", "HostKeyAlgorithms=ssh-ed25519",
                "-o", "UpdateHostKeys=no", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=15",
                "-o", "ServerAliveCountMax=4", "-o", "LogLevel=QUIET", "-l", SSH_USER, "--", slot["address"]]
        data = payload(login, region)
        deadline = self.clock() + timedelta(seconds=SSH_ATTEMPTS_SECONDS)
        while True:
            rc = self.ssh(base + [remote_command()], data, SSH_TIMEOUT)
            if rc != 255 or self.clock() >= deadline:  # 255: SSH itself (not yet reachable, or the key)
                break
            self.sleep(5)
        if rc == 255:
            return f"{role}: couldn't connect, or its host key didn't match the pin"
        if rc != 0:
            step = f"the VM step failed (exit {rc})" if 0 < rc < 256 else "the VM step didn't finish"
            # Whatever happened, /run must still be clean.
            return f"{role}: {step}; /run " + self._clean(base, role)
        return f"{role}: delivered; /run " + self._clean(base, role)

    def _clean(self, base: list[str], role: str) -> str:
        rc = self.ssh(base + [clean_check_command()], b"", 60)
        return {0: "clean", 1: "NOT clean; escalate"}.get(rc, "unchecked; escalate")

    def run(self, fleet: dict, roles: list[str], servers: dict[str, str], login: dict, key_text: str) -> bool:
        fids = sorted({str(fleet["roles"][r]["firewall_id"]) for r in roles})
        with tempfile.TemporaryDirectory(prefix="pia-") as tmp:
            workdir = Path(tmp)
            os.chmod(workdir, 0o700)
            key = workdir / "id"
            key.write_text(key_text.strip() + "\n")
            os.chmod(key, 0o600)
            lines, ok = [], True
            try:
                self.open_rules(fids)
                for r in roles:
                    line = self.to_target(r, fleet["roles"][r], login, servers[r], str(key), workdir)
                    ok = ok and line.endswith("delivered; /run clean")
                    lines.append(line)
            finally:
                if not self.close_rules(fids):
                    lines.append("its firewall rule couldn't be closed; the next sweep removes it; escalate")
                    ok = False
        for line in lines:
            self.report(f"deliver-pia: {line}")
        return ok


def deliver(env: dict, ctx_report: Callable[[str], None], *, connect=None, emit=print,
            clock=None, sleep=time.sleep, repo: str = ".") -> int:
    """The job's entry point, with the live wiring injectable for tests."""
    limits = limits_mod.load()
    clock = clock or (lambda: datetime.now(timezone.utc))
    if env.get("GITHUB_RUN_ATTEMPT") != "1":
        ctx_report("deliver-pia: refused, a re-run")
        return 0
    try:
        roles = targets_for(env, limits)
    except Gated:
        return 0
    except Refused as r:
        ctx_report(f"deliver-pia: refused: {r.reason}")
        return 0
    if not roles:
        print("deliver-pia: nothing to deliver for this command")
        return 0
    if env.get("FLEET_LIVE") != "true":
        ctx_report("deliver-pia: would deliver the PIA login to " + ", ".join(roles))
        return 0
    missing = [k for k in LIVE_ENV if not (env.get(k) or "").strip()]
    if missing:
        raise SystemExit("deliver-pia: live mode needs " + ", ".join(missing))
    mask(env["FLEET_SSH_KEY"], emit)
    try:
        all_roles = sorted(limits["presets"]["screen"]["roles"])
        servers = pia_servers(env["PIA_SERVERS"], roles, all_roles)
        for v in servers.values():
            mask(v, emit)
        live = (connect or live_wiring)(env, emit)
        fleet = read_fleet(live["s3"], env, limits, roles, clock(), repo)
        login = live["login"]()
        delivery = Delivery(env, linode=live["linode"], ssh=live["ssh"], my_ip=live["my_ip"], meta=live["meta"],
                            lookup_run=live["lookup_run"], report=ctx_report, emit=emit, clock=clock, sleep=sleep)
        ok = delivery.run(fleet, roles, servers, login, env["FLEET_SSH_KEY"])
    except Refusal as r:
        ctx_report(f"deliver-pia: refused: {r}")
        return 1
    return 0 if ok else 1


def run_ssh(argv: list[str], data: bytes, timeout: int) -> int:  # pragma: no cover - live only
    """SSH with stdin from memory and the remote output discarded (PIA's API
    answers with the exit address). A timeout reads as the step not finishing."""
    try:
        return subprocess.run(argv, input=data, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              timeout=timeout, check=False).returncode
    except subprocess.TimeoutExpired:
        return 256


def live_wiring(env: dict, emit) -> dict:  # pragma: no cover - live only
    """LodestarPiaDelivery's session, the two secrets (masked on read), the
    firewall client and the GitHub lookups."""
    import urllib.request

    import apply  # noqa: PLC0415 - needs boto3, installed only when FLEET_LIVE=true
    from linode import urllib_transport  # noqa: PLC0415

    run_id = f"{env['GITHUB_RUN_ID']}-{env['GITHUB_RUN_ATTEMPT']}"
    session, _ = apply.oidc_session(env, run_id, role_env="AWS_PIA_ROLE_ARN")
    sm = session.client("secretsmanager", config=apply.aws_config())

    def secret(name: str) -> str:
        try:
            value = sm.get_secret_value(SecretId=name)["SecretString"]
        except Exception:
            raise Refusal("a delivery secret couldn't be read") from None
        mask(value, emit)
        return value

    def login() -> dict:
        doc = pia_login(secret(PIA_SECRET))
        mask(doc["username"], emit)
        mask(doc["password"], emit)
        return doc

    try:
        token = apply.linode_token(secret(FIREWALL_SECRET))
    except ValueError:
        raise Refusal("the firewall token secret is empty") from None
    mask(token, emit)

    def my_ip() -> str:
        with urllib.request.urlopen("https://checkip.amazonaws.com", timeout=30) as r:
            return r.read(64).decode("ascii", "replace")

    gh, repo = env["GITHUB_TOKEN"], env["GITHUB_REPOSITORY"]
    return {
        "s3": session.client("s3", config=apply.aws_config()),
        "login": login,
        "linode": Linode(urllib_transport(token)),
        "ssh": run_ssh,
        "my_ip": my_ip,
        "meta": lambda: apply.github_json(gh, "/meta") or {},
        "lookup_run": lambda rid: apply.github_json(gh, f"/repos/{repo}/actions/runs/{int(rid)}"),
    }


def main(argv=None, env=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv != ["deliver"]:
        print("usage: deliver.py deliver", file=sys.stderr)
        return 2
    env = dict(os.environ) if env is None else env
    import fleet  # noqa: PLC0415 - the channel and its best-effort report

    ctx = fleet.Ctx(env, None, None, None, None, None, ".")
    try:
        return deliver(env, ctx.report)
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 - a traceback could carry private values
        ctx.report(f"deliver-pia: unexpected error ({type(e).__name__}); escalate")
        return 1


if __name__ == "__main__":
    sys.exit(main())
