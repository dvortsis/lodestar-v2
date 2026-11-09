"""`run <step> <vm>` in live mode (OP-3a): one committed step on one fleet VM.

Called by fleet.py's act, under the fleet lock, after the gates and the
ancestor checks. In order:
1. The target from fleet.json, shape-checked; the step's files read from the
   fleet's pinned commit (never this checkout, never a comment).
2. This job's /32 opened on the fleet firewalls (OP-2's set_job_rules, fenced),
   only for an IPv4 address inside GitHub's Actions ranges.
3. One SSH session: pinned known_hosts, the step and its arguments on stdin,
   stdout parsed by remote.LINE and everything else dropped. Exit 255 (SSH
   itself: the new rule not applied yet) is retried for up to
   SSH_ATTEMPTS_SECONDS, as deliver-pia does; a 255 ran nothing remotely.
4. Always: the rule closed again.
The result is one report line of fixed words and grammar names.
"""

from __future__ import annotations

import ipaddress
import os
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Callable

from linode import LinodeError
from lock import LockLost
from steps import remote

STEPS_PATH = "infra/fleet/steps/vm"
# The steps built so far (OP-3a). The rest of limits.yaml's allow-list refuses
# in live mode until OP-3b adds them.
BUILT = {
    "vm-status": {"files": ("vm-status.sh",), "timeout": 60},
    "leak-test": {"files": ("leak-test.sh", "compose.yaml"), "timeout": 600},
}
SSH_ATTEMPTS_SECONDS = 120  # the firewall rule can take a little while to apply (deliver-pia's)
SSH_RETRY_PAUSE = 5
OUTCOMES = {255: "couldn't connect, or its host key didn't match the pin",
            256: "the session didn't finish in time",
            remote.BUSY_VM: "the VM is busy (a delivery or another step holds its lock)",
            124: "the step timed out"}


def step_files(step: str, commit: str, repo: str, run=subprocess.run) -> dict[str, bytes]:
    """The step's files at the fleet's pinned commit (already checked to be an
    ancestor of main). A fleet pinned before the step existed can't run it."""
    files = {}
    for name in BUILT[step]["files"]:
        r = run(["git", "-C", repo, "show", f"{commit}:{STEPS_PATH}/{name}"], capture_output=True, timeout=30)
        if r.returncode != 0:
            raise remote.StepRefused("the step isn't in the fleet's pinned commit")
        files["step.sh" if name.endswith(".sh") else name] = r.stdout
    return files


def summary(rc: int, lines: list[str]) -> str:
    """One line from the exit code and the grammar lines: fixed words only."""
    if rc in OUTCOMES:
        head = OUTCOMES[rc]
    elif rc == 0:
        head = "done"
    else:
        head = f"failed (exit {rc})"
    oks = sum(1 for ln in lines if ln.startswith("ok "))
    fails = [ln.split(" ", 1)[1] for ln in lines if ln.startswith("fail ")]
    counts = [" ".join(ln.split(" ")[1:]) for ln in lines if ln.startswith("count ")]
    parts = [head, f"{oks} ok"]
    if fails:
        parts.append("failed: " + ", ".join(fails))
    if counts:
        parts.append(", ".join(counts))
    return "; ".join(parts)


def run_step(env: dict, backend, fleet: dict, step: str, vm: str, run_id: str, fence: Callable[[], None], *,
             ssh: Callable, my_ip: Callable[[], str], emit: Callable[[str], None], repo: str,
             fingerprint: Callable[[str], str], close_fence: Callable[[], None] | None = None,
             sleep: Callable[[float], None] = time.sleep, monotonic: Callable[[], float] = time.monotonic) -> str:
    """The step's report line. Raises remote.StepRefused with a fixed reason."""
    if step not in BUILT:
        raise remote.StepRefused("step not built yet (OP-3b)")
    key_text = (env.get("FLEET_SSH_KEY") or "").strip()
    if not key_text:
        raise remote.StepRefused("live steps need FLEET_SSH_KEY")
    for line in key_text.splitlines():  # never printed; masked in case anything ever echoes it
        if line.strip():
            emit("::add-mask::" + line.replace("%", "%25").replace("\r", "%0D"))
    slot = fleet["roles"][vm]
    remote.check_target(slot, fingerprint)
    files = step_files(step, fleet["commit"], repo)
    data = remote.payload(files, {})
    ip = my_ip().strip()
    try:
        cidr = str(ipaddress.IPv4Network(f"{ip}/32"))
    except ValueError:
        raise remote.StepRefused("this runner's address isn't an IPv4 address") from None
    emit("::add-mask::" + ip)  # validated first: one IPv4 address, nothing to escape
    run_num, attempt = run_id.rsplit("-", 1)
    timeout = BUILT[step]["timeout"]
    with tempfile.TemporaryDirectory(prefix="step-") as tmp:
        work = Path(tmp)
        os.chmod(work, 0o700)
        key, kh = work / "id", work / "known_hosts"
        key.write_text(key_text + "\n")
        os.chmod(key, 0o600)
        kh.write_text(f"{slot['address']} {slot['host_key']}\n")
        refused, rc, out = None, None, b""
        try:
            try:
                backend.set_job_rules(fleet, run_num, attempt, cidr, fence)
            except ValueError:  # audit.open_rule: not in GitHub's Actions ranges
                refused = "this runner's address isn't in GitHub's Actions ranges"
            except (LinodeError, RuntimeError) as e:  # a lost lock still stops everything
                if isinstance(e, LockLost):
                    raise
                refused = "its firewall rule couldn't be opened"
            else:
                argv = remote.ssh_argv(slot["address"], str(key), kh) + [remote.remote_command(timeout)]
                retry_until = monotonic() + SSH_ATTEMPTS_SECONDS
                while True:
                    rc, out = ssh(argv, data, timeout + remote.FLOCK_WAIT + 60)
                    if rc != 255 or monotonic() >= retry_until:
                        break
                    sleep(SSH_RETRY_PAUSE)
        finally:  # whatever happened, including a half-opened rule set or a lost lock
            try:
                # A lock read that fails (S3) mustn't leave the rule open; a lost lock still stops it.
                backend.set_job_rules(fleet, run_num, attempt, None, close_fence or fence)
                closed = True
            except Exception:  # noqa: BLE001 - reported below; the next sweep removes it
                closed = False
    unclosed = "" if closed else "; its firewall rule couldn't be closed; the next sweep removes it; escalate"
    if refused:
        raise remote.StepRefused(refused + unclosed)
    return summary(rc, remote.parse(out)) + unclosed


def checkip() -> str:  # pragma: no cover - live only
    import urllib.request
    with urllib.request.urlopen("https://checkip.amazonaws.com", timeout=30) as r:
        return r.read(64).decode("ascii", "replace")
