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
   SSH_ATTEMPTS_SECONDS, as deliver-pia does. A 255 is usually a session that
   never started, but one dropped mid-step exits 255 too; a retry still can't
   overlap it, because the bootstrap waits on the same per-VM lock.
4. Always: the rule closed again.
The result is one report line of fixed words and grammar names.
"""

from __future__ import annotations

import base64
import binascii
import importlib.util
import ipaddress
import json
import os
import re
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Callable

from ancestry import is_ancestor
from linode import LinodeError
from lock import LockLost
from steps import remote, rules as rules_mod

STEPS_PATH = "infra/fleet/steps/vm"
# The steps built (OP-3a, OP-3b). The first file is the step itself; the rest
# travel beside it. Every limits.yaml step but renew-vpn (deliver-pia's) is here.
BUILT = {
    "vm-status": {"files": ("vm-status.sh", "block-lib.sh"), "timeout": 60},
    "leak-test": {"files": ("leak-test.sh", "block-lib.sh", "compose.yaml"), "timeout": 600},
    # The leak test (600 s) runs inside it; then the pulls (300 + 120 + 120 s),
    # Lodestar's start (240 s), the proxy (about 200 s), the API wait (180 s),
    # the token (90 s), the collector's start (120 s), and the rest.
    "start-block": {"files": ("start-block.sh", "block-lib.sh", "leak-test.sh", "compose.yaml", "watcher.sh",
                              "scrub.py", "docker-proxy.cfg"), "timeout": 2400},
    # Up to 15 minutes' wait for the segment to end (WAIT_SECONDS), then the switch.
    "arm-switch": {"files": ("arm-switch.sh", "block-lib.sh", "compose.yaml", "watcher.sh", "docker-proxy.cfg"),
                   "timeout": 2100},
    # The host guard's check (X-gluetun-guard-reboot) at its worst: the lock (15 s),
    # retries, then keeping gluetun down (update, stop, kill, inspect: 90 s) and the note.
    "outage-watcher": {"files": ("outage-watcher.sh", "block-lib.sh", "watcher.sh"), "timeout": 180},
    "copy-off": {"files": ("copy-off.sh", "block-lib.sh", "scrub.py"), "timeout": 900},
}
# copy-off's scrubbed document comes back on one line of its own, outside the
# report grammar; it goes only to the private evidence bucket.
EVIDENCE_LINE = re.compile(rb"^evidence ([A-Za-z0-9+/]{1,262144}={0,2})$", re.M)
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
    for i, name in enumerate(BUILT[step]["files"]):
        r = run(["git", "-C", repo, "show", f"{commit}:{STEPS_PATH}/{name}"], capture_output=True, timeout=30)
        if r.returncode != 0:
            raise remote.StepRefused("the step isn't in the fleet's pinned commit")
        files["step.sh" if i == 0 else name] = r.stdout
    return files


def _scrubber():
    """The control plane's copy of the scrubber (this checkout: main's head)."""
    spec = importlib.util.spec_from_file_location("fleet_scrub", Path(__file__).resolve().parent / "vm" / "scrub.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def guard_health(doc: dict) -> str:
    """The host guard's state from the scrubbed document (X-scrub-guard-fields),
    fixed words and a count only. `damaged` makes the block not clean. A
    document from before those fields reads as not reported, never as clean."""
    if "damaged" not in doc and "host_guard_drops" not in doc:
        return "host guard not reported"
    parts = []
    if doc.get("damaged") is True:
        parts.append("not clean: damaged, the host guard couldn't be held")
    if doc.get("host_guard_drops"):
        parts.append(f"host_guard_drops {doc['host_guard_drops']}")
    return "; ".join(parts) or "host guard held"


def copied_off(out: bytes, put: Callable[[bytes], None] | None) -> str:
    """copy-off's document: scrubbed again here, then put in the private evidence
    bucket. Never printed; the report says only whether it was stored, and the
    host guard's state (guard_health)."""
    found = EVIDENCE_LINE.findall(out)
    if len(found) != 1:
        return "no evidence came back"
    try:
        scrub = _scrubber()
        doc = scrub.scrub(json.loads(base64.b64decode(found[0], validate=True)))
        body = scrub.dumps(doc)
    except (binascii.Error, ValueError, UnicodeDecodeError, RecursionError):
        return "the evidence didn't pass the scrubber"
    health = guard_health(doc)
    if put is None:
        return "the evidence couldn't be stored; " + health
    try:
        put(body)
    except Exception:  # noqa: BLE001 - the bucket's error isn't printed
        return "the evidence couldn't be stored; it's still on the VM, so copy-off can run again; " + health
    return "evidence stored in the private bucket; " + health


def bucket_writer(backend, env: dict, run_id: str, vm: str) -> Callable[[bytes], None] | None:
    """A put into the evidence bucket through the backend's owner-checked S3
    client (apply.OwnedS3, so ExpectedBucketOwner is always sent)."""
    s3, bucket = getattr(backend, "s3", None), (env.get("EVIDENCE_BUCKET") or "").strip()
    if s3 is None or not bucket:
        return None
    return lambda body: s3.put_object(Bucket=bucket, Key=f"fleet/{run_id}/copy-off-{vm}.json", Body=body)


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
             sleep: Callable[[float], None] = time.sleep, monotonic: Callable[[], float] = time.monotonic,
             commit: str | None = None, evidence: Callable[[bytes], None] | None = None) -> str:
    """The step's report line. Raises remote.StepRefused with a fixed reason."""
    if step not in BUILT:
        raise remote.StepRefused("step not built")
    key_text = (env.get("FLEET_SSH_KEY") or "").strip()
    if not key_text:
        raise remote.StepRefused("live steps need FLEET_SSH_KEY")
    for line in key_text.splitlines():  # never printed; masked in case anything ever echoes it
        if line.strip():
            emit("::add-mask::" + line.replace("%", "%25").replace("\r", "%0D"))
    slot = fleet["roles"][vm]
    remote.check_target(slot, fingerprint)
    files, args = step_files(step, fleet["commit"], repo), {}
    if step == "start-block":  # at the rules commit, which act checked is an ancestor of main
        if commit is None:
            raise remote.StepRefused("start-block needs a rules commit")
        rules, tools = rules_mod.read(commit, repo)
        for c in (rules["lodestar_commit"], rules["spec_commit"]):
            if not is_ancestor(c, "origin/main", repo):
                raise remote.StepRefused("a commit in the screen rules is not an ancestor of main")
        files.update(tools)
        args = rules_mod.args(rules, commit, vm, run_id)
    data = remote.payload(files, args)
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
    # copy-off's evidence line is taken out before the grammar reads the rest.
    line = summary(rc, remote.parse(EVIDENCE_LINE.sub(b"", out)))
    if step == "copy-off" and rc == 0:
        line += "; " + copied_off(out, evidence)
    return line + unclosed


def checkip() -> str:  # pragma: no cover - live only
    import urllib.request
    with urllib.request.urlopen("https://checkip.amazonaws.com", timeout=30) as r:
        return r.read(64).decode("ascii", "replace")
