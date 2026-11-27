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
  leak         record an arm's leak-test result from its output. A failure stops that arm's
               Lodestar (every arm's with --host-level) until a later passing re-test
  up           start each arm's stack (gluetun and Lodestar) with its declared settings
  bring-up     read each arm's egress inside its own namespace and write the exit assignment
               (X, Y, Z to salted hashes); refuses two arms on one exit, or an exit that is
               the uplink
  collect      start each arm's collector (without --stop) and the 10 s host sampler, detached
  watch        scrape each arm's health and counters every minute; once every collector has
               closed, keep every healthy arm crawling until the latest final boundary + 2 min,
               then stop every stack and the sampler, and write the day archive
  archive      (re)write the day archive from what's recorded
  end-attempt  record an attempt that ends short of its declared days (the manifest's attempt_ends)
  identity     the host's salted identity, the uplink's hash, the /data device and the uplink
               interface, for the runner's declaration
  manifest     the round's manifest: every input with its SHA-256, checked for one image
               digest, unique run IDs and contiguous repeats

CONFIG is the round's coordinator file (JSON; the run sheet has the template). Everything it
records is aggregates and salted hashes: no address, hostname, credential or salt. Python 3.10+,
standard library only; root for the namespace reads (egress) and Docker.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOLS = HERE.parents[2] / "docs" / "soak" / "tools" / "linux"
sys.path.insert(0, str(TOOLS))
import collect  # noqa: E402  (the collector's hash, namespace lookup and switch table)

SCRAPE_SECONDS = 60
OVERHANG_SECONDS = 120  # §5.2 item 11: the latest final boundary scrape of the day + 2 min
SPACING = timedelta(hours=12)  # rule 2.2: after the comparator's window end
GAP_SECONDS = 180  # rule 6.6: two scrapes further apart than this are missing evidence
DISK_MARGIN = 1.20
ARM = re.compile(r"^[A-Z]$")
EXIT_LABELS = ("X", "Y", "Z")
STACK_ENV = {"LODESTAR_ARM_SUFFIX", "LODESTAR_UI_PORT"}
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))
# Collector reasons that say the arm's own process failed (§6.3 class 2); the runner's
# diagnosis with a fresh reviewer decides, so they're recorded `pending`.
PRODUCT_REASONS = ("process_exited", "three_consecutive_health_failures", "process_memory_over_1GiB_for_five_samples",
                   "rss_over_1GiB_for_five_samples", "active_peer_requests_without_completion_for_five_minutes",
                   "three_consecutive_metrics_failures", "three_consecutive_capacity_violations")
HOST_REASONS = ("disk_reserve", "disk_read_failures", "storage")


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


class Host:
    """Everything the coordinator does to the host, so the tests can replace it."""

    def __init__(self):
        self.now = utc_now

    def run(self, argv, env=None, timeout=120):
        try:
            done = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, check=False,
                                  env=dict(os.environ, **(env or {})))
        except (OSError, subprocess.SubprocessError):
            return None, ""
        return done.returncode, done.stdout

    def spawn(self, argv, log, env=None):
        """A detached process (its own session), its output to `log`; returns its PID."""
        with open(log, "ab") as out:
            proc = subprocess.Popen(argv, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                    start_new_session=True, env=dict(os.environ, **(env or {})))
        return proc.pid

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

    def sleep(self, seconds):
        time.sleep(seconds)


# ------------------------------------------------------------------ the round's config
def load_config(path):
    cfg = load(path)
    if not (isinstance(cfg, dict) and cfg.get("schema") == 1 and cfg.get("kind") == "cr_round1_coordinator"):
        raise Fail("config: schema 1, kind cr_round1_coordinator")
    base = Path(path).resolve().parent
    for key in ("declaration", "comparator_report", "salt_file", "data_volume", "round_dir"):
        if not isinstance(cfg.get(key), str):
            raise Fail(f"config: {key}")
        cfg[key] = str((base / cfg[key]).resolve()) if not os.path.isabs(cfg[key]) else cfg[key]
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
            raise Fail(f"config: arm {arm}'s tunnel_target is the IPv4 address its probe pings (declared before the rehearsal)")
        if a["port"] in ports or a["suffix"] in suffixes:
            raise Fail("config: every arm has its own port and suffix")
        ports.add(a["port"])
        suffixes.add(a["suffix"])
    cfg.setdefault("ip_echo_url", collect.DEFAULT_IP_ECHO)
    cfg.setdefault("expected_growth_bytes", {})
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
    return {"LODESTAR_ARM_SUFFIX": a["suffix"], "LODESTAR_UI_PORT": str(a["port"]),
            "LODESTAR_IMAGE": cfg.get("image") or "unused", "DOCKER_GID": "unused"}


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
        if self.state is not None:
            want = (self.state["attempt"], self.state["day"], self.state["repeat"])
            if want != (attempt, day, repeat):
                raise Fail(f"this day directory is {want}, not {(attempt, day, repeat)}")
            return self.state
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
    """The leak test's verdict from its output: its last line is `ok leak_test` or not."""
    lines = [ln.strip() for ln in (text or "").splitlines() if ln.strip()]
    return "pass" if lines and lines[-1] == "ok leak_test" else "fail"


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
    if s.get("rehearsal"):  # §8 item 5: the rehearsal runs inside the 12 h, from stores deleted after
        lines.append("skip spacing (rehearsal: not a measurement)")
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
    lines.append("ok leak_tests_before ok fresh_dirs")
    return lines


def stop_arm(cfg, host, arm):
    rc, _ = host.run(["docker", "stop", "-t", "60", lodestar(cfg, arm)], timeout=120)
    return rc == 0


def cmd_leak(cfg, day, host, args):
    """Record a leak test (§5.2 item 3). A failure is an evidenced infrastructure fault (§6.3
    class 1): the arm stops crawling (every arm with --host-level) until a passing re-test."""
    s = day.init(args.attempt, args.day, args.repeat, args.rehearsal) if day.state is None else day.state
    if args.arm not in s["arms"]:
        raise Fail(f"arm {args.arm} isn't in this day")
    text = Path(args.log).read_text(encoding="utf-8", errors="replace")
    result = leak_verdict(text)
    started = any(a.get("collector_pid") for a in s["arms"].values())
    s["arms"][args.arm]["leak_tests"].append({"utc": iso(host.now()), "phase": args.phase, "result": result,
                                              "after_start": started and args.phase != "after",
                                              "log_sha256": hashlib.sha256(text.encode()).hexdigest()})
    lines = [f"{'ok' if result == 'pass' else 'fail'} leak_test arm={args.arm} phase={args.phase}"]
    if result == "fail":
        hit = sorted(s["arms"]) if args.host_level else [args.arm]
        for arm in hit:
            s["arms"][arm]["blocked"] = True
            stopped = stop_arm(cfg, host, arm)
            lines.append(f"{'ok' if stopped else 'fail'} crawling_stopped arm={arm}")
        s["events"].append({"class": "infrastructure", "cause": "leak_test_failed", "arms": [] if args.host_level else hit,
                            "utc": iso(host.now()), "diagnosis": None})
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
    if cfg["image"].rsplit("@", 1)[1] != cfg["decl"]["image_digest"]:
        raise Fail("config: image's digest isn't the declaration's image_digest")
    lines = []
    for arm in day.arms():
        env = dict(stack_env(cfg, arm), **arm_switch_env(day.entry(arm)))
        rc, _ = host.run(compose(cfg, arm, "--profile", "block", "up", "-d", "lodestar"), env=env, timeout=300)
        if rc != 0:
            raise Fail(f"arm {arm}'s stack didn't start")
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
    exits = {s["arms"][arm]["exit"]: readings[arm]["hash"] for arm in day.arms()}
    if s["exits"] and s["exits"] != exits:
        raise Fail("the exits changed since this day's bring-up")
    s["exits"] = exits
    for arm in day.arms():
        s["arms"][arm]["egress"].append(readings[arm])
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
    if s.get("rehearsal"):  # the 2 h rehearsal (§6.3): marked in run.json, never a measurement
        argv += ["--rehearsal", "--baseline-hours", "2"]
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
    lines = []
    for arm in day.arms():
        st = s["arms"][arm]
        if st["blocked"]:
            raise Fail(f"arm {arm} is stopped after a failed leak test")
        if st.get("collector_pid"):
            continue
        run_id = run_id_for(day, arm)
        st["run_id"] = run_id
        st["collector_pid"] = host.spawn(collector_argv(cfg, day, arm, run_id), str(day.dir / f"collector-{arm}.log"))
        lines.append(f"ok collector arm={arm} run_id={run_id}")
    if not s.get("sampler_pid"):
        argv = [sys.executable, "-I", str(TOOLS / "host_samples.py"), "run", "--out", str(day.dir / "host-samples.jsonl"),
                "--device", cfg["decl"]["backing_device"], "--uplink", cfg["uplink_interface"]]
        for arm in day.arms():
            if cfg["arms"][arm].get("tunnel_target"):
                argv += ["--tunnel", f"{arm}={gluetun(cfg, arm)}:{cfg['arms'][arm]['tunnel_target']}"]
        s["sampler_pid"] = host.spawn(argv, str(day.dir / "host-samples.log"))
        lines.append("ok host_sampler")
    day.save()
    return lines


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


def active_span(rows, gap=GAP_SECONDS):
    """§5.2 item 11's activity: from the first scrape that is healthy with counters rising over
    the one before, through the last of the unbroken run that follows. A failed scrape, a
    counter that stops rising, or a gap over 180 s ends it, and it never restarts."""
    start = end = prev = None
    for r in sorted(rows, key=lambda r: r["utc"]):
        t = parse_utc(r["utc"])
        rising = (prev is not None and r.get("queries_out") is not None and prev.get("queries_out") is not None
                  and r["queries_out"] > prev["queries_out"])
        ok = r.get("health") == 200 and rising and (t - parse_utc(prev["utc"])).total_seconds() <= gap
        if start is None:
            if ok:
                start = end = t
        elif ok:
            end = t
        else:
            break
        prev = r
    return start, end


def collector_state(run_dir):
    """A collector's progress: whether it closed, why, and its final boundary scrape's time."""
    run_dir = Path(run_dir)
    out = {"closed": (run_dir / "result.json").exists(), "reason": None, "final_boundary_utc": None,
           "window_hours": None, "egress": []}
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
        if isinstance(row.get("egress"), dict) and row["egress"].get("read") == "ok":
            out["egress"].append({"utc": row["time"], "hash": row["egress"]["hash"]})
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


def classify_close(arm, st):
    """An early close, as an event (§6.3). A product failure is `pending` until the runner's
    diagnosis; a host guard is infrastructure; anything else is `unclassified`, which
    gate-round reads as an unknown event, so the day fails closed until the runner classes it."""
    reason = st["reason"]
    if reason == "baseline_window_complete" or reason is None:
        return None
    if any(reason.startswith(p) for p in PRODUCT_REASONS):
        return {"class": "product", "arms": [arm], "cause": reason, "diagnosis": "pending"}
    if any(reason.startswith(p) for p in HOST_REASONS):
        return {"class": "infrastructure", "arms": [arm], "cause": reason, "diagnosis": None}
    return {"class": "unclassified", "arms": [arm], "cause": reason}


def cmd_watch(cfg, day, host, args, max_loops=None):
    s = day.require()
    log = day.dir / "activity.jsonl"
    loops, cache = 0, {}
    while True:
        with log.open("a", encoding="utf-8") as fh:
            for arm in day.arms():
                fh.write(json.dumps(scrape(cfg, host, arm), separators=(",", ":")) + "\n")
        states = {arm: cached_state(cache, day.dir / arm) for arm in day.arms()}
        when = shutdown_at(states)
        if when is not None and (when == "now" or host.now() >= when + timedelta(seconds=OVERHANG_SECONDS)):
            break
        loops += 1
        if max_loops is not None and loops >= max_loops:
            return ["ok watch paused (test bound)"]
        host.sleep(SCRAPE_SECONDS)
    key = salt(cfg)
    lines = []
    for arm in day.arms():  # egress through the last boundary, before anything stops
        r = read_egress(cfg, host, arm, key)
        if r is not None:
            s["arms"][arm]["egress"].append(r)
        else:
            s["notes"].append(f"{iso(host.now())} arm {arm}: egress unreadable at shutdown")
    s["shutdown_utc"] = iso(host.now())
    for arm in day.arms():
        rc, _ = host.run(compose(cfg, arm, "stop"), env=stack_env(cfg, arm), timeout=300)
        lines.append(f"{'ok' if rc == 0 else 'fail'} stack_stopped arm={arm}")
    if s.get("sampler_pid"):
        host.signal(s["sampler_pid"])
        lines.append("ok host_sampler_stopped")
    day.save()
    lines += cmd_archive(cfg, day, host, args)
    return lines


def cmd_archive(cfg, day, host, args):
    """The day archive gate-round reads: exits, and per arm its egress, activity and leak tests,
    and the day's events."""
    s = day.require()
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
        start, end = active_span([r for r in rows if r.get("arm") == arm])
        egress = sorted(st["egress"] + col["egress"], key=lambda r: r["utc"])
        arms[arm] = {"exit": st["exit"], "run_id": st.get("run_id"), "egress": egress,
                     "active_from_utc": iso(start) if start else None, "active_through_utc": iso(end) if end else None,
                     "activity_scrapes": sum(1 for r in rows if r.get("arm") == arm),
                     "collector": {"closed": col["closed"], "reason": col["reason"],
                                   "final_boundary_utc": col["final_boundary_utc"]},
                     "leak_test": leak_summary(st["leak_tests"]), "leak_tests": st["leak_tests"]}
        ev = classify_close(arm, col)
        if ev is not None and not any(e.get("cause") == ev["cause"] and e.get("arms") == ev["arms"] for e in events):
            events.append(ev)
    archive = {"schema": 1, "round": s["round"], "attempt": s["attempt"], "day": s["day"], "repeat": s["repeat"],
               "exits": dict(s["exits"]), "arms": arms, "events": events, "shutdown_utc": s.get("shutdown_utc"),
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
    comp_run = (load(comp).get("binding") or {}).get("run_id")
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
            if got != decl["image_digest"]:
                raise Fail(f"{p}'s image digest isn't the round's")
            run_ids.append((rep.get("binding") or {}).get("run_id"))
            reports[arm] = ref(p)
        for name in ("archive.json", "host-samples.json"):
            if not (d["dir"] / name).exists():
                raise Fail(f"{d['dir'] / name} is missing")
        repeats.setdefault((d["attempt"], d["day"]), []).append(d["repeat"])
        days.append({"attempt": d["attempt"], "day": d["day"], "repeat": d["repeat"], "reports": reports,
                     "archive": ref(d["dir"] / "archive.json"), "contention": ref(d["dir"] / "host-samples.json")})
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
    out = root / "manifest.json"
    save(out, man)
    return [f"ok manifest {out} sha256={sha256(out)}"]


def block_device(path):
    """The kernel name of the block device holding `path` (for the declaration's backing_device)."""
    dev = os.stat(path).st_dev
    link = Path(f"/sys/dev/block/{os.major(dev)}:{os.minor(dev)}")
    try:
        return link.resolve().name
    except OSError:
        return None


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


COMMANDS = {"identity": cmd_identity, "check-endpoints": cmd_check_endpoints, "check-start": cmd_check_start, "leak": cmd_leak, "up": cmd_up, "bring-up": cmd_bring_up,
            "collect": cmd_collect, "watch": cmd_watch, "archive": cmd_archive, "end-attempt": cmd_end_attempt,
            "manifest": cmd_manifest}


def build_parser():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("command", choices=sorted(COMMANDS))
    p.add_argument("config")
    p.add_argument("--day-dir", help="This day's directory under the round directory")
    p.add_argument("--attempt")
    p.add_argument("--day", type=int, choices=(1, 2))
    p.add_argument("--repeat", type=int, default=0)
    p.add_argument("--arm")
    p.add_argument("--phase", choices=("before", "after", "retest"))
    p.add_argument("--log", help="The leak test's output (leak)")
    p.add_argument("--host-level", action="store_true", help="A leak failure with a host-level cause stops every arm")
    p.add_argument("--reason", help="Why the attempt ended (end-attempt)")
    p.add_argument("--rehearsal", action="store_true", help="The rehearsal's day (leak, bring-up): never in the manifest")
    return p


def main(argv=None, host=None):
    args = build_parser().parse_args(argv)
    host = host or Host()
    try:
        cfg = load_config(args.config)
        needs_day = args.command not in ("end-attempt", "manifest", "check-endpoints", "identity")
        if cfg["decl"] is None and args.command not in ("identity", "check-endpoints"):
            raise Fail(f"the declaration ({cfg['declaration']}) isn't there yet: the runner writes it")
        if needs_day and not args.day_dir:
            raise Fail("--day-dir is required")
        if args.command in ("bring-up",) or (args.command == "leak" and not (Path(args.day_dir) / "state.json").exists()):
            if not args.attempt or args.day is None:
                raise Fail("--attempt and --day name the day")
        if args.command == "leak" and (not args.arm or not args.phase or not args.log):
            raise Fail("leak needs --arm, --phase and --log")
        if args.command == "end-attempt" and (not args.attempt or not args.reason):
            raise Fail("end-attempt needs --attempt and --reason")
        day = Day(cfg, args.day_dir) if needs_day else None
        for line in COMMANDS[args.command](cfg, day, host, args):
            print(line)
        return 0
    except Fail as error:
        print(f"fail {error}")
        return 1
    except SystemExit as error:  # the collector's own refusals (the salt file)
        print(f"fail {error}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
