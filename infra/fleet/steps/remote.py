"""Running a committed step on a fleet VM over SSH (OP-3; plan §3, `run <step> <vm>`).

The control side of every step:
- the target's address and host key come only from fleet.json, shape-checked;
- the step script and its validated arguments travel as an in-memory tar on
  SSH stdin, at the fleet's pinned commit (this checkout is main; the step
  files are read from the pinned commit's tree, never from a comment);
- on the VM a fixed bootstrap takes the per-VM lock that PIA delivery uses
  (so a step and a delivery never overlap), unpacks into a fresh root-only
  directory in /run, deletes it on exit, and runs the step under `timeout`;
- only output lines in a strict grammar come back; everything else a step
  prints is dropped here, so no address or name can reach a log or comment.
"""

from __future__ import annotations

import io
import ipaddress
import re
import shlex
import subprocess
import tarfile
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent
VM_DIR = HERE / "vm"
RUN_DIR = "/run/lodestar-step"
VM_LOCK = "/run/lodestar-pia.lock"  # shared with deliver-pia: one remote action per VM at a time
FLOCK_WAIT = 60
SSH_USER = "lodestar"
BUSY_VM = 21

# The only lines a step's output may contribute. Names are fixed vocabulary in
# the step scripts; counts are bare integers. Nothing else survives.
LINE = re.compile(r"(?:(?:ok|fail|skip) [a-z][a-z0-9_]{0,40}|count [a-z][a-z0-9_]{0,40} [0-9]{1,12})\Z")
MAX_OUTPUT = 64 * 1024
MAX_LINES = 200

ED25519_KEY = re.compile(r"ssh-ed25519 (AAAAC3NzaC1lZDI1NTE5AAAAI[A-Za-z0-9+/]{43})\Z")
ARG_VALUE = re.compile(r"[a-z0-9_-]{1,64}\Z|[0-9a-f]{40}\Z")


class StepRefused(Exception):
    """A fixed, printable reason. Never carries a value."""


def check_target(slot: dict, fingerprint: Callable[[str], str]) -> None:
    """The same shape checks deliver-pia makes, before anything reaches known_hosts or argv."""
    try:
        addr = ipaddress.IPv4Address(str(slot.get("address")))
    except ValueError:
        raise StepRefused("the target's address in fleet.json isn't an IPv4 address") from None
    if str(addr) != slot["address"] or addr.is_loopback or addr.is_link_local or addr.is_multicast \
            or addr.is_unspecified or addr.is_reserved:
        raise StepRefused("the target's address in fleet.json isn't an IPv4 address")
    key = slot.get("host_key")
    if not isinstance(key, str) or not ED25519_KEY.match(key):
        raise StepRefused("the target's host key in fleet.json isn't one ed25519 key")
    try:
        ok = fingerprint(key) == slot.get("fingerprint")
    except Exception:  # noqa: BLE001
        ok = False
    if not ok:
        raise StepRefused("the target's host key doesn't match its recorded fingerprint")


def bootstrap(timeout: int, run_dir: str = RUN_DIR, lock: str = VM_LOCK) -> str:
    """The remote side, run as root: the per-VM lock, a fresh root-only work
    directory deleted on exit (HUP, INT and TERM exit, so EXIT runs), and the
    step under a hard timeout. A busy VM answers with a grammar line."""
    d, q = shlex.quote(run_dir), shlex.quote
    return "\n".join([
        "set -eu",
        "umask 077",
        f"exec 9>{q(lock)}",
        f"flock -w {FLOCK_WAIT} 9 || {{ echo 'fail vm_busy'; exit {BUSY_VM}; }}",
        f"d={d}",
        'rm -rf -- "$d"',
        'mkdir -m 0700 -- "$d"',
        "trap 'rm -rf -- \"$d\"' EXIT",
        "trap 'exit 129' HUP",
        "trap 'exit 130' INT",
        "trap 'exit 143' TERM",
        'tar -x -C "$d" --no-same-owner -f -',
        f'timeout -s TERM {int(timeout)} bash "$d/step.sh" "$d"',
    ])


def remote_command(timeout: int, run_dir: str = RUN_DIR, lock: str = VM_LOCK) -> str:
    return "sudo -n bash -c " + shlex.quote(bootstrap(timeout, run_dir, lock))


def payload(files: dict[str, bytes], args: dict[str, str]) -> bytes:
    """The step's files and its arguments as an in-memory tar. Every argument is
    a key from the step's own list and a value in the narrow ARG_VALUE shape."""
    for k, v in args.items():
        if not re.fullmatch(r"[A-Z][A-Z0-9_]{0,31}", k) or not ARG_VALUE.match(v):
            raise StepRefused("a step argument is malformed")
    entries = dict(files)
    entries["args"] = "".join(f"{k}={v}\n" for k, v in sorted(args.items())).encode()
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for name, data in sorted(entries.items()):
            info = tarfile.TarInfo(name)
            info.size, info.uid, info.gid, info.mtime = len(data), 0, 0, 0
            info.mode = 0o700 if name.endswith(".sh") else 0o600
            tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def parse(stdout: bytes) -> list[str]:
    """The grammar lines of a step's output, in order. Everything else is dropped."""
    text = stdout[:MAX_OUTPUT].decode("ascii", "replace")
    return [ln for ln in text.splitlines() if LINE.match(ln)][:MAX_LINES]


def ssh_argv(address: str, key: str, known_hosts: Path) -> list[str]:
    return ["ssh", "-F", "/dev/null", "-i", key, "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=yes", "-o", f"UserKnownHostsFile={known_hosts}",
            "-o", "GlobalKnownHostsFile=/dev/null", "-o", "HostKeyAlgorithms=ssh-ed25519",
            "-o", "UpdateHostKeys=no", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=15",
            "-o", "ServerAliveCountMax=4", "-o", "LogLevel=QUIET", "-l", SSH_USER, "--", address]


def run_ssh(argv: list[str], data: bytes, timeout: int) -> tuple[int, bytes]:  # pragma: no cover - live only
    """stdin from memory; stdout captured (and parsed by the grammar); stderr
    discarded. A timeout reads as 256 with no output."""
    try:
        r = subprocess.run(argv, input=data, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                           timeout=timeout, check=False)
        return r.returncode, r.stdout
    except subprocess.TimeoutExpired:
        return 256, b""
