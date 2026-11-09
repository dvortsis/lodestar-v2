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
from concurrent.futures import ThreadPoolExecutor
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
BOOT_MARKER = "/var/lib/cloud/instance/boot-finished"
RUN_LOCK = "/run/lodestar-pia.lock"  # holds nothing; one delivery at a time per VM
CLOUD_INIT_WAIT = 480  # jq, wireguard-tools and the secrets directory arrive late in boot
FLOCK_WAIT = 180  # a retry waits this long for an orphaned earlier session, then exits 21
STEP_TIMEOUT = 150  # the VM step: three PIA calls of at most 30 s each, with room
SSH_ATTEMPTS_SECONDS = 120  # the firewall rule can take a little while to apply
SSH_TIMEOUT = FLOCK_WAIT + CLOUD_INIT_WAIT + STEP_TIMEOUT + 60  # one session, all of the above
# After a lost session the orphan holds the VM's lock for at most its cloud-init
# wait and its step; the clean check polls past that.
CLEAN_WAIT_SECONDS = CLOUD_INIT_WAIT + STEP_TIMEOUT + 30
DELIVER_JOB_MINUTES = 30  # fleet.yml's deliver-pia timeout-minutes (a test keeps them in step)
# The job's deadline runs from this program's start (on a monotonic clock).
# Kept back from it: the steps before the program (checkout and pip, 120 s); the
# close (2 firewalls x 3 calls x 30 s, plus a run lookup per other job rule it
# sweeps, 60 s); the report (up to 3 comments x 30 s); and the clean check's
# 10 s floor. The AWS, checkip, /meta and firewall calls before the SSH work
# spend the same budget. A session starts only when a whole one fits, so an
# orphan's remote bound (FLOCK_WAIT + CLOUD_INIT_WAIT + STEP_TIMEOUT, under
# SSH_TIMEOUT) ends before the deadline, and the lock outlives it; every other
# wait is clamped to what's left. GitHub never cancels the job before it closes
# its rule and reports.
RESERVE_SECONDS = 8 * 60
WORK_SECONDS = DELIVER_JOB_MINUTES * 60 - RESERVE_SECONDS
LEASE_NEEDED = timedelta(minutes=DELIVER_JOB_MINUTES + 2)  # the whole job, and a margin
NOT_READY, BUSY_VM, STEP_TIMED_OUT = 20, 21, 124
MID_BLOCK, RESTART_FAILED = 30, 31  # after the step: renewed but left alone mid-block; the restart failed
# Exit codes that mean the login was delivered and /run is still to be checked.
DELIVERED = {0: "delivered", MID_BLOCK: "renewed; takes effect after the block"}
LIVE_ENV = ("AWS_PIA_ROLE_ARN", "STATE_BUCKET", "STATE_BUCKET_OWNER", "FLEET_SSH_KEY", "PIA_SERVERS",
            "GITHUB_TOKEN", "GITHUB_REPOSITORY", "ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN")


class Refusal(Exception):
    """A fixed, printable reason. Never carries a value."""


def bootstrap(run_dir: str = RUN_DIR, out: str = WG_CONF, boot_marker: str = BOOT_MARKER,
              lock: str = RUN_LOCK) -> str:
    """The remote side, run as root.
    1. Take a per-VM lock first, so a retried session waits for an orphaned
       earlier one (in any stage) instead of sharing its directory; exit 21 if
       it can't. The clean check takes the same lock.
    2. Wait for cloud-init to finish (it installs jq and wireguard-tools and
       makes the secrets directory late in boot); exit 20 if it hasn't.
    3. Unpack stdin into a fresh root-only directory in /run, deleted on EXIT
       (HUP, INT and TERM exit, so EXIT runs), and run the VM step under a hard
       timeout (exit 124). A dropped session doesn't signal the remote side
       (no PTY): the step then runs to that bound, and its traps clean up."""
    d, o, q = shlex.quote(run_dir), shlex.quote(out), shlex.quote
    return "\n".join([
        "set -eu",
        "umask 077",
        f"exec 9>{q(lock)}",
        f"flock -w {FLOCK_WAIT} 9 || exit {BUSY_VM}",
        f"if command -v cloud-init >/dev/null 2>&1; then timeout {CLOUD_INIT_WAIT} cloud-init status --wait"
        " >/dev/null 2>&1 || true; fi",
        f"test -e {q(boot_marker)} || exit {NOT_READY}",
        f"command -v jq >/dev/null 2>&1 && command -v wg >/dev/null 2>&1 && command -v curl >/dev/null 2>&1"
        f" || exit {NOT_READY}",
        f'test -d "$(dirname -- {o})" || exit {NOT_READY}',
        f"d={d}",
        'rm -rf -- "$d"',
        'mkdir -m 0700 -- "$d"',
        "trap 'rm -rf -- \"$d\"' EXIT",
        "trap 'exit 129' HUP",
        "trap 'exit 130' INT",
        "trap 'exit 143' TERM",
        'tar -x -C "$d" --no-same-owner -f -',
        f'timeout -s TERM {STEP_TIMEOUT} bash "$d/pia-wireguard.sh" "$d" {o}',
        # OP-3a: a new wg0.conf takes effect only when gluetun restarts, and a
        # gluetun restart needs a Lodestar restart (operations 4.3). Never mid-
        # block: with a collector running, leave the tunnel alone (exit 30);
        # start-block recreates gluetun from the current wg0.conf anyway.
        f"if docker ps --filter 'name=^lodestar-collector' --format x 2>/dev/null | grep -q .; then exit {MID_BLOCK}; fi",
        "if [ \"$(docker inspect -f '{{.State.Running}}' gluetun 2>/dev/null)\" = true ]; then",
        f"  docker restart gluetun >/dev/null 2>&1 || exit {RESTART_FAILED}",
        "  if [ \"$(docker inspect -f '{{.State.Running}}' lodestar 2>/dev/null)\" = true ]; then",
        f"    docker restart lodestar >/dev/null 2>&1 || exit {RESTART_FAILED}",
        "  fi",
        "fi",
    ])


def remote_command(run_dir: str = RUN_DIR, out: str = WG_CONF) -> str:
    return "sudo -n bash -c " + shlex.quote(bootstrap(run_dir, out))


def clean_check_command(run_dir: str = RUN_DIR, lock: str = RUN_LOCK) -> str:
    """Clean means no delivery holds the VM's lock and /run/lodestar-pia is gone.
    An orphan still waiting on cloud-init holds the lock, so it reads as not
    clean yet (exit 1) instead of passing before its directory exists."""
    return f"sudo -n flock -n {shlex.quote(lock)} test ! -e {shlex.quote(run_dir)}"


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
        check_slot(slot)
    if not is_ancestor(fleet["commit"], "origin/main", repo):
        raise Refusal("the fleet's pinned commit is not an ancestor of main")
    return fleet


ED25519_KEY = re.compile(r"ssh-ed25519 (AAAAC3NzaC1lZDI1NTE5AAAAI[A-Za-z0-9+/]{43})\Z")
FIREWALL_ID = re.compile(r"[0-9]{1,12}\Z")


def check_slot(slot: dict) -> None:
    """A target's address, host key and firewall ID, shape-checked before any of
    them reaches known_hosts, an SSH argument or a firewall call."""
    try:
        addr = ipaddress.IPv4Address(str(slot["address"]))
    except ValueError:
        raise Refusal("a target's address in fleet.json isn't an IPv4 address") from None
    if str(addr) != slot["address"] or addr.is_loopback or addr.is_link_local or addr.is_multicast \
            or addr.is_unspecified or addr.is_reserved:
        raise Refusal("a target's address in fleet.json isn't an IPv4 address")
    if not isinstance(slot["host_key"], str) or not ED25519_KEY.match(slot["host_key"]):
        raise Refusal("a target's host key in fleet.json isn't one ed25519 key")
    if not FIREWALL_ID.match(str(slot["firewall_id"])):
        raise Refusal("a target's firewall ID in fleet.json is malformed")
    if fingerprint(slot["host_key"]) != slot["fingerprint"]:
        raise Refusal("a target's host key doesn't match its recorded fingerprint")


def fingerprint(public_key: str) -> str:
    from apply import fingerprint as fp  # noqa: PLC0415 - one definition (OP-2)
    try:
        return fp(public_key)
    except Exception:  # noqa: BLE001
        raise Refusal("a target's host key in fleet.json isn't one ed25519 key") from None


def read_secrets(sm, emit: Callable[[str], None]) -> tuple[dict, str]:
    """The PIA login and the firewall token, each value masked as soon as it's
    read and before any other use: the raw strings, the username, the password
    and the token."""
    def secret(name: str) -> str:
        try:
            value = sm.get_secret_value(SecretId=name)["SecretString"]
        except Exception:
            raise Refusal("a delivery secret couldn't be read") from None
        mask(value, emit)
        return value

    login = pia_login(secret(PIA_SECRET))
    mask(login["username"], emit)
    mask(login["password"], emit)
    from apply import linode_token  # noqa: PLC0415
    try:
        token = linode_token(secret(FIREWALL_SECRET))
    except ValueError:
        raise Refusal("the firewall token secret is empty or malformed") from None
    mask(token, emit)
    return login, token


def lease_left(env: dict, now: datetime) -> timedelta:
    """How long the fleet lock pia-fence checked has left (its expires_at, a
    job output; nothing private). Unknown or unparsable reads as none."""
    try:
        return parse_time(env.get("LOCK_EXPIRES_AT", "")) - now
    except Exception:  # noqa: BLE001
        return timedelta(0)


class Delivery:
    """One run of deliver-pia, with every outside call injected."""

    def __init__(self, env: dict, *, linode: Linode, ssh: Callable, my_ip: Callable[[], str],
                 meta: Callable[[], dict], lookup_run: Callable[[str], dict | None], report: Callable[[str], None],
                 emit: Callable[[str], None], clock: Callable[[], datetime], sleep: Callable[[float], None],
                 monotonic: Callable[[], float] = time.monotonic, deadline: float | None = None):
        self.env, self.linode, self.ssh, self.my_ip, self.meta = env, linode, ssh, my_ip, meta
        self.lookup_run, self.report, self.emit, self.clock, self.sleep = lookup_run, report, emit, clock, sleep
        self.run_id, self.attempt = env["GITHUB_RUN_ID"], env["GITHUB_RUN_ATTEMPT"]
        self.monotonic = monotonic
        self.deadline = deadline if deadline is not None else monotonic() + WORK_SECONDS  # the SSH work ends by here

    def _left(self) -> float:
        return self.deadline - self.monotonic()

    def open_rules(self, fids: list[str]) -> None:
        from apply import writable  # noqa: PLC0415
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
        for fid, rules in plans:
            swept, _, _ = audit.sweep(rules, self.lookup_run, now)
            self.linode.put_firewall_rules(fid, writable(audit.open_rule(swept, self.run_id, self.attempt, cidr, ranges)))

    def close_rules(self, fids: list[str]) -> bool:
        """Its own rule out, and stale job rules swept again (plan §3: each job
        sweeps at its start and in its cleanup). Every firewall gets its try."""
        from apply import writable  # noqa: PLC0415
        ok, now = True, self.clock()
        for fid in fids:
            try:
                rules = audit.close_rule(self.linode.firewall_rules(fid), self.run_id, self.attempt)
                swept, _, _ = audit.sweep(rules, self.lookup_run, now)
                self.linode.put_firewall_rules(fid, writable(swept))
            except Exception:  # noqa: BLE001
                ok = False
        return ok

    def _ssh_base(self, role: str, slot: dict, key: str, workdir: Path) -> list[str]:
        kh = workdir / f"known_hosts-{role}"
        kh.write_text(f"{slot['address']} {slot['host_key']}\n")
        return ["ssh", "-F", "/dev/null", "-i", key, "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
                "-o", "StrictHostKeyChecking=yes", "-o", f"UserKnownHostsFile={kh}",
                "-o", "GlobalKnownHostsFile=/dev/null", "-o", "HostKeyAlgorithms=ssh-ed25519",
                "-o", "UpdateHostKeys=no", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=15",
                "-o", "ServerAliveCountMax=4", "-o", "LogLevel=QUIET", "-l", SSH_USER, "--", slot["address"]]

    def to_target(self, role: str, slot: dict, login: dict, region: str, key: str, workdir: Path) -> str:
        """One target's report line. An unexpected error ends only that target's
        delivery, so the other target's line is still reported."""
        try:
            return self._to_target(role, slot, login, region, key, workdir)
        except Exception as e:  # noqa: BLE001 - the type only; a message could carry a value
            return f"{role}: unexpected error ({type(e).__name__}); escalate"

    def _to_target(self, role: str, slot: dict, login: dict, region: str, key: str, workdir: Path) -> str:
        base = self._ssh_base(role, slot, key, workdir)
        data = payload(login, region)
        retry_until = min(self.monotonic() + SSH_ATTEMPTS_SECONDS, self.deadline)
        lost_any, rc = False, None
        while self._left() >= SSH_TIMEOUT:  # a whole session fits, so its orphan's bound does too
            rc = self.ssh(base + [remote_command()], data, SSH_TIMEOUT)
            lost_any = lost_any or rc in (255, 256)
            if rc != 255 or self.monotonic() >= retry_until:  # 255: SSH itself (not yet reachable, or the key)
                break
            self.sleep(5)
        if rc is None:
            return f"{role}: out of time before it could connect; /run " + self._clean(base, wait=False)
        if rc in DELIVERED:
            return f"{role}: {DELIVERED[rc]}; /run " + self._clean(base, wait=False)
        # A session (not the step) failed somewhere: an orphan may still be running. A
        # busy VM after a lost attempt is usually that orphan, holding the lock.
        lost = lost_any or rc in (255, 256, BUSY_VM)
        what = {255: "couldn't connect, or its host key didn't match the pin",
                256: "the session didn't finish in time",
                NOT_READY: "the VM isn't ready (cloud-init hasn't finished)",
                BUSY_VM: "another delivery is still running there",
                STEP_TIMED_OUT: "the VM step timed out",
                RESTART_FAILED: "delivered, but restarting the tunnel failed; escalate"}.get(
                    rc, f"the VM step failed (exit {rc})")
        return f"{role}: {what}; /run " + self._clean(base, wait=lost)

    def _clean(self, base: list[str], wait: bool) -> str:
        """/run/lodestar-pia must be gone. After a lost session, an orphaned step
        may still be running toward its own bound, so poll until that's past."""
        until = min(self.monotonic() + (CLEAN_WAIT_SECONDS if wait else 0), self.deadline)
        while True:
            # Always one check, however late; each is clamped to what's left (at least 10 s).
            rc = self.ssh(base + [clean_check_command()], b"", int(max(10, min(60, self._left()))))
            if rc == 0 or self.monotonic() + 10 >= until:
                break
            self.sleep(10)
        return {0: "clean", 1: "NOT clean; escalate"}.get(rc, "unchecked; escalate")

    def run(self, fleet: dict, roles: list[str], servers: dict[str, str], login: dict, key_text: str) -> bool:
        fids = sorted({str(fleet["roles"][r]["firewall_id"]) for r in roles})
        lines: list[str] = []
        ok = False
        try:
            with tempfile.TemporaryDirectory(prefix="pia-") as tmp:
                workdir = Path(tmp)
                os.chmod(workdir, 0o700)
                key = workdir / "id"
                key.write_text(key_text.strip() + "\n")
                os.chmod(key, 0o600)
                try:
                    self.open_rules(fids)
                    args = [(r, fleet["roles"][r], login, servers[r], str(key), workdir) for r in roles]
                    if len(args) == 1:
                        results = [self.to_target(*args[0])]
                    else:  # the targets boot together, so deliver to them together
                        with ThreadPoolExecutor(max_workers=len(args)) as pool:
                            results = list(pool.map(lambda a: self.to_target(*a), args))
                    lines += results
                    ok = all(any(line.endswith(f": {word}; /run clean") for word in DELIVERED.values())
                             for line in results)
                finally:
                    if not self.close_rules(fids):
                        lines.append("its firewall rule couldn't be closed; the next sweep removes it; escalate")
                        ok = False
        finally:
            for line in lines:  # reported even when an exception is on its way out
                self.report(f"deliver-pia: {line}")
        return ok


def deliver(env: dict, ctx_report: Callable[[str], None], *, connect=None, emit=print,
            clock=None, sleep=time.sleep, monotonic: Callable[[], float] = time.monotonic, repo: str = ".") -> int:
    """The job's entry point, with the live wiring injectable for tests."""
    limits = limits_mod.load()
    clock = clock or (lambda: datetime.now(timezone.utc))
    deadline = monotonic() + WORK_SECONDS  # from the program's start: everything below spends it
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
        # The lock pia-fence checked must outlast this job's worst case: queue time
        # before this job, or a wait on the environment, comes out of it.
        if lease_left(env, clock()) < LEASE_NEEDED:
            raise Refusal("the fleet lock's lease is too short for a delivery; nothing delivered")
        live = (connect or live_wiring)(env, emit)
        fleet = read_fleet(live["s3"], env, limits, roles, clock(), repo)
        login, token = read_secrets(live["sm"], emit)
        delivery = Delivery(env, linode=live["linode"](token), ssh=live["ssh"], my_ip=live["my_ip"],
                            meta=live["meta"], lookup_run=live["lookup_run"], report=ctx_report, emit=emit,
                            clock=clock, sleep=sleep, monotonic=monotonic, deadline=deadline)
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
    """LodestarPiaDelivery's session and clients, and the GitHub lookups. The
    secrets themselves are read (and masked) by read_secrets."""
    import urllib.request

    import apply  # noqa: PLC0415 - needs boto3, installed only when FLEET_LIVE=true
    from linode import urllib_transport  # noqa: PLC0415

    run_id = f"{env['GITHUB_RUN_ID']}-{env['GITHUB_RUN_ATTEMPT']}"
    session, _ = apply.oidc_session(env, run_id, role_env="AWS_PIA_ROLE_ARN")

    def my_ip() -> str:
        with urllib.request.urlopen("https://checkip.amazonaws.com", timeout=30) as r:
            return r.read(64).decode("ascii", "replace")

    gh, repo = env["GITHUB_TOKEN"], env["GITHUB_REPOSITORY"]
    return {
        "s3": session.client("s3", config=apply.aws_config()),
        "sm": session.client("secretsmanager", config=apply.aws_config()),
        "linode": lambda token: Linode(urllib_transport(token)),
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
