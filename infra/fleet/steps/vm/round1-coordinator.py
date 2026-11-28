#!/usr/bin/env python3
"""Round 1's coordinator: three VPN-isolated arms on one host, one shutdown, one archive.

docs/plan/2026-11-27-revised-path-to-100.md §5.2 items 3, 4, 11 and 12, §6.3; the files it
writes are the ones `analyze_cr.py gate-round` reads (docs/decisions/2026-11-27-X-cr-round1-gate.md),
recorded as the agreed interface in docs/decisions/2026-11-27-X-cr-round1-collect.md. The
operator's steps are docs/soak/runsheets/host-round1-runsheet.md.

  round1-coordinator.py COMMAND CONFIG --day-dir DIR [options]

  check-endpoints  every arm on its own VPN server (before any leak test)
  check-start  spacing (the comparator's window end + 12 h), the host's disk budget, fresh
               data, and every arm's passing leak test; exit 1 with the reason otherwise
  leak         run one arm's leak test (leak-test.sh with that arm's settings, from a private
               temporary folder) and record it. A failure stops that arm's Lodestar (every
               arm's with --host-level) until a later passing re-test
  up           start each arm's stack (gluetun and Lodestar) with its declared settings
  bring-up     read each arm's egress inside its own namespace and write the exit assignment
               (X, Y, Z to salted hashes); refuses two arms on one exit, or an exit that is
               the uplink
  collect      start each arm's collector (without --stop) and the 10 s host sampler, detached
  watch        scrape each arm's health and counters every minute; once every collector has
               closed, keep every healthy arm crawling until the latest final boundary + 2 min,
               then stop every stack and the sampler, and write the day archive
  stop         stop every arm now (the rehearsal's 4 h limit), then the archive
  archive      (re)write the day archive from what's recorded
  end-attempt  record an attempt that ends short of its declared days (the manifest's attempt_ends)
  bind-comparator  bind a comparator run without round-1 fields, after its window: its run
               id, the salted identities computed now, and its end (comparator-binding.json)
  identity     the host's salted identity, the uplink's hash, the /data device and the uplink
               interface, for the runner's declaration
  manifest     the round's manifest: every input with its SHA-256, checked for one image
               digest, unique run IDs and contiguous repeats

CONFIG is the round's coordinator file (JSON; the run sheet has the template). Everything it
records is aggregates and salted hashes: no address, hostname, credential or salt. Python 3.10+,
standard library only; root for the namespace reads (egress) and Docker.
"""

import argparse
import fcntl
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOLS = HERE.parents[3] / "docs" / "soak" / "tools" / "linux"
sys.path.insert(0, str(TOOLS))
import collect  # noqa: E402  (the collector's hash, namespace lookup and switch table)

SCRAPE_SECONDS = 60
OVERHANG_SECONDS = 120  # §5.2 item 11: the latest final boundary scrape of the day + 2 min
SPACING = timedelta(hours=12)  # rule 2.2: after the comparator's window end
GAP_SECONDS = 180  # rule 6.6: two scrapes further apart than this are missing evidence
DISK_MARGIN = 1.20
COLLECTOR_START_SECONDS = 60  # each collector writes run.json within this, or the launch is reported
# The rehearsal (CR-SPEC-R1 §6, X-cr-round1-spec-fixes item 1): a 30 min warm-up, then a 3 h
# window, stopped at 4 h from the first start whatever is still running. Measurement days
# keep rule 1.1's 60 min warm-up (collect.py's own default for a spec kind).
REHEARSAL_WARMUP_MINUTES = 30
REHEARSAL_HOURS = 3
REHEARSAL_CAP = timedelta(hours=4)
TUNNEL_INTERFACE = "wg0"  # gluetun's VPN_INTERFACE in round 1's stacks (the image defaults to tun0)
# Why an attempt ended short (end-attempt, the manifest's attempt_ends): comma-separated
# tokens. `rehearsal_drop:<arm>` (the rehearsal dropped it: only for an attempt that ran no
# day) and `product_failure:<arm>` (a class 2 failure on it) are the drops gate-round reads
# for an S attempt's "only arm left" (CR-SPEC-R1 §5); `solo_confirmation:<arm>` and
# `valid_miss:<arm>` name the other ends, and `other:<slug>` anything the runner rules.
END_REASON = re.compile(r"^(?:(rehearsal_drop|product_failure|solo_confirmation|valid_miss):([A-Z])|other:[a-z0-9_]{1,40})$")
ARM = re.compile(r"^[A-Z]$")
EXIT_LABELS = ("X", "Y", "Z")
STACK_ENV = {"LODESTAR_ARM_SUFFIX", "LODESTAR_UI_PORT"}
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))
# Collector reasons that say the arm's own process failed (§6.3 class 2); the runner's
# diagnosis with a fresh reviewer decides, so they're recorded `pending`.
PRODUCT_REASONS = ("process_exited", "three_consecutive_health_failures", "process_memory_over_1GiB_for_five_samples",
                   "rss_over_1GiB_for_five_samples", "active_peer_requests_without_completion_for_five_minutes",
                   "three_consecutive_metrics_failures", "three_consecutive_capacity_violations")
HOST_REASONS = ("disk_reserve", "disk_monitor", "storage")
# The leak test's own refusals before it tests anything: not a verdict on the tunnel.
LEAK_PRECONDITIONS = {"args_valid", "block_running", "block_check", "wg_conf_present", "endpoint_valid",
                      "compose_installed", "leak_test"}


def utc_now():
    return datetime.now(timezone.utc)


def iso(t):
    return t.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def parse_utc(text):
    try:
        t = datetime.fromisoformat(str(text).replace("Z", "+00:00"))
    except ValueError:
        return None
    return t if t.tzinfo else None


def load(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def save(path, value):
    path = Path(path)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(value, indent=1, sort_keys=True, allow_nan=False) + "\n", encoding="utf-8")
    with tmp.open("rb") as fh:
        os.fsync(fh.fileno())
    os.replace(tmp, path)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class Fail(Exception):
    """A refusal: the coordinator prints it and exits 1, having changed nothing further."""


class Partial(Fail):
    """A step that did part of its work and failed: its lines are printed, then exit 1."""

    def __init__(self, lines, message):
        super().__init__(message)
        self.lines = lines


def base_env(extra=None):
    """A scrubbed environment: no stray LODESTAR_* or proxy from root's shell reaches a
    stack, a collector or the leak test; only what they need, plus `extra`."""
    env = {"PATH": os.environ.get("PATH") or "/usr/sbin:/usr/bin:/sbin:/bin",
           "HOME": os.environ.get("HOME") or "/root", "LANG": "C.UTF-8"}
    env.update(extra or {})
    return env


class Host:
    """Everything the coordinator does to the host, so the tests can replace it."""

    def __init__(self):
        self.now = utc_now

    def run(self, argv, env=None, timeout=120):
        try:
            done = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, check=False,
                                  env=base_env(env))
        except (OSError, subprocess.SubprocessError):
            return None, ""
        return done.returncode, done.stdout

    def spawn(self, argv, log, env=None):
        """A detached process (its own session), its output to `log`; returns its PID."""
        with open(log, "ab") as out:
            proc = subprocess.Popen(argv, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                    start_new_session=True, env=base_env(env))
        return proc.pid

    def alive(self, pid, needle):
        """The PID is running and its command line names `needle` (a reused PID never matches)."""
        try:
            return needle.encode() in Path(f"/proc/{int(pid)}/cmdline").read_bytes()
        except (OSError, ValueError, TypeError):
            return False

    def signal(self, pid, sig=signal.SIGTERM):
        try:
            os.kill(pid, sig)
            return True
        except OSError:
            return False

    def get(self, url, timeout=5):
        try:
            with OPENER.open(url, timeout=timeout) as resp:
                return resp.status, resp.read(8 << 20)
        except OSError:
            return 0, b""

    def free_bytes(self, path):
        return shutil.disk_usage(path).free

    def tree_bytes(self, path):
        total = 0
        for root, _dirs, files in os.walk(path):
            for f in files:
                try:
                    total += os.lstat(os.path.join(root, f)).st_size
                except OSError:
                    pass
        return total

    def uplink_ip(self, url):
        return collect.ip_echo(url)

    def egress_ip(self, pid, url):
        return collect.netns_ip_echo(f"/proc/{pid}/ns/net", url)

    def netns_run(self, pid, argv, timeout=15):
        """`argv` inside the PID's network namespace (the host's own iproute2 and ping)."""
        return self.run(["nsenter", f"--net=/proc/{int(pid)}/ns/net", *argv], timeout=timeout)

    def container_logs(self, name, since, until):
        """A container's log between two times, stdout and stderr together (gluetun logs to both)."""
        try:
            done = subprocess.run(["docker", "logs", "--timestamps", "--since", since, "--until", until, name],
                                  stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=300, check=False,
                                  env=base_env())
        except (OSError, subprocess.SubprocessError):
            return None
        return done.stdout if done.returncode == 0 else None

    def sleep(self, seconds):
        time.sleep(seconds)


# ------------------------------------------------------------------ the round's config
def load_config(path):
    cfg = load(path)
    if not (isinstance(cfg, dict) and cfg.get("schema") == 1 and cfg.get("kind") == "cr_round1_coordinator"):
        raise Fail("config: schema 1, kind cr_round1_coordinator")
    base = Path(path).resolve().parent
    for key in ("round_dir", "salt_file", "data_volume", "declaration", "comparator_report"):
        if not isinstance(cfg.get(key), str):
            raise Fail(f"config: {key}")
        # The declaration and the comparator's report live in the round's folder (they're in
        # its manifest); everything else is relative to this file.
        rel = Path(cfg["round_dir"]) if key in ("declaration", "comparator_report") else base
        cfg[key] = str((rel / cfg[key]).resolve()) if not os.path.isabs(cfg[key]) else cfg[key]
    if Path(path).resolve().is_relative_to(Path(cfg["round_dir"]).resolve()):
        # It names the stacks, the salt and the password files: the round's folder is copied off.
        raise Fail("config: keep the coordinator file outside round_dir (it holds host paths; round_dir is copied off)")
    arms = cfg.get("arms")
    if not isinstance(arms, dict) or not arms or not all(ARM.fullmatch(a) for a in arms):
        raise Fail("config: arms {LETTER: {...}}")
    ports, suffixes = set(), set()
    for arm, a in arms.items():
        for key in ("stack", "suffix", "port", "tunnel_target", "password_file"):
            if key not in a:
                raise Fail(f"config: arm {arm} needs {key}")
        if not re.fullmatch(r"-[a-z]", a["suffix"]) or not isinstance(a["port"], int) or not 1024 < a["port"] < 65536:
            raise Fail(f"config: arm {arm}'s suffix is -<letter> and its port 1025-65535")
        if not re.fullmatch(r"(\d{1,3}\.){3}\d{1,3}", str(a["tunnel_target"])):
            raise Fail(f"config: arm {arm}'s tunnel_target is its WireGuard gateway's IPv4 address (declared before the rehearsal)")
        if a["port"] in ports or a["suffix"] in suffixes:
            raise Fail("config: every arm has its own port and suffix")
        ports.add(a["port"])
        suffixes.add(a["suffix"])
    cfg.setdefault("ip_echo_url", collect.DEFAULT_IP_ECHO)
    growth = cfg.get("expected_growth_bytes")
    if not isinstance(growth, dict) or not all(isinstance(growth.get(a, growth.get("default")), int) for a in arms):
        raise Fail("config: expected_growth_bytes {arm or default: bytes per 24 h} covers every arm")
    if not isinstance(cfg.get("uplink_interface"), str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,15}", cfg["uplink_interface"]):
        raise Fail("config: uplink_interface (the `identity` command prints it)")
    cfg["decl"] = load(cfg["declaration"]) if Path(cfg["declaration"]).exists() else None
    return cfg


def gluetun(cfg, arm):
    return "gluetun" + cfg["arms"][arm]["suffix"]


def lodestar(cfg, arm):
    return "lodestar" + cfg["arms"][arm]["suffix"]


def project(cfg, arm):
    return "lodestar" + cfg["arms"][arm]["suffix"]


def compose(cfg, arm, *args):
    stack = cfg["arms"][arm]["stack"]
    return ["docker", "compose", "-p", project(cfg, arm), "--project-directory", stack, "-f",
            str(Path(stack) / "compose.yaml"), *args]


def stack_env(cfg, arm):
    a = cfg["arms"][arm]
    # Round 1's gluetun never restarts on its own (a reboot brings no arm back without its
    # leak test; runner's review of #1150).
    return {"LODESTAR_ARM_SUFFIX": a["suffix"], "LODESTAR_UI_PORT": str(a["port"]),
            "LODESTAR_IMAGE": cfg.get("image") or "unused", "DOCKER_GID": "unused", "LODESTAR_GLUETUN_RESTART": "no"}


# ------------------------------------------------------------------ the day's state
class Day:
    def __init__(self, cfg, day_dir):
        self.cfg, self.dir = cfg, Path(day_dir)
        self.path = self.dir / "state.json"
        self.state = load(self.path) if self.path.exists() else None

    def require(self):
        if self.state is None:
            raise Fail("no day state: run bring-up first (or `leak` for the day's first leak tests)")
        return self.state

    def init(self, attempt, day, repeat, rehearsal=False):
        """The day's state: created from the given placement, or the stored one, which any
        value given must match (an omitted --repeat is the stored one)."""
        if self.state is not None:
            have = (self.state["attempt"], self.state["day"], self.state["repeat"])
            given = (attempt, day, repeat)
            if any(g is not None and g != h for g, h in zip(given, have)):
                raise Fail(f"this day directory is {have}, not {given}")
            return self.state
        if not attempt or day is None:
            raise Fail("--attempt and --day name a new day")
        repeat = repeat or 0
        entries = {e["arm"]: e for e in self.cfg["decl"]["arms"] if e["attempt"] == attempt and e["day"] == day}
        if not entries:
            raise Fail(f"the declaration has no arms for attempt {attempt} day {day}")
        missing = sorted(set(entries) - set(self.cfg["arms"]))
        if missing:
            raise Fail(f"config has no stack for declared arm(s) {missing}")
        self.dir.mkdir(parents=True, exist_ok=True)
        self.state = {"schema": 1, "round": self.cfg["decl"]["round"], "attempt": attempt, "day": day, "repeat": repeat,
                      "rehearsal": bool(rehearsal), "exits": {}, "events": [], "notes": [],
                      "arms": {arm: {"exit": e["exit"], "egress": [], "leak_tests": [], "blocked": False}
                               for arm, e in sorted(entries.items())}}
        self.save()
        return self.state

    def save(self):
        save(self.path, self.state)

    def arms(self):
        return sorted(self.require()["arms"])

    def entry(self, arm):
        s = self.require()
        return next(e for e in self.cfg["decl"]["arms"]
                    if e["attempt"] == s["attempt"] and e["day"] == s["day"] and e["arm"] == arm)


def leak_verdict(text):
    """The leak test's verdict from its output: `pass` when its last line is `ok leak_test`;
    `not_run` when every failure is one of its own refusals before testing (wrong arguments,
    a running block or Lodestar, a missing file or tool), which say nothing about the
    tunnel; otherwise `fail`."""
    lines = [ln.strip() for ln in (text or "").splitlines() if ln.strip()]
    if lines and lines[-1] == "ok leak_test":
        return "pass"
    fails = {ln[5:] for ln in lines if ln.startswith("fail ")}
    if lines and lines[-1] == "fail leak_test" and all(f in LEAK_PRECONDITIONS or f.startswith("tool_missing_")
                                                      for f in fails):
        return "not_run"
    return "fail"


def leak_summary(tests, window_start=None):
    """{before, after}: the latest result before the day's first start, and the latest after it."""
    before = [t for t in tests if t["phase"] in ("before", "retest") and not t.get("after_start")]
    after = [t for t in tests if t["phase"] == "after"]
    return {"before": before[-1]["result"] if before else None, "after": after[-1]["result"] if after else None}


def salt(cfg):
    return collect.read_salt(cfg["salt_file"])


def container_pid(host, name):
    rc, out = host.run(["docker", "inspect", "--type", "container", "-f", "{{.State.Pid}}", name], timeout=30)
    out = (out or "").strip()
    return int(out) if rc == 0 and out.isdigit() and int(out) > 0 else None


def read_egress(cfg, host, arm, key):
    """One salted egress reading inside the arm's gluetun namespace, or None."""
    pid = container_pid(host, gluetun(cfg, arm))
    if pid is None:
        return None
    try:
        ip = host.egress_ip(pid, cfg["ip_echo_url"])
    except (OSError, ValueError, UnicodeError, AttributeError):
        return None
    return {"utc": iso(host.now()), "hash": collect.salted_hash(key, "ip", ip)}


IPV4_TEXT = re.compile(r"(?<![0-9.])(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?![0-9])")


def gateway_of(cfg, host, arm):
    """The arm's WireGuard gateway, read inside its gluetun namespace, compared with its
    declared tunnel_target (CR-SPEC-R1 §7; plan line 441: the probe is "ICMP to its WireGuard
    gateway"). Returns (ok, how) and never the address.

    The target must route through the tunnel interface and not be one of the namespace's own
    addresses (an address that never leaves the namespace never shows loss). Then the gateway
    is the tunnel's peer address or its routes' `via`, where the interface has one; a
    point-to-point WireGuard /32 (PIA's, as gluetun sets it up) has neither, so the gateway is
    the first hop: an echo with TTL 1 that the target itself answers. A target further away
    answers TTL 1 with the gateway's `Time to live exceeded`, and is refused."""
    target = str(cfg["arms"][arm]["tunnel_target"])
    iface = cfg["arms"][arm].get("tunnel_interface", TUNNEL_INTERFACE)
    pid = container_pid(host, gluetun(cfg, arm))
    if pid is None:
        return False, "gluetun_not_running"
    rc, out = host.netns_run(pid, ["ip", "-4", "-o", "addr", "show", "dev", iface])
    if rc != 0 or " inet " not in f" {out or ''}":
        return False, "tunnel_interface_missing"
    local = set(IPV4_TEXT.findall(re.sub(r"peer [0-9.]+", "", out or "")))
    peer = re.search(r" peer ([0-9.]+)(?:/32)?", out or "")
    rc, out = host.netns_run(pid, ["ip", "-4", "-o", "addr", "show"])
    local |= set(IPV4_TEXT.findall(re.sub(r"(peer|brd) [0-9.]+", "", out or ""))) if rc == 0 else set()
    if target in local:
        return False, "target_is_a_namespace_address"
    rc, out = host.netns_run(pid, ["ip", "-4", "route", "get", target])
    words = (out or "").split()
    if rc != 0 or not words or words[0] in ("local", "broadcast", "unreachable", "prohibit", "blackhole") \
            or "dev" not in words or words[words.index("dev") + 1:words.index("dev") + 2] != [iface]:
        return False, "target_not_through_the_tunnel"
    if peer:
        return peer[1] == target, "peer_address"
    rc, out = host.netns_run(pid, ["ip", "-4", "route", "show", "table", "all", "dev", iface])
    vias = set(re.findall(r" via ([0-9.]+)", out or "")) if rc == 0 else set()
    if vias:
        return vias == {target}, "route_via"
    rc, out = host.netns_run(pid, ["ping", "-n", "-c", "1", "-t", "1", "-W", "2", target])
    answered = rc == 0 and re.search(r"(?m)^\d+ bytes from " + re.escape(target) + r": ", out or "") is not None
    return answered, "first_hop"


# ------------------------------------------------------------------ commands
def endpoint_of(stack):
    """An arm's WireGuard endpoint from its wg0.conf, compared and never printed or kept."""
    try:
        text = (Path(stack) / "secrets" / "wg0.conf").read_text(encoding="utf-8")
    except OSError:
        return None
    m = re.search(r"(?m)^Endpoint = ([0-9.]+):([0-9]{1,5})$", text)
    return (m[1], m[2]) if m else None


def cmd_check_endpoints(cfg, day, host, args):
    """Every arm on its own PIA server: the leak test's recreate and host guards are scoped
    to the endpoint, so two arms on one endpoint could hold each other's tunnel."""
    seen = {}
    for arm in sorted(cfg["arms"]):
        ep = endpoint_of(cfg["arms"][arm]["stack"])
        if ep is None:
            raise Fail(f"arm {arm}'s secrets/wg0.conf has no readable Endpoint line")
        if ep[0] in seen:
            raise Fail(f"arms {seen[ep[0]]} and {arm} use the same VPN server: give each arm its own")
        seen[ep[0]] = arm
    return [f"ok endpoints_distinct arms={','.join(sorted(cfg['arms']))}"]


def cmd_check_start(cfg, day, host, args):
    """§6.3's start gates. Returns the lines printed; raises Fail on the first gate that fails."""
    s = day.require()
    lines = cmd_check_endpoints(cfg, day, host, args)
    comp = load(cfg["comparator_report"])
    end = parse_utc(((comp.get("timeline") or {}).get("window_end_utc")))
    if end is None:
        raise Fail("the comparator report has no timeline.window_end_utc")
    if s.get("rehearsal"):  # §8 item 5: inside the 12 h, from stores deleted after; never beside the comparator
        ended = (comp.get("binding") or {}).get("ended_utc") or (comparator_binding(cfg) or {}).get("ended_utc")
        if host.now() < end or not ended:
            raise Fail(f"the comparator is still running (its window ends {iso(end)}): the rehearsal waits for it")
        lines.append("ok rehearsal after the comparator (spacing doesn't apply: not a measurement)")
    elif host.now() < end + SPACING:
        raise Fail(f"too early: no arm may start before {iso(end + SPACING)} (the comparator's window end + 12 h)")
    else:
        lines.append(f"ok spacing (comparator window end + 12 h = {iso(end + SPACING)})")
    growth = cfg["expected_growth_bytes"]
    need_growth = sum(int(growth.get(arm, growth.get("default", 0))) for arm in day.arms())
    retained = host.tree_bytes(cfg["round_dir"])
    need = int(DISK_MARGIN * (need_growth + retained))
    free = host.free_bytes(cfg["data_volume"])
    if free < need:
        raise Fail(f"disk budget: {free} bytes free, {need} needed (growth {need_growth} + archives {retained}, +20 %)")
    lines.append(f"ok disk_budget free={free} need={need}")
    for arm in day.arms():
        st = s["arms"][arm]
        if st["blocked"]:
            raise Fail(f"arm {arm} is stopped after a failed leak test: repair, then a passing `leak --phase retest`")
        if leak_summary(st["leak_tests"])["before"] != "pass":
            raise Fail(f"arm {arm} has no passing leak test before the start")
        for sub in ("data", "witness"):
            d = Path(cfg["arms"][arm]["stack"]) / sub
            if d.exists() and any(d.iterdir()):
                raise Fail(f"arm {arm}'s {sub} directory isn't empty: every day starts fresh (§6.3)")
    checks = s.get("exit_checks") or []
    if not checks:
        raise Fail("no exit check yet: run bring-up after the leak tests")
    last = parse_utc(checks[-1]["utc"])
    for arm in day.arms():
        tests = s["arms"][arm]["leak_tests"]
        if tests and parse_utc(tests[-1]["utc"]) > last:
            raise Fail(f"arm {arm}'s latest leak test is after the last exit check: run bring-up again")
    lines.append("ok leak_tests_before ok fresh_dirs ok exits_checked_after_leak_tests")
    return lines


def stop_arm(cfg, host, arm):
    rc, _ = host.run(["docker", "stop", "-t", "60", lodestar(cfg, arm)], timeout=120)
    return rc == 0


def measuring(day, host):
    """The arms whose collector is still measuring: started, no result yet, still running."""
    s = day.require()
    return [arm for arm, st in sorted(s["arms"].items()) if st.get("collector_pid")
            and not (day.dir / arm / "result.json").exists() and host.alive(st["collector_pid"], "collect.py")]


GRAMMAR = re.compile(r"^(?:(?:ok|fail|skip) [a-z0-9_]{1,64}|count [a-z0-9_]{1,80} [0-9]{1,12})$")
LEAK_FILES = ("leak-test.sh", "block-lib.sh", "compose.yaml")
LEAK_TIMEOUT = 1500  # the step's own 900 s, and margin for a loaded host
LEAK_CLEANUP_SECONDS = 120  # after TERM, the test's EXIT trap releases its rules and captures


def run_leak_test(cfg, host, arm):
    """The arm's leak test, run by the coordinator from a fresh private work folder (its
    captures hold raw addresses, so they never touch the checkout and are deleted after).
    Only the test's grammar lines (stdout) are kept."""
    a = cfg["arms"][arm]
    work = tempfile.mkdtemp(prefix="round1-leak-")
    try:
        os.chmod(work, 0o700)
        for name in LEAK_FILES:
            shutil.copyfile(HERE / name, Path(work) / name)
        env = {"LODESTAR_ARM": a["suffix"][1:], "LODESTAR_UI_PORT": str(a["port"]), "LODESTAR_GLUETUN_RESTART": "no"}
        # `timeout` sends TERM, so the test's own traps release its guards, its endpoint block and
        # its captures before the folder goes (a kill from Python would skip them); Python's own
        # limit is longer, for that cleanup (steps/remote.py runs steps the same way).
        rc, out = host.run(["timeout", "-s", "TERM", str(LEAK_TIMEOUT), "bash", str(Path(work) / "leak-test.sh"),
                            work, a["stack"]], env=env, timeout=LEAK_TIMEOUT + LEAK_CLEANUP_SECONDS)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    lines = [ln for ln in (out or "").splitlines() if GRAMMAR.fullmatch(ln)]
    if rc is None or rc == 124:  # timed out: never a pass, whatever it printed before
        lines += ["fail leak_test_timed_out", "fail leak_test"]
    return lines


def leak_lock(cfg):
    """One leak test at a time per round folder (its lock file lives there): each recreates its
    own tunnel and sets host rules."""
    fh = open(Path(cfg["round_dir"]) / ".leak-test.lock", "a")
    try:
        fcntl.flock(fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        fh.close()
        raise Fail("another leak test is running: arms are tested one at a time") from None
    return fh


def cmd_leak(cfg, day, host, args):
    """Run and record one arm's leak test (§5.2 item 3). Leak tests run only while no window is
    open (§6.3), one arm at a time. A failure stops crawling on the arm (every arm with
    --host-level) until a passing re-test. Before the day's collectors start nothing has been
    measured, so it blocks the arm and is noted; once they have, it's an evidenced
    infrastructure fault (§6.3 class 1), an event."""
    s = day.init(args.attempt, args.day, args.repeat, args.rehearsal)
    if args.arm not in s["arms"]:
        raise Fail(f"arm {args.arm} isn't in this day")
    busy = measuring(day, host)
    if busy:
        raise Fail(f"arm(s) {busy} are still measuring: leak tests run only while no window is open")
    lock = leak_lock(cfg)
    try:
        lines_out = run_leak_test(cfg, host, args.arm)
    finally:
        lock.close()
    text = "\n".join(lines_out) + "\n"
    n = len(list(day.dir.glob(f"leak-{args.arm}-*.log"))) + 1  # every run numbered, `not_run` ones too
    log = day.dir / f"leak-{args.arm}-{args.phase}-{n}.log"
    log.write_text(text, encoding="utf-8")
    result = leak_verdict(text)
    started = any(a.get("collector_pid") for a in s["arms"].values())
    if result == "not_run":
        return [f"skip leak_test arm={args.arm}: the test refused before testing (see {log.name}); nothing recorded"]
    s["arms"][args.arm]["leak_tests"].append({"utc": iso(host.now()), "phase": args.phase, "result": result,
                                              "after_start": started and args.phase != "after", "log": log.name,
                                              "log_sha256": hashlib.sha256(text.encode()).hexdigest()})
    lines = [f"{'ok' if result == 'pass' else 'fail'} leak_test arm={args.arm} phase={args.phase}"]
    if result == "fail":
        hit = sorted(s["arms"]) if args.host_level else [args.arm]
        for arm in hit:
            s["arms"][arm]["blocked"] = True
            stopped = stop_arm(cfg, host, arm)
            s.setdefault("coordinator_stopped", []).append(arm)
            lines.append(f"{'ok' if stopped else 'fail'} crawling_stopped arm={arm}")
        if started or args.phase == "after":
            s["events"].append({"class": "infrastructure", "cause": "leak_test_failed",
                                "arms": [] if args.host_level else hit, "utc": iso(host.now()), "diagnosis": None})
        else:  # nothing measured yet: the arm waits for a repair and a passing re-test
            s["notes"].append(f"{iso(host.now())} leak test failed before the start on arm(s) {hit}; blocked until a retest")
    elif args.phase == "retest" and s["arms"][args.arm]["blocked"]:
        s["arms"][args.arm]["blocked"] = False  # repaired and re-tested: it may crawl again
        lines.append(f"ok unblocked arm={args.arm}")
    day.save()
    return lines


def arm_switch_env(entry):
    """The arm's declared settings as explicit environment (§6.1: every switch set, safe_restart
    on, LODESTAR_INTENSITY only for a non-Balanced preset)."""
    env = {collect.SWITCH_ENV[k]: "true" if v else "false" for k, v in entry["switches"].items()}
    env[collect.SWITCH_ENV["safe_restart"]] = "true"
    if entry["preset"] != "balanced":
        env["LODESTAR_INTENSITY"] = entry["preset"]
    return env


def cmd_up(cfg, day, host, args):
    cmd_check_start(cfg, day, host, args)
    if not cfg.get("image") or "@sha256:" not in cfg["image"]:
        raise Fail("config: image must be pinned by digest (name@sha256:...)")
    if cfg["image"] != cfg["decl"]["image_digest"]:  # both name@sha256:<hex>: run by the declared digest
        raise Fail("config: image's digest isn't the declaration's image_digest")
    lines, up = [], []
    for arm in day.arms():
        env = dict(stack_env(cfg, arm), **arm_switch_env(day.entry(arm)))
        rc, _ = host.run(compose(cfg, arm, "--profile", "block", "up", "-d", "lodestar"), env=env, timeout=300)
        if rc != 0:  # no partial day: what started is stopped again, the failing arm too (its
            # containers may be up although the command failed or timed out)
            for other in up + [arm]:
                rc2, _ = host.run(compose(cfg, other, "stop"), env=stack_env(cfg, other), timeout=300)
                lines.append(f"{'ok' if rc2 == 0 else 'fail'} stack_stopped arm={other}")
            raise Partial(lines, f"arm {arm}'s stack didn't start; every arm's stack, its own too, was stopped again")
        up.append(arm)
        day.require()["arms"][arm]["up_utc"] = iso(host.now())  # its gluetun log is archived from here
        day.save()
        lines.append(f"ok up arm={arm}")
    return lines


def cmd_bring_up(cfg, day, host, args):
    """§5.2 item 4: each arm's exit as a salted hash, written when the stacks come up."""
    s = day.init(args.attempt, args.day, args.repeat, args.rehearsal)
    key = salt(cfg)
    try:
        uplink = collect.salted_hash(key, "ip", host.uplink_ip(cfg["ip_echo_url"]))
    except (OSError, ValueError, UnicodeError):
        raise Fail("can't read the uplink's address outside the tunnels") from None
    readings = {}
    for arm in day.arms():
        r = read_egress(cfg, host, arm, key)
        if r is None:
            raise Fail(f"arm {arm}'s egress couldn't be read inside its namespace")
        readings[arm] = r
    hashes = [r["hash"] for r in readings.values()]
    if len(set(hashes)) != len(hashes):
        raise Fail("two arms share an egress address: each arm needs its own exit (§8 item 2)")
    if uplink in hashes:
        raise Fail("an arm's egress is the uplink's own address: its traffic isn't in the tunnel")
    for arm in day.arms():  # the probe's target is the arm's WireGuard gateway (X-cr-round1-spec-fixes item 4)
        ok, how = gateway_of(cfg, host, arm)
        if not ok:
            raise Fail(f"arm {arm}'s tunnel_target isn't its WireGuard gateway ({how}): declare the gateway")
        readings[arm]["gateway_check"] = how
    exits = {s["arms"][arm]["exit"]: readings[arm]["hash"] for arm in day.arms()}
    started = any(a.get("collector_pid") for a in s["arms"].values())
    if s["exits"] and s["exits"] != exits:
        if started:
            raise Fail("the exits changed since this day's bring-up, after its start")
        # Before the start (runner, 2026-11-27): a repair that changed a server is checked
        # again, and the archive keeps both checks.
        s["notes"].append(f"{iso(host.now())} exits checked again before the start: "
                          + ", ".join(sorted(k for k in exits if exits[k] != s["exits"].get(k))) + " changed")
    # Stamped with this check's first reading, so every reading it took is in force under it.
    first = iso(min(parse_utc(r["utc"]) for r in readings.values()))
    s.setdefault("exit_checks", []).append({"utc": first, "exits": exits})
    s["exits"] = exits
    for arm in day.arms():
        how = readings[arm].pop("gateway_check")
        s["arms"][arm]["egress"].append(readings[arm])
        s["arms"][arm].update(tunnel_target_is_gateway=True, tunnel_gateway_check=how)
    s["uplink_hash"] = uplink
    day.save()
    return [f"ok exit {s['arms'][arm]['exit']} arm={arm}" for arm in day.arms()]


def collector_argv(cfg, day, arm, run_id):
    a, s, e = cfg["arms"][arm], day.require(), day.entry(arm)
    argv = [sys.executable, "-I", str(TOOLS / "collect.py"), "--mode", "docker", "--container", lodestar(cfg, arm),
            "--url", f"http://127.0.0.1:{a['port']}", "--root", str(day.dir / arm), "--commit", cfg["commit"],
            "--run-kind", "comb", "--spec-commit", cfg["decl"]["spec_commit"], "--intensity", e["preset"],
            "--password-file", a["password_file"], "--ip-echo-url", cfg["ip_echo_url"],
            "--storage-path", str(Path(a["stack"]) / "data"),
            "--round", str(s["round"]), "--attempt", s["attempt"], "--day", str(s["day"]), "--arm", arm,
            "--repeat", str(s["repeat"]), "--run-id", run_id, "--identity-salt-file", cfg["salt_file"]]
    if s.get("rehearsal"):  # the rehearsal (CR-SPEC-R1 §6): marked in run.json, never a measurement
        argv += ["--rehearsal", "--warmup-minutes", str(REHEARSAL_WARMUP_MINUTES), "--baseline-hours", str(REHEARSAL_HOURS)]
    for item in cfg.get("checklist", []):
        argv += ["--checklist", item]
    for item in cfg.get("next_run", []):
        argv += ["--next-run", item]
    return argv


def run_id_for(day, arm):
    s = day.require()
    return (f"r{s['round']}-{s['attempt']}-d{s['day']}-{arm}" + (f"-rep{s['repeat']}" if s["repeat"] else "")
            + ("-rehearsal" if s.get("rehearsal") else ""))


def cmd_collect(cfg, day, host, args):
    s = day.require()
    if not s["exits"]:
        raise Fail("bring-up first: the exits aren't recorded")
    for key in ("commit",):
        if not cfg.get(key):
            raise Fail(f"config: {key}")
    lines, started = [], []
    for arm in day.arms():
        if s["arms"][arm]["blocked"]:
            raise Fail(f"arm {arm} is stopped after a failed leak test")
    for arm in day.arms():
        st = s["arms"][arm]
        if st.get("collector_pid"):
            continue
        run_id = run_id_for(day, arm)
        st["run_id"] = run_id
        st["collector_pid"] = host.spawn(collector_argv(cfg, day, arm, run_id), str(day.dir / f"collector-{arm}.log"))
        st["collector_started_utc"] = iso(host.now())
        day.save()  # every PID recorded as soon as it exists
        started.append(arm)
        lines.append(f"ok collector arm={arm} run_id={run_id}")
    # Each collector has launched once it writes run.json; one that exits first refused.
    deadline = host.now() + timedelta(seconds=COLLECTOR_START_SECONDS)
    waiting = list(started)
    while waiting:
        for arm in list(waiting):
            if (day.dir / arm / "run.json").exists():
                waiting.remove(arm)
            elif not host.alive(s["arms"][arm]["collector_pid"], "collect.py"):
                s["arms"][arm]["collector_pid"] = None
                day.save()
                raise Fail(f"arm {arm}'s collector exited at launch: read {day.dir / f'collector-{arm}.log'}, fix, rerun collect")
        if waiting and host.now() >= deadline:
            lines += [f"warn collector arm={arm} still preparing after {COLLECTOR_START_SECONDS} s" for arm in waiting]
            break
        if waiting:
            host.sleep(1)
    if not s.get("sampler_pid") or not host.alive(s["sampler_pid"], "host_samples.py"):
        if s.get("sampler_pid"):
            s["notes"].append(f"{iso(host.now())} host sampler wasn't running; started again")
        argv = [sys.executable, "-I", str(TOOLS / "host_samples.py"), "run", "--out", str(day.dir / "host-samples.jsonl"),
                "--device", cfg["decl"]["backing_device"], "--uplink", cfg["uplink_interface"]]
        for arm in day.arms():
            if cfg["arms"][arm].get("tunnel_target"):
                argv += ["--tunnel", f"{arm}={gluetun(cfg, arm)}:{cfg['arms'][arm]['tunnel_target']}"]
        s["sampler_pid"] = host.spawn(argv, str(day.dir / "host-samples.log"))
        lines.append("ok host_sampler")
    if s.get("rehearsal"):
        end, cap = rehearsal_ends(s)
        lines.append(f"{'warn' if end > cap else 'ok'} rehearsal ends by {iso(end)} at the latest; "
                     f"watch stops every arm at {iso(cap)} (4 h from the first start)" + (" first" if end > cap else ""))
    day.save()
    return lines


def first_start(s):
    """The day's first start: the earliest `up` or collector start any arm recorded."""
    times = [parse_utc(a.get(k)) for a in s["arms"].values() for k in ("up_utc", "collector_started_utc") if a.get(k)]
    return min(times) if times else None


def rehearsal_ends(s):
    """(the latest the rehearsal's last window can end, the 4 h cap). The worst case is the
    last collector's start, its launch allowance, the 30 min warm-up, the 3 h window and the
    overhang: the stagger between the arms' starts adds to it."""
    starts = [parse_utc(a["collector_started_utc"]) for a in s["arms"].values() if a.get("collector_started_utc")]
    end = max(starts) + timedelta(seconds=COLLECTOR_START_SECONDS + OVERHANG_SECONDS,
                                  minutes=REHEARSAL_WARMUP_MINUTES, hours=REHEARSAL_HOURS)
    return end, first_start(s) + REHEARSAL_CAP


def metric(text, name):
    m = re.search(r"(?m)^lodestar_" + re.escape(name) + r" ([0-9]+(?:\.[0-9]+)?)\r?$", text or "")
    return float(m[1]) if m else None


def scrape(cfg, host, arm):
    """One activity reading: health, and the counters that show the process working."""
    base = f"http://127.0.0.1:{cfg['arms'][arm]['port']}"
    health, _ = host.get(base + "/healthz")
    status, body = host.get(base + "/metrics")
    text = body.decode("utf-8", "replace") if status == 200 else ""
    return {"utc": iso(host.now()), "arm": arm, "health": health,
            "queries_out": metric(text, "dht_queries_out_total"), "packets_in": metric(text, "dht_packets_in_total")}


def working(rows, counter):
    """The times a source showed the process working: healthy, with its counter above the
    source's last healthy reading. A failed scrape doesn't reset the comparison."""
    out, prev = [], None
    for r in sorted(rows, key=lambda r: parse_utc(r["utc"])):
        v = r.get(counter)
        if r.get("health") == 200 and isinstance(v, (int, float)):
            if prev is not None and v > prev:
                out.append(parse_utc(r["utc"]))
            prev = v
    return out


def active_span(rows, gap=GAP_SECONDS, collector_rows=()):
    """§5.2 item 11's activity, from two sources: the coordinator's minute scrapes
    (`dht_queries_out_total`) and the collector's own (`dht_lookups_completed_total`, up to its
    final boundary). Active from the first time either shows the process working, through the
    last of the run in which such times are never more than 180 s apart (rule 6.6). An isolated
    failed scrape doesn't end it; a process that stops working for longer does, for good."""
    times = sorted(working(rows, "queries_out") + working(collector_rows, "lookups_completed"))
    if not times:
        return None, None
    start = end = times[0]
    for t in times[1:]:
        if (t - end).total_seconds() > gap:
            break
        end = t
    return start, end


def collector_state(run_dir):
    """A collector's progress: whether it closed, why, and its final boundary scrape's time."""
    run_dir = Path(run_dir)
    out = {"closed": (run_dir / "result.json").exists(), "reason": None, "final_boundary_utc": None,
           "window_hours": None, "egress": [], "activity": []}
    try:
        run = load(run_dir / "run.json")
        out["window_hours"] = int(run["settings"]["baseline_hours"])
        out["run_id"] = run.get("run_id")
    except (OSError, ValueError, KeyError, TypeError):
        pass
    if out["closed"]:
        try:
            out["reason"] = load(run_dir / "result.json").get("reason")
        except (OSError, ValueError):
            out["reason"] = "unreadable"
    try:
        lines = (run_dir / "samples.jsonl").read_text(encoding="utf-8").splitlines()
    except OSError:
        lines = []
    for line in lines:
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row.get("time"), str) and "health" in row:
            out["activity"].append({"utc": row["time"], "health": row["health"],
                                    "lookups_completed": (row.get("values") or {}).get("dht_lookups_completed_total")})
        if isinstance(row.get("egress"), dict) and row["egress"].get("read") == "ok":
            out["egress"].append({"utc": row["time"], "hash": row["egress"]["hash"]})
        for retry in row.get("egress_retries") or []:  # the final boundary's own retries, each with its time
            if isinstance(retry, dict) and retry.get("read") == "ok" and isinstance(retry.get("utc"), str):
                out["egress"].append({"utc": retry["utc"], "hash": retry["hash"]})
        if out["window_hours"] is not None and out["window_hours"] in (row.get("boundary") or []):
            out["final_boundary_utc"] = row["time"]
    return out


def cached_state(cache, run_dir):
    """collector_state, read again only when the collector has written since."""
    try:
        key = ((run_dir / "samples.jsonl").stat().st_size, (run_dir / "result.json").exists())
    except OSError:
        key = None
    if cache.get(run_dir, (None,))[0] != key or key is None:
        cache[run_dir] = (key, collector_state(run_dir))
    return cache[run_dir][1]


def shutdown_at(states):
    """When the coordinator stops every stack: once every collector has closed, the latest
    final boundary scrape of the day + 2 min (§5.2 item 11). None while any is still measuring."""
    if not states or not all(st["closed"] for st in states.values()):
        return None
    finals = [parse_utc(st["final_boundary_utc"]) for st in states.values() if st["final_boundary_utc"]]
    return (max(finals) if finals else None) or "now"


def with_liveness(day, host, arm, st):
    """A collector that died without writing result.json is closed, as `collector_died`."""
    pid = day.require()["arms"][arm].get("collector_pid")
    if not st["closed"] and pid and not host.alive(pid, "collect.py"):
        return dict(st, closed=True, reason="collector_died")
    return st


def classify_close(arm, st, stopped=()):
    """An early close, as an event (§6.3). A product failure is `pending` until the runner's
    diagnosis; a host guard is infrastructure; anything else is `unclassified`, which
    gate-round reads as an unknown event, so the day fails closed until the runner classes it."""
    reason = st["reason"]
    if reason == "baseline_window_complete" or reason is None:
        return None
    if arm in stopped:  # the coordinator stopped it after a failed leak test: that event covers it
        return None
    if any(reason.startswith(p) for p in PRODUCT_REASONS):
        return {"class": "product", "arms": [arm], "cause": reason, "diagnosis": "pending"}
    if any(reason.startswith(p) for p in HOST_REASONS):
        return {"class": "infrastructure", "arms": [arm], "cause": reason, "diagnosis": None}
    return {"class": "unclassified", "arms": [arm], "cause": reason}


def cmd_watch(cfg, day, host, args, max_loops=None):
    s = day.require()
    if s.get("shutdown_utc"):
        raise Fail(f"this day was already shut down at {s['shutdown_utc']}: run `archive` to rewrite its archive")
    log = day.dir / "activity.jsonl"
    loops, cache = 0, {}
    while True:
        with log.open("a", encoding="utf-8") as fh:
            for arm in day.arms():
                fh.write(json.dumps(scrape(cfg, host, arm), separators=(",", ":")) + "\n")
        states = {arm: with_liveness(day, host, arm, cached_state(cache, day.dir / arm)) for arm in day.arms()}
        when = shutdown_at(states)
        if when is not None and (when == "now" or host.now() >= when + timedelta(seconds=OVERHANG_SECONDS)):
            break
        first = first_start(s) if s.get("rehearsal") else None
        if first is not None and host.now() >= first + REHEARSAL_CAP:  # CR-SPEC-R1 §6: the 4 h cap
            s["notes"].append(f"{iso(host.now())} the rehearsal reached its 4 h cap from the first start "
                              f"({iso(first)}): every arm stopped")
            break
        loops += 1
        if max_loops is not None and loops >= max_loops:
            return ["ok watch paused (test bound)"]
        wait = SCRAPE_SECONDS
        if first is not None:  # wake at the cap, not up to a minute after it
            wait = max(1, min(wait, int((first + REHEARSAL_CAP - host.now()).total_seconds()) + 1))
        host.sleep(wait)
    key = salt(cfg)
    lines = []
    for arm in day.arms():  # egress through the last boundary, before anything stops
        r = read_egress(cfg, host, arm, key)
        if r is not None:
            s["arms"][arm]["egress"].append(r)
        else:
            s["notes"].append(f"{iso(host.now())} arm {arm}: egress unreadable at shutdown")
    for arm, st in states.items():
        if st["reason"] == "collector_died":
            s["arms"][arm]["collector_died"] = True
    lines += stop_all(cfg, day, host)
    lines += cmd_archive(cfg, day, host, args)
    return failed_stops(lines)


def failed_stops(lines):
    """State and the archive are saved first; then a stack that didn't stop fails the step."""
    bad = [ln for ln in lines if ln.startswith("fail stack_stopped")]
    if bad:
        raise Partial(lines, f"{len(bad)} stack(s) didn't stop: stop them by hand (run sheet), then tell the runner")
    return lines


def stop_all(cfg, day, host):
    """Stop every arm's stack and the sampler, and record when."""
    s, lines = day.require(), []
    s["shutdown_utc"] = iso(host.now())
    for arm in day.arms():
        rc, _ = host.run(compose(cfg, arm, "stop"), env=stack_env(cfg, arm), timeout=300)
        lines.append(f"{'ok' if rc == 0 else 'fail'} stack_stopped arm={arm}")
    pid = s.get("sampler_pid")
    if pid and host.alive(pid, "host_samples.py"):
        host.signal(pid)
        lines.append("ok host_sampler_stopped")
    day.save()
    return lines


def cmd_stop(cfg, day, host, args):
    """Stop every arm now (the rehearsal's 4 h limit, or the runner's call), then the archive."""
    if day.require().get("shutdown_utc"):
        raise Fail("this day was already shut down")
    day.require()["notes"].append(f"{iso(host.now())} stopped by `stop`, not at the coordinated shutdown")
    return failed_stops(stop_all(cfg, day, host) + cmd_archive(cfg, day, host, args))


IPV6_CANDIDATE = re.compile(r"(?i)(?<![0-9a-z:.])[0-9a-f:.]*:[0-9a-f:.]*:[0-9a-f:.]*(?:%[0-9a-z_.-]+)?(?![0-9a-z:])")
IPV4_LOG = re.compile(r"(?<![0-9])(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?![0-9])")
WG_KEY = re.compile(r"(?<![A-Za-z0-9+/])[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=(?![A-Za-z0-9+/=])")


def redact_log(text):
    """A gluetun log with every IPv6 and IPv4 address and every WireGuard key (32 bytes,
    base64) replaced. A candidate IPv6 token is replaced only if it parses as one, so a
    time like 10:00:00 is kept: the handshake evidence is in the times."""
    import ipaddress

    def v6(m):
        token = m[0].split("%", 1)[0].strip(".")
        try:
            ipaddress.IPv6Address(token)
        except ValueError:
            return m[0]
        return "[ipv6]"
    text = WG_KEY.sub("[key]", text)
    text = IPV6_CANDIDATE.sub(v6, text)
    return IPV4_LOG.sub("[ipv4]", text)


def save_gluetun_logs(cfg, day, host):
    """Each arm's gluetun log for the day, from its start (`up`) to the shutdown, redacted, as
    <arm>/gluetun.log (X-cr-round1-spec-fixes item 6): the evidence for handshake events.
    Returns {arm: sha256}; an arm whose log can't be read is noted and has none."""
    s, out = day.require(), {}
    until = s.get("shutdown_utc") or iso(host.now())
    for arm in day.arms():
        st = s["arms"][arm]
        since = st.get("up_utc") or ((s.get("exit_checks") or [{}])[0].get("utc"))
        if not since:
            continue  # never started: no log for the day
        raw = host.container_logs(gluetun(cfg, arm), since, until)
        if raw is None:
            note = f"arm {arm}: gluetun's log couldn't be read for the archive"
            if not any(n.endswith(note) for n in s["notes"]):
                s["notes"].append(f"{iso(host.now())} {note}")
                day.save()
            continue
        data = redact_log(raw.decode("utf-8", "replace")).encode("utf-8")
        path = day.dir / arm / "gluetun.log"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        out[arm] = hashlib.sha256(data).hexdigest()
    return out


def cmd_archive(cfg, day, host, args):
    """The day archive gate-round reads: exits, and per arm its egress, activity and leak tests,
    whether its probe target was checked as its gateway, its gluetun log's hash, and the
    day's events."""
    s = day.require()
    logs = save_gluetun_logs(cfg, day, host)
    rows = []
    log = day.dir / "activity.jsonl"
    if log.exists():
        for line in log.read_text(encoding="utf-8").splitlines():
            try:
                rows.append(json.loads(line))
            except ValueError:
                pass
    events = list(s["events"])
    arms = {}
    for arm in day.arms():
        st = s["arms"][arm]
        col = collector_state(day.dir / arm)
        if st.get("collector_died") and not col["closed"]:
            col = dict(col, closed=True, reason="collector_died")
        start, end = active_span([r for r in rows if r.get("arm") == arm], collector_rows=col["activity"])
        egress = sorted(st["egress"] + col["egress"], key=lambda r: r["utc"])
        arms[arm] = {"exit": st["exit"], "run_id": st.get("run_id"), "egress": egress,
                     "active_from_utc": iso(start) if start else None, "active_through_utc": iso(end) if end else None,
                     "activity_scrapes": sum(1 for r in rows if r.get("arm") == arm),
                     "collector": {"closed": col["closed"], "reason": col["reason"],
                                   "final_boundary_utc": col["final_boundary_utc"]},
                     "leak_test": leak_summary(st["leak_tests"]), "leak_tests": st["leak_tests"],
                     "tunnel_target_is_gateway": st.get("tunnel_target_is_gateway") is True}
        if arm in logs:
            arms[arm]["gluetun_log_sha256"] = logs[arm]
        ev = classify_close(arm, col, s.get("coordinator_stopped", ()))
        if ev is not None and not any(e.get("cause") == ev["cause"] and e.get("arms") == ev["arms"] for e in events):
            events.append(ev)
    archive = {"schema": 1, "round": s["round"], "attempt": s["attempt"], "day": s["day"], "repeat": s["repeat"],
               "exits": dict(s["exits"]), "exit_checks": s.get("exit_checks", []), "arms": arms, "events": events,
               "shutdown_utc": s.get("shutdown_utc"),
               "notes": s["notes"]}
    save(day.dir / "archive.json", archive)
    return ["ok archive " + str(day.dir / "archive.json")]


def cmd_end_attempt(cfg, day, host, args):
    """attempt_ends (gate-round's manifest): the days this attempt ran, from the days listed."""
    path = Path(cfg["round_dir"]) / "attempt-ends.json"
    ends = load(path) if path.exists() else {}
    declared = {a["id"] for a in cfg["decl"]["attempts"]}
    if args.attempt not in declared:
        raise Fail(f"attempt {args.attempt} isn't declared")
    ran = sorted({d["day"] for d in list_days(cfg) if d["attempt"] == args.attempt})
    if ran not in ([], [1], [1, 2]):
        raise Fail(f"attempt {args.attempt} ran days {ran}: only [], [1] or [1, 2] can be recorded")
    own = {e["arm"] for e in cfg["decl"]["arms"] if e["attempt"] == args.attempt}
    for token in args.reason.split(","):
        m = END_REASON.fullmatch(token)
        if not m:
            raise Fail(f"--reason {token!r}: give one or more of rehearsal_drop:<arm>, product_failure:<arm>, "
                       "solo_confirmation:<arm>, valid_miss:<arm> or other:<slug>, comma-separated")
        if m[2] is not None and m[2] not in own:
            raise Fail(f"--reason {token}: arm {m[2]} isn't in attempt {args.attempt}")
        if m[1] == "rehearsal_drop" and ran:
            raise Fail(f"--reason {token}: the rehearsal drops an arm only before the attempt's first day")
    ends[args.attempt] = {"reason": args.reason, "days_run": ran}
    save(path, ends)
    return [f"ok attempt_end {args.attempt} days_run={ran}"]


def list_days(cfg):
    """Every day directory under the round with a state file, as (attempt, day, repeat, dir)."""
    out = []
    for path in sorted(Path(cfg["round_dir"]).rglob("state.json")):
        st = load(path)
        if st.get("rehearsal"):
            continue  # the rehearsal is never an input
        if not any(a.get("collector_pid") or a.get("run_id") for a in st["arms"].values()):
            continue  # never started (abandoned before its collectors ran): not a day of the attempt
        out.append({"attempt": st["attempt"], "day": st["day"], "repeat": st["repeat"], "dir": path.parent})
    return out


def cmd_manifest(cfg, day, host, args):
    """§5.2 item 12: one manifest, every input by SHA-256, paths relative to the round directory.
    Each day needs its archive, its host samples and every arm's segment report
    (`analyze_cr.py segment <arm dir> --declaration ...`, saved as <arm>.report.json)."""
    root = Path(cfg["round_dir"]).resolve()
    decl = cfg["decl"]

    def ref(path):
        path = Path(path).resolve()
        if not path.is_relative_to(root):
            raise Fail(f"{path} isn't inside the round directory")
        return {"path": path.relative_to(root).as_posix(), "sha256": sha256(path)}
    for e in decl["arms"]:
        if e["image_digest"] != decl["image_digest"]:
            raise Fail("the declaration names more than one image digest")
    for a in decl["attempts"]:
        if any(v != decl["image_digest"] for v in a.get("candidate_digests", {}).values()):
            raise Fail(f"attempt {a['id']}'s candidate digests aren't the round's image")
    comp = Path(cfg["comparator_report"])
    binding = root / BINDING
    comp_run = (load(comp).get("binding") or {}).get("run_id") or (load(binding)["run_id"] if binding.exists() else None)
    run_ids = [comp_run]
    days, repeats = [], {}
    for d in list_days(cfg):
        reports = {}
        for arm in sorted(load(d["dir"] / "state.json")["arms"]):
            p = d["dir"] / f"{arm}.report.json"
            if not p.exists():
                raise Fail(f"{p} is missing: run analyze_cr.py segment for arm {arm} first")
            rep = load(p)
            got = (rep.get("identity") or {}).get("image_digest")
            if (got or "").rsplit("@", 1)[-1] != decl["image_digest"].rsplit("@", 1)[-1]:
                raise Fail(f"{p}'s image digest isn't the round's")
            run_ids.append((rep.get("binding") or {}).get("run_id"))
            reports[arm] = ref(p)
        for name in ("archive.json", "host-samples.json"):
            if not (d["dir"] / name).exists():
                raise Fail(f"{d['dir'] / name} is missing")
        logs = {}
        for arm in sorted(reports):  # the runner's handshake classification is checked against these
            p = d["dir"] / arm / "gluetun.log"
            if not p.exists():
                raise Fail(f"{p} is missing: run `archive` for that day again")
            logs[arm] = ref(p)
        repeats.setdefault((d["attempt"], d["day"]), []).append(d["repeat"])
        days.append({"attempt": d["attempt"], "day": d["day"], "repeat": d["repeat"], "reports": reports,
                     "archive": ref(d["dir"] / "archive.json"), "contention": ref(d["dir"] / "host-samples.json"),
                     "gluetun_logs": logs})
    if None in run_ids or len(set(run_ids)) != len(run_ids):
        raise Fail("run IDs must be present and unique across the manifest, the comparator's included")
    for key, reps in repeats.items():
        if sorted(reps) != list(range(len(reps))):
            raise Fail(f"attempt {key[0]} day {key[1]}: repeats must run 0..k with no gap, got {sorted(reps)}")
    ends_path = root / "attempt-ends.json"
    man = {"schema": 1, "kind": "cr_round1_manifest", "round": decl["round"], "declaration": ref(cfg["declaration"]),
           "comparator": ref(comp), "days": sorted(days, key=lambda d: (d["attempt"], d["day"], d["repeat"]))}
    if ends_path.exists():
        man["attempt_ends"] = load(ends_path)
    if binding.exists():
        man["comparator_binding"] = ref(binding)
    out = root / "manifest.json"
    save(out, man)
    return [f"ok manifest {out} sha256={sha256(out)}"]


def block_device(path):
    """The kernel name of the block device holding `path` (for the declaration's backing_device)."""
    dev = os.stat(path).st_dev
    link = Path(f"/sys/dev/block/{os.major(dev)}:{os.minor(dev)}")
    try:
        return link.resolve(strict=True).name
    except OSError:
        return None


BINDING = "comparator-binding.json"


def comparator_binding(cfg):
    path = Path(cfg["round_dir"]) / BINDING
    return load(path) if path.exists() else None


def comparator_segment(run_dir):
    """`analyze_cr.py segment` on a run directory, in process (the same analyzer the round's
    comparator.json came from)."""
    if str(TOOLS.parent) not in sys.path:
        sys.path.insert(0, str(TOOLS.parent))
    import analyze_cr  # noqa: E402  (loaded only here: the other commands don't need it)
    return analyze_cr.analyze_segment(run_dir)


def cmd_bind_comparator(cfg, day, host, args):
    """Bind a comparator that ran under a collector without round-1 binding fields (runner,
    2026-11-27, §2.19): after its window, from its run directory, with the identities computed
    now under the round's salt, as the arms compute theirs. Written once into the round folder,
    so the manifest carries it. Its run id is `comparator-<run directory name>`."""
    out = Path(cfg["round_dir"]) / BINDING
    if out.exists():
        raise Fail(f"{out} is already written: a binding is never changed")
    run_dir = Path(args.comparator_run or "")
    try:
        run, result = load(run_dir / "run.json"), load(run_dir / "result.json")
    except (OSError, ValueError):
        raise Fail("--comparator-run must name the comparator's finished run directory (run.json and result.json)") from None
    if (run.get("settings") or {}).get("run_kind") != "comparator":
        raise Fail("that run directory isn't a comparator run")
    ended = parse_utc(result.get("ended"))
    if ended is None:
        raise Fail("the comparator's result.json has no `ended`: it hasn't finished")
    try:  # runner, 2026-11-27: an end before its window's end is an interrupted comparator
        end = parse_utc((load(cfg["comparator_report"]).get("timeline") or {}).get("window_end_utc"))
    except (OSError, ValueError):
        end = None
    if end is None:
        raise Fail("the comparator's report (comparator_report) isn't there or has no timeline.window_end_utc")
    if ended < end:
        raise Fail(f"the comparator ended at {iso(ended)}, before its window's end {iso(end)}: it was interrupted")
    # Runner review (A): the round's comparator.json must be this run's report, or the binding
    # would pair this run's identity with another comparator's evidence. Segment the run
    # directory now and require the same window and binary.
    try:
        report = load(cfg["comparator_report"])
        seg = comparator_segment(run_dir)
    except (OSError, ValueError, KeyError, TypeError):
        raise Fail("the comparator's run directory can't be segmented (analyze_cr.py segment)") from None
    sha = (report.get("identity") or {}).get("binary_sha256")
    if seg.get("kind") != "comparator" or seg.get("timeline") != report.get("timeline") \
            or seg.get("window_hours") != report.get("window_hours") \
            or not sha or (seg.get("identity") or {}).get("binary_sha256") != sha:
        raise Fail("that run directory isn't the comparator the round's comparator.json was made from "
                   "(its window or binary differs)")
    run_id = "comparator-" + run_dir.resolve().name
    if not collect.RUN_ID.fullmatch(run_id):
        raise Fail("the comparator's run directory name makes no valid run id (letters, digits, _ . : -)")
    key = salt(cfg)
    machine = collect.host_key()
    if machine is None:
        raise Fail("can't read /etc/machine-id")
    try:
        uplink = collect.salted_hash(key, "ip", host.uplink_ip(cfg["ip_echo_url"]))
    except (OSError, ValueError, UnicodeError):
        raise Fail("can't read the uplink's address outside the tunnels") from None
    binding = {"schema": 1, "kind": "cr_round1_comparator_binding", "run_id": run_id,
               "host_identity": collect.salted_hash(key, "host", machine),
               "network_identity": {"uplink_hash": uplink, "provider": cfg.get("vpn_provider", "pia"),
                                    "protocol": cfg.get("vpn_protocol", "wireguard")},
               "ended_utc": iso(ended), "bound_utc": iso(host.now()), "bound_after_window": True}
    save(out, binding)
    return [f"ok comparator_bound run_id={run_id} ended={iso(ended)}",
            "the declaration's comparator.run_id must name this run id"]


def cmd_identity(cfg, day, host, args):
    """What the runner needs to write the declaration: salted hashes and kernel names only."""
    key = salt(cfg)
    machine = collect.host_key()
    if machine is None:
        raise Fail("can't read /etc/machine-id")
    try:
        uplink = collect.salted_hash(key, "ip", host.uplink_ip(cfg["ip_echo_url"]))
    except (OSError, ValueError, UnicodeError):
        raise Fail("can't read the uplink's address outside the tunnels") from None
    rc, out = host.run(["ip", "-4", "route", "get", "1.1.1.1"], timeout=10)
    m = re.search(r" dev ([A-Za-z0-9_.-]{1,15}) ", out or "")
    return [f"host_identity {collect.salted_hash(key, 'host', machine)}", f"uplink_hash {uplink}",
            f"backing_device {block_device(cfg['data_volume'])}", f"uplink_interface {m[1] if m else 'unknown'}"]


COMMANDS = {"bind-comparator": cmd_bind_comparator, "stop": cmd_stop, "identity": cmd_identity, "check-endpoints": cmd_check_endpoints, "check-start": cmd_check_start, "leak": cmd_leak, "up": cmd_up, "bring-up": cmd_bring_up,
            "collect": cmd_collect, "watch": cmd_watch, "archive": cmd_archive, "end-attempt": cmd_end_attempt,
            "manifest": cmd_manifest}


def build_parser():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("command", choices=sorted(COMMANDS))
    p.add_argument("config")
    p.add_argument("--day-dir", help="This day's directory under the round directory")
    p.add_argument("--attempt")
    p.add_argument("--day", type=int, choices=(1, 2))
    p.add_argument("--repeat", type=int, help="A new day's repeat number (default 0); an existing day's is stored")
    p.add_argument("--arm")
    p.add_argument("--phase", choices=("before", "after", "retest"))
    p.add_argument("--host-level", action="store_true", help="A leak failure with a host-level cause stops every arm")
    p.add_argument("--reason", help="Why the attempt ended (end-attempt)")
    p.add_argument("--comparator-run", help="The comparator's finished run directory (bind-comparator)")
    p.add_argument("--rehearsal", action="store_true", help="The rehearsal's day (leak, bring-up): never in the manifest")
    return p


def main(argv=None, host=None):
    args = build_parser().parse_args(argv)
    host = host or Host()
    try:
        cfg = load_config(args.config)
        needs_day = args.command not in ("end-attempt", "manifest", "check-endpoints", "identity", "bind-comparator")
        if cfg["decl"] is None and args.command not in ("identity", "check-endpoints", "bind-comparator"):
            raise Fail(f"the declaration ({cfg['declaration']}) isn't there yet: the runner writes it")
        if needs_day and not args.day_dir:
            raise Fail("--day-dir is required")
        if args.command == "leak" and (not args.arm or not args.phase):
            raise Fail("leak needs --arm and --phase")
        if args.command == "end-attempt" and (not args.attempt or not args.reason):
            raise Fail("end-attempt needs --attempt and --reason")
        day = Day(cfg, args.day_dir) if needs_day else None
        for line in COMMANDS[args.command](cfg, day, host, args):
            print(line)
        return 0
    except Fail as error:
        for line in getattr(error, "lines", []):
            print(line)
        print(f"fail {error}")
        return 1
    except SystemExit as error:  # the collector's own refusals (the salt file)
        print(f"fail {error}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
