#!/usr/bin/env python3
"""Linux/Docker soak collector: aggregate evidence for analyze_cr0.py.

Observes a running Lodestar instance (a Docker container or a native process)
and writes the evidence contract that ../analyze_cr0.py reads: run.json,
samples.jsonl, metrics/NNNNN.prom, status.json, disk.jsonl and result.json.
It mirrors cr0-soak.ps1's fields and semantics. Python 3.10+, standard library
only.

Safety: read-only towards the instance. It signals or stops nothing unless
--stop is given, and then only after re-verifying the instance's identity.
It refuses a non-empty run directory and writes atomically. Evidence is
aggregates only: no logs, torrent names, infohashes, peer addresses or tokens.
Anything this host cannot provide is left out of the row, never invented.
"""

import argparse
import hashlib
import http.client
import json
import math
import os
import platform
import re
import shutil
import signal
import socket
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

SCHEMA = 2
SAMPLE_SECONDS = 60.0
WATCH_SECONDS = 10.0
UNVERIFIED_LIMIT_SECONDS = 180.0  # three missed samples
GIB = 2**30
# Metrics every sample must carry (cr0-soak.ps1); a missing one fails the sample.
REQUIRED = (
    "start_time_seconds", "torrents", "dht_sample_suppression_seconds",
    "dht_sample_cooling", "dht_sample_inflight", "dht_sample_frontier",
    "dht_peer_active", "dht_peer_queued", "dht_peer_admitted_total",
    "dht_peer_rejected_total", "dht_peer_cancelled_total", "dht_lookup_scheduled",
    "dht_lookup_deadlines_total", "dht_lookups_completed_total", "dht_outstanding",
    "fetch_lookups", "fetch_active", "fetch_pending",
)
# Counters analyze_cr0.py reads from snapshots. test_collect.py pins this to its list.
COUNTERS = tuple("dht_" + name + "_total" for name in (
    "peer_admitted", "peer_rejected", "peer_cancelled", "peer_rejection_reasons",
    "peer_ack_failed", "peer_command_full", "peer_command_closed", "lookups_completed",
    "lookup_deadlines", "sample_stale", "sample_deferred", "queries_out",
    "responses_in", "timeouts", "packets_in", "packets_out", "harvested",
    "harvest_dropped",
)) + tuple(name + "_total" for name in (
    "fetch_attempts", "fetch_success", "fetch_failures", "fetch_lost",
    "fetch_finder_full", "fetch_finder_closed", "fetch_lookup_unaccepted",
    "fetch_retry_unaccepted", "fetch_evicted_service",
    "writer_stored", "writer_errors", "writer_transactions", "search_errors",
))
KEEP = frozenset(REQUIRED) | frozenset(COUNTERS)
NUMBER = r"[0-9]+(?:\.[0-9]+)?"
SERIES = re.compile(r"^lodestar_([a-z_0-9]+)(\{[^{}\r\n]*\})? (" + NUMBER + r")\r?$")
SAFE_LABELS = re.compile(r'^\{[a-z_]+="[a-z_0-9]{1,24}"(,[a-z_]+="[a-z_0-9]{1,24}")*\}$')
FAMILY = re.compile(r'(?:^|[{,])(?:ip_)?family="(v4|v6|ipv4|ipv6)"')
COMMIT = re.compile(r"^[0-9a-f]{7,40}$")


def utc_now():
    return datetime.now(timezone.utc)


def iso(stamp):
    return stamp.isoformat().replace("+00:00", "Z")


def read_metric(text, name):
    match = re.search(r"(?m)^lodestar_" + re.escape(name) + " (" + NUMBER + r")\r?$", text)
    if not match:
        raise ValueError(f"Missing metric: {name}")
    return float(match[1])


def sanitize_snapshot(text):
    """Keep allowlisted series with plain enum-like labels; drop everything else."""
    seen, out = set(), []
    for line in text.splitlines():
        match = SERIES.fullmatch(line)
        if not match or match[1] not in KEEP:
            continue
        name, labels, value = match.groups()
        if labels and not SAFE_LABELS.fullmatch(labels):
            continue
        if (name, labels) in seen:
            continue
        seen.add((name, labels))
        out.append(f"lodestar_{name}{labels or ''} {value}")
    return "\n".join(out) + "\n"


def ip_family_counters(text):
    """Per-family counter totals, only for series the metrics label by IP family."""
    found = {}
    for line in text.splitlines():
        match = SERIES.fullmatch(line)
        if not match or match[1] not in COUNTERS or not match[2]:
            continue
        fam = FAMILY.search(match[2])
        if fam:
            key = "v4" if fam[1] in ("v4", "ipv4") else "v6"
            bucket = found.setdefault(key, {})
            bucket[match[1]] = bucket.get(match[1], 0) + float(match[3])
    return found


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_text(path):
    try:
        return Path(path).read_text(encoding="utf-8")
    except (OSError, ValueError):
        return None


def read_int(path):
    text = read_text(path)
    try:
        return int(text.strip())
    except (AttributeError, ValueError):
        return None


def read_kv(path):
    text = read_text(path)
    if text is None:
        return None
    out = {}
    for line in text.splitlines():
        key, _, value = line.partition(" ")
        try:
            out[key] = int(value)
        except ValueError:
            pass
    return out


class Verdict:
    """Result of an identity check: ok, unverified, exited or changed."""

    def __init__(self, kind, reason=None, exit_code=None, **detail):
        self.kind, self.reason, self.exit_code, self.detail = kind, reason, exit_code, detail


OK = Verdict("ok")


# ---------------------------------------------------------------- Docker mode
class UnixHTTPConnection(http.client.HTTPConnection):
    def __init__(self, path, timeout):
        super().__init__("localhost", timeout=timeout)
        self._path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self._path)


class DockerAPI:
    """Minimal Docker Engine API client over the unix socket."""

    def __init__(self, socket_path, timeout=5):
        self.socket_path, self.timeout = socket_path, timeout

    def request(self, method, path, timeout=None):
        conn = UnixHTTPConnection(self.socket_path, timeout or self.timeout)
        try:
            conn.request(method, path)
            response = conn.getresponse()
            body = response.read()
            try:
                doc = json.loads(body) if body else None
            except ValueError:
                doc = None
            return response.status, doc
        finally:
            conn.close()

    def get(self, path):
        return self.request("GET", path)

    def post(self, path, timeout=None):
        return self.request("POST", path, timeout)


class DockerInstance:
    kind = "docker"

    def __init__(self, api, container, cgroup_root="/sys/fs/cgroup", cgroup_path=None,
                 stop_timeout=60, clock=time.monotonic, sleep=time.sleep):
        self.api, self.container = api, container
        self.cgroup_root, self.cgroup_path = Path(cgroup_root), cgroup_path
        self.stop_timeout, self.clock, self.sleep = stop_timeout, clock, sleep
        self.id = self.started_at = self.identity = None
        self.doc = {}
        self.parent_id = self.parent_identity = self.parent_doc = None
        self.parent_changes = 0
        self._cgroup = None

    def _inspect(self, name):
        status, doc = self.api.get("/containers/" + urllib.parse.quote(name, safe="") + "/json")
        return status, doc

    def attach(self):
        try:
            status, doc = self._inspect(self.container)
        except OSError:
            raise SystemExit("Cannot reach the Docker API socket.") from None
        if status != 200 or not isinstance(doc, dict) or not doc["State"].get("Running"):
            raise SystemExit("Container not found or not running.")
        self.doc = doc
        self.id = doc["Id"]
        self.started_at = doc["State"]["StartedAt"]
        self.identity = f"{self.id}:{self.started_at}"
        self._read_parent()

    def _read_parent(self):
        mode = (self.doc.get("HostConfig") or {}).get("NetworkMode") or ""
        if not mode.startswith("container:"):
            return
        try:
            status, doc = self._inspect(mode.split(":", 1)[1])
        except OSError:
            return
        if status == 200 and isinstance(doc, dict):
            identity = f"{doc['Id']}:{doc['State'].get('StartedAt')}"
            if self.parent_identity not in (None, identity):
                self.parent_changes += 1
            self.parent_id, self.parent_identity, self.parent_doc = doc["Id"], identity, doc

    def network(self):
        mode = (self.doc.get("HostConfig") or {}).get("NetworkMode") or "unknown"
        info = {"mode": "container" if mode.startswith("container:") else mode,
                "host_network": mode == "host", "gluetun_network_parent": False}
        if self.parent_doc is not None:
            image = str((self.parent_doc.get("Config") or {}).get("Image", ""))
            info["gluetun_network_parent"] = "gluetun" in image.lower()
            info["network_parent_container_id"] = self.parent_id
        return info

    def describe(self):
        config = self.doc.get("Config") or {}
        info = {"container_id": self.id, "image": config.get("Image"),
                "image_id": self.doc.get("Image"), "restart_count": self.doc.get("RestartCount")}
        image_hex = str(self.doc.get("Image", "")).removeprefix("sha256:")
        if re.fullmatch(r"[0-9a-f]{64}", image_hex):
            info["sha256"], info["sha256_kind"] = image_hex, "image_id"
        try:
            status, image = self.api.get("/images/" + urllib.parse.quote(str(self.doc.get("Image")), safe="") + "/json")
            if status == 200 and image.get("RepoDigests"):
                info["image_digest"] = image["RepoDigests"][0]
        except (OSError, AttributeError, KeyError):
            pass
        return info

    def check(self):
        try:
            status, doc = self._inspect(self.id)
        except OSError:
            return Verdict("unverified")
        if status == 404:
            return Verdict("exited", "process_exited_container_removed")
        if status != 200 or not isinstance(doc, dict):
            return Verdict("unverified")
        state = doc.get("State") or {}
        if doc.get("Id") != self.id or state.get("StartedAt") != self.started_at:
            return Verdict("changed", "process_exited_identity_changed",
                           observed=f"{doc.get('Id')}:{state.get('StartedAt')}")
        if not state.get("Running"):
            code = state.get("ExitCode")
            return Verdict("exited", f"process_exited_{code}", code, oom_killed=bool(state.get("OOMKilled")))
        self.doc = doc
        self._read_parent()
        return OK

    def row_extra(self):
        state = self.doc.get("State") or {}
        extra = {"restart_count": self.doc.get("RestartCount")}
        health = (state.get("Health") or {}).get("Status")
        if health in ("healthy", "unhealthy", "starting"):
            extra["container_health"] = health
        if self.parent_identity is not None:
            extra["network_parent_identity"] = self.parent_identity
            extra["network_parent_running"] = bool((self.parent_doc.get("State") or {}).get("Running"))
        return {k: v for k, v in extra.items() if v is not None}

    @property
    def process_id(self):
        return (self.doc.get("State") or {}).get("Pid") or 0

    def _find_cgroup(self):
        if self._cgroup is None:
            candidates = [Path(self.cgroup_path)] if self.cgroup_path else [
                self.cgroup_root / "system.slice" / f"docker-{self.id}.scope",
                self.cgroup_root / "docker" / self.id]
            self._cgroup = next((c for c in candidates if (c / "memory.current").exists()), False)
        return self._cgroup

    def resources(self):
        out, cgroup = {}, self._find_cgroup()
        if cgroup:
            current, stat = read_int(cgroup / "memory.current"), read_kv(cgroup / "memory.stat")
            if current is not None and stat and "inactive_file" in stat:
                out["working_set_bytes"] = max(current - stat["inactive_file"], 0)  # docker stats
            if stat and "anon" in stat:
                out["private_bytes"] = stat["anon"]
            cpu = read_kv(cgroup / "cpu.stat")
            if cpu and "usage_usec" in cpu:
                out["cpu_seconds"] = cpu["usage_usec"] / 1e6
            threads = read_int(cgroup / "pids.current")
            if threads is not None:
                out["threads"] = threads
            return out
        try:  # Docker API fallback when the host cgroup tree is not mounted
            status, stats = self.api.get(f"/containers/{self.id}/stats?stream=false&one-shot=true")
            if status != 200 or not isinstance(stats, dict):
                return out
            memory = stats.get("memory_stats") or {}
            inner = memory.get("stats") or {}
            inactive = inner.get("inactive_file", inner.get("total_inactive_file"))
            if isinstance(memory.get("usage"), int) and isinstance(inactive, int):
                out["working_set_bytes"] = max(memory["usage"] - inactive, 0)
            if isinstance(inner.get("anon"), int):
                out["private_bytes"] = inner["anon"]
            total = ((stats.get("cpu_stats") or {}).get("cpu_usage") or {}).get("total_usage")
            if isinstance(total, int):
                out["cpu_seconds"] = total / 1e9
            pids = (stats.get("pids_stats") or {}).get("current")
            if isinstance(pids, int):
                out["threads"] = pids
        except (OSError, ValueError):
            pass
        return out

    def stop(self):
        outcome = new_shutdown()
        started = self.clock()
        verdict = self.check()
        if verdict.kind == "exited":
            outcome.update(exit_confirmed=True, exit_code=verdict.exit_code,
                           stop_mode=f"already_exited_{verdict.exit_code}")
        elif verdict.kind != "ok":
            outcome["errors"].append("process_identity_mismatch" if verdict.kind == "changed"
                                     else "process_identity_unverified")
        else:
            try:
                # The stop call blocks until the container exits, so it outlives the default timeout.
                status, _ = self.api.post(f"/containers/{self.id}/stop?t={self.stop_timeout}",
                                          self.stop_timeout + 30)
                outcome["signal_delivered"] = status == 204  # 304: it had already stopped
            except OSError:
                outcome["errors"].append("stop_failed")
            deadline = self.clock() + self.stop_timeout + 30
            while True:
                verdict = self.check()
                if verdict.kind == "exited":
                    outcome.update(exit_confirmed=True, exit_code=verdict.exit_code,
                                   stop_mode=f"exited_{verdict.exit_code}")
                    break
                if verdict.kind == "changed" or self.clock() >= deadline:
                    outcome["errors"].append("process_cleanup_failed")
                    break
                self.sleep(1)
        outcome["shutdown_seconds"] = self.clock() - started
        return outcome


# ---------------------------------------------------------------- native mode
def new_shutdown():
    return {"stop_mode": "unknown", "exit_confirmed": False, "signal_delivered": False,
            "exit_code": None, "errors": []}


class NativeInstance:
    kind = "native"

    def __init__(self, pid, proc_root="/proc", clock=time.monotonic, sleep=time.sleep,
                 kill=os.kill, clk_tck=None, stop_timeout=60):
        self.pid, self.proc = int(pid), Path(proc_root)
        self.clock, self.sleep, self.kill = clock, sleep, kill
        self.clk_tck = clk_tck or os.sysconf("SC_CLK_TCK")
        self.stop_timeout = stop_timeout
        self.identity = self.start_ticks = self.comm = None

    def _stat(self):
        """(comm, state, start_ticks, cpu_ticks), or None when the pid is gone."""
        text = read_text(self.proc / str(self.pid) / "stat")
        if text is None:
            return None
        head, _, tail = text.rpartition(")")
        fields = tail.split()
        try:
            return (head.partition("(")[2], fields[0], int(fields[19]), int(fields[11]) + int(fields[12]))
        except (IndexError, ValueError):
            return None

    def attach(self):
        stat = self._stat()
        if stat is None or stat[1] in ("Z", "X"):
            raise SystemExit("Process not found or not running.")
        boot = (read_text(self.proc / "sys/kernel/random/boot_id") or "").strip()
        self.comm, self.start_ticks = stat[0], stat[2]
        self.boot = boot
        self.identity = f"{self.pid}:{boot}:{self.start_ticks}"

    def network(self):
        return {"mode": "native_host", "host_network": True, "gluetun_network_parent": False}

    def describe(self):
        info = {"process_name": self.comm}
        try:
            info["sha256"], info["sha256_kind"] = sha256_file(self.proc / str(self.pid) / "exe"), "exe"
        except OSError:
            pass
        return info

    @property
    def process_id(self):
        return self.pid

    def check(self):
        stat = self._stat()
        if stat is None or stat[1] in ("Z", "X"):
            return Verdict("exited", "process_exited_unknown")
        if stat[2] != self.start_ticks:
            return Verdict("changed", "process_exited_identity_changed")
        return OK

    def row_extra(self):
        return {}

    def resources(self):
        out, base = {}, self.proc / str(self.pid)
        status = read_text(base / "status") or ""
        for key, field in (("VmRSS", "working_set_bytes"), ("RssAnon", "private_bytes")):
            match = re.search(rf"(?m)^{key}:\s+(\d+) kB$", status)
            if match:
                out[field] = int(match[1]) * 1024
        match = re.search(r"(?m)^Threads:\s+(\d+)$", status)
        if match:
            out["threads"] = int(match[1])
        stat = self._stat()
        if stat:
            out["cpu_seconds"] = stat[3] / self.clk_tck
        try:
            out["handles"] = len(os.listdir(base / "fd"))
        except OSError:
            pass
        return out

    def stop(self):
        outcome = new_shutdown()
        started = self.clock()
        verdict = self.check()
        if verdict.kind == "exited":
            outcome.update(exit_confirmed=True, stop_mode="already_exited_unknown")
        elif verdict.kind != "ok":
            outcome["errors"].append("process_identity_mismatch")
        else:
            try:
                self.kill(self.pid, signal.SIGTERM)
                outcome["signal_delivered"] = True
            except OSError:
                outcome["errors"].append("signal_failed")
            if self._wait_gone(self.stop_timeout):
                outcome.update(exit_confirmed=True, stop_mode="exited_unknown")
            elif self.check().kind == "ok":  # identity re-verified before the forced stop
                try:
                    self.kill(self.pid, signal.SIGKILL)
                except OSError:
                    outcome["errors"].append("kill_failed")
                outcome["stop_mode"] = "kill_unconfirmed"
                if self._wait_gone(30):
                    outcome.update(exit_confirmed=True, stop_mode="forced_kill")
            else:
                outcome["errors"].append("kill_identity_mismatch")
        outcome["shutdown_seconds"] = self.clock() - started
        return outcome

    def _wait_gone(self, seconds):
        deadline = self.clock() + seconds
        while True:
            if self.check().kind != "ok":
                return True
            if self.clock() >= deadline:
                return False
            self.sleep(1)


# ------------------------------------------------------------------ plumbing
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def http_get(url, timeout=3, limit=8 << 20):
    request = urllib.request.Request(url, headers={"Accept-Encoding": "identity"})
    try:
        with OPENER.open(request, timeout=timeout) as response:
            return response.status, response.read(limit)
    except urllib.error.HTTPError as error:
        return error.code, b""


def statvfs_volumes(paths):
    """Free and total bytes per distinct filesystem under the given paths."""
    seen, out = set(), []
    for path in paths:
        device = os.stat(path).st_dev
        if device in seen:
            continue
        seen.add(device)
        stat = os.statvfs(path)
        out.append({"volume": str(path), "free_bytes": stat.f_bavail * stat.f_frsize,
                    "total_bytes": stat.f_blocks * stat.f_frsize})
    return out


def save_json(value, path):
    temp = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    try:
        with open(temp, "w", encoding="utf-8") as stream:
            stream.write(json.dumps(value, indent=2, allow_nan=False))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp, path)
    finally:
        try:
            temp.unlink()
        except OSError:
            pass


def write_text(path, text):
    temp = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    try:
        with open(temp, "w", encoding="utf-8", newline="\n") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp, path)
    finally:
        try:
            temp.unlink()
        except OSError:
            pass


def append_line(path, line):
    with open(path, "a", encoding="utf-8", newline="\n") as stream:
        stream.write(line + "\n")
        stream.flush()
        os.fsync(stream.fileno())


def guard_reason(volumes, stop_bytes, failures):
    if failures >= 3:
        return "disk_monitor_failed"
    if any(v["free_bytes"] < stop_bytes for v in volumes):
        return "disk_reserve_reached"
    return None


def endpoint_reached(elapsed, end_at, ok, baseline_start):
    return ok and baseline_start is not None and end_at is not None and elapsed >= end_at


class Collector:
    def __init__(self, args, instance, *, clock=time.monotonic, sleep=time.sleep,
                 now=utc_now, fetch=http_get, volumes=statvfs_volumes):
        self.args, self.instance = args, instance
        self.clock, self.sleep, self.now, self.fetch, self.volume_reader = clock, sleep, now, fetch, volumes
        self.root = Path(args.root)
        self.stop_flag = threading.Event()
        self.stop_signal = None
        self.reason = "collector_error"
        self.events = self.root / "events.log"

    # -- control
    def request_stop(self, name="stop"):
        self.stop_signal = self.stop_signal or name
        self.stop_flag.set()

    def note(self, message):
        try:
            append_line(self.events, f"{iso(self.now())} {message}")
        except OSError:
            sys.stderr.write("Soak event log write failed; collection continues.\n")

    def save(self, value, path):
        try:
            save_json(value, path)
        except (OSError, ValueError):
            self.reason = "evidence_write_error"
            raise

    def write(self, path, text, append=False):
        try:
            append_line(path, text) if append else write_text(path, text)
        except OSError:
            self.reason = "evidence_write_error"
            raise

    # -- launch
    def storage_paths(self):
        return [self.root] + [Path(p) for p in self.args.storage_path]

    def prepare(self):
        a = self.args
        if (a.warmup_minutes < 30 or a.baseline_hours < 24) and not a.rehearsal:
            raise SystemExit("Baseline requires at least 30 minutes warmup and 24 measured hours.")
        if not (0 < a.stop_free_bytes < a.warn_free_bytes <= a.min_free_bytes):
            raise SystemExit("Invalid storage guardrails.")
        parsed = urllib.parse.urlsplit(a.url)
        if (parsed.scheme != "http" or not parsed.hostname or parsed.username is not None
                or parsed.path not in ("", "/") or parsed.query or parsed.fragment):
            raise SystemExit("Use a plain http://host:port URL without credentials or path.")
        if not COMMIT.fullmatch(a.commit):
            raise SystemExit("--commit must be a lowercase hex commit id (7 to 40 characters).")
        if self.root.exists() and (not self.root.is_dir() or any(self.root.iterdir())):
            raise SystemExit("Run directory is not empty; use a fresh directory.")
        try:
            self.instance.attach()
        except (OSError, KeyError, TypeError, AttributeError) as error:
            raise SystemExit(f"Cannot attach to the instance ({type(error).__name__}).") from None
        if (a.stop and self.instance.kind == "native" and self.instance.kill is os.kill
                and self.instance.proc.resolve() != Path("/proc").resolve()):
            raise SystemExit("--stop needs the collector's own /proc: identity and signal must share a PID namespace.")
        if a.require_gluetun and not self.instance.network()["gluetun_network_parent"]:
            raise SystemExit("--require-gluetun: the instance's network parent is not a gluetun container.")
        self.root.mkdir(parents=True, exist_ok=True)
        self.volumes_now = self.volume_reader(self.storage_paths())
        if any(v["free_bytes"] < a.min_free_bytes for v in self.volumes_now):
            raise SystemExit("Insufficient free disk space for soak launch.")
        (self.root / "metrics").mkdir()

    def run_json(self):
        a, inst = self.args, self.instance
        stamp = self.now()
        local = stamp.astimezone()
        collector = self.root / "collector.py"
        shutil.copyfile(os.path.abspath(__file__), collector)
        settings = {
            "mode": inst.kind, "http_url": a.url, "warmup_minutes": a.warmup_minutes,
            "baseline_hours": a.baseline_hours, "rehearsal": bool(a.rehearsal),
            "storage_guardrails": {"start_free_bytes": a.min_free_bytes, "warn_free_bytes": a.warn_free_bytes,
                                   "stop_free_bytes": a.stop_free_bytes, "check_seconds": WATCH_SECONDS,
                                   "volumes": self.volumes_now},
            "stop_instance_at_end": bool(a.stop),
        }
        parsed = urllib.parse.urlsplit(a.url)
        network = dict(inst.network(), http_url_host=parsed.hostname, http_port=parsed.port,
                       proxy_configured=bool(os.environ.get("HTTP_PROXY") or os.environ.get("HTTPS_PROXY")
                                             or os.environ.get("ALL_PROXY")))
        run = {
            "schema_version": SCHEMA, "commit": a.commit, **inst.describe(),
            "started": iso(stamp), "timezone": local.tzname(), "utc_offset_seconds": local.utcoffset().total_seconds(),
            "user_timezone": a.user_timezone, "os": platform.platform(), "kernel": platform.release(),
            "logical_cpus": os.cpu_count(), "python": platform.python_version(),
            "collector_sha256": sha256_file(collector), "network": network,
            "process_identity": inst.identity, "process_id": inst.process_id,
            "collector_process_id": os.getpid(), "settings": settings,
        }
        if inst.kind == "docker":
            run["cgroup_version"] = 2 if (Path(a.cgroup_root) / "cgroup.controllers").exists() else None
        self.run = {k: v for k, v in run.items() if v is not None}
        self.save(self.run, self.root / "run.json")
        write_text(self.root / "samples.jsonl", "")  # the analyzer requires it even if no sample lands

    # -- main loop
    def run(self):
        a = self.args
        try:
            self.prepare()
        except OSError as error:
            raise SystemExit(f"Cannot prepare the run ({type(error).__name__}).") from None
        status_path, started = self.root / "status.json", self.clock()
        phase = "startup"
        self.sample = 0
        suppression_seen = False
        self.suppression_end_at = self.baseline_start = baseline_at = end_at = None
        bad_health = bad_metrics = bad_bounds = bad_memory = disk_failures = 0
        unverified_since = None
        last_completed, last_progress = 0.0, 0.0
        self.verdict = None
        history, volumes = {}, self.volumes_now
        next_sample, next_watch = 5.0, 0.0
        self.shutdown = None
        self.finalization_errors = 0
        try:
            self.run_json()
            self.note(f"started identity={self.instance.identity}; waiting for sampling suppression to end")
            while True:
                elapsed = self.clock() - started
                if self.stop_flag.is_set() or (self.root / "stop.request").exists():
                    self.reason = "operator_stop"
                    break
                if elapsed >= (8 + a.baseline_hours) * 3600:
                    self.reason = "overall_deadline_without_complete_baseline"
                    break
                if elapsed >= next_watch:
                    next_watch = elapsed + WATCH_SECONDS
                    verdict = self.instance.check()
                    if verdict.kind in ("exited", "changed"):
                        self.verdict, self.reason = verdict, verdict.reason
                        break
                    if verdict.kind == "unverified":
                        unverified_since = elapsed if unverified_since is None else unverified_since
                        if elapsed - unverified_since >= UNVERIFIED_LIMIT_SECONDS:
                            self.reason = "collector_error"
                            self.note("instance identity unverifiable for three minutes")
                            break
                    else:
                        unverified_since = None
                    try:
                        volumes = self.volume_reader(self.storage_paths())
                        disk_failures = 0
                    except OSError:
                        disk_failures, volumes = disk_failures + 1, []
                    for volume in volumes:
                        if volume["free_bytes"] < a.warn_free_bytes:
                            self.note(f"disk reserve warning volume={volume['volume']} free={volume['free_bytes']}")
                        past = [h for h in history.get(volume["volume"], []) if h[0] >= elapsed - 600]
                        if past and elapsed - past[0][0] >= 60:
                            rate = (past[0][1] - volume["free_bytes"]) / (elapsed - past[0][0])
                            if rate > 0 and (volume["free_bytes"] - a.stop_free_bytes) / rate < 3600:
                                self.note(f"projected disk reserve within one hour volume={volume['volume']}")
                        history[volume["volume"]] = past + [(elapsed, volume["free_bytes"])]
                    self.write(self.root / "disk.jsonl", dumps({
                        "time": iso(self.now()), "elapsed_seconds": elapsed, "volumes": volumes,
                        "read_failures": disk_failures}), append=True)
                    guard = guard_reason(volumes, a.stop_free_bytes, disk_failures)
                    if guard:
                        self.reason = guard
                        break
                if elapsed < next_sample:
                    self.sleep(a.tick_seconds)
                    continue
                next_sample = elapsed + SAMPLE_SECONDS
                verdict = self.instance.check()
                if verdict.kind in ("exited", "changed"):
                    self.verdict, self.reason = verdict, verdict.reason
                    break
                if verdict.kind == "unverified":  # a gap, never a row for an unverified identity
                    unverified_since = elapsed if unverified_since is None else unverified_since
                    self.note("instance identity unverified; sample skipped")
                    if elapsed - unverified_since >= UNVERIFIED_LIMIT_SECONDS:
                        self.reason = "collector_error"
                        break
                    continue
                unverified_since = None
                stamp = self.now()
                health = self.probe("/healthz")
                bad_health = 0 if health == 200 else bad_health + 1
                values, metrics_ok, text = {}, False, ""
                try:
                    status, body = self.fetch(a.url.rstrip("/") + "/metrics")
                    if status != 200:
                        raise ValueError("metrics status")
                    text = body.decode("utf-8")
                    values = {name: read_metric(text, name) for name in REQUIRED}
                    metrics_ok, bad_metrics = True, 0
                except (OSError, ValueError, http.client.HTTPException):
                    values, bad_metrics = {}, bad_metrics + 1
                    self.note("metrics unavailable or missing required fields")
                if metrics_ok:
                    snapshot = sanitize_snapshot(text)
                    self.write(self.root / "metrics" / f"{self.sample:05d}.prom", snapshot)
                    if values["dht_sample_suppression_seconds"] > 0:
                        suppression_seen = True
                    if suppression_seen and values["dht_sample_suppression_seconds"] == 0 and baseline_at is None:
                        self.suppression_end_at = iso(stamp)
                        baseline_at = elapsed + a.warmup_minutes * 60
                        end_at = baseline_at + a.baseline_hours * 3600
                        self.note(f"sampling suppression ended; warming up for {a.warmup_minutes:g} minutes")
                    if baseline_at is not None and elapsed >= baseline_at and self.baseline_start is None:
                        self.baseline_start = iso(stamp)
                        end_at = elapsed + a.baseline_hours * 3600
                        self.write(self.root / "baseline-start.prom", snapshot)
                        self.note(f"baseline started; collecting {a.baseline_hours:g} hours")
                    over = (values["dht_peer_active"] + values["dht_peer_queued"] > 32
                            or values["dht_sample_cooling"] + values["dht_sample_inflight"] > 200000
                            or values["dht_sample_frontier"] > 20000 or values["dht_outstanding"] > 1024
                            or values["fetch_lookups"] > 256)
                    bad_bounds = bad_bounds + 1 if over else 0
                    if values["dht_lookups_completed_total"] != last_completed or values["dht_peer_active"] == 0:
                        last_completed, last_progress = values["dht_lookups_completed_total"], elapsed
                resources = self.instance.resources()
                bad_memory = bad_memory + 1 if resources.get("working_set_bytes", 0) > GIB else 0
                phase = ("baseline" if self.baseline_start else "warmup" if baseline_at is not None else "startup")
                row = {"schema_version": SCHEMA, "sequence": self.sample, "process_identity": self.instance.identity,
                       "time": iso(stamp), "elapsed_seconds": round(elapsed, 1), "phase": phase,
                       "process_id": self.instance.process_id, "health": health, "metrics_ok": metrics_ok,
                       **resources, **self.instance.row_extra(), "volumes": volumes, "values": values}
                family = ip_family_counters(text) if metrics_ok else {}
                if family:
                    row["ip_family_counters"] = family
                self.write(self.root / "samples.jsonl", dumps(row), append=True)
                self.save({"state": "running", "phase": phase, "sample": self.sample, "latest": row,
                           "suppression_ended": self.suppression_end_at, "baseline_started": self.baseline_start,
                           "expected_end": iso(stamp + timedelta(seconds=end_at - elapsed)) if end_at else None},
                          status_path)
                self.sample += 1
                if bad_health >= 3:
                    self.reason = "three_consecutive_health_failures"
                    break
                if bad_metrics >= 3:
                    self.reason = "three_consecutive_metrics_failures"
                    break
                if bad_bounds >= 3:
                    self.reason = "three_consecutive_capacity_violations"
                    break
                if bad_memory >= 5:
                    self.reason = "rss_over_1GiB_for_five_samples"
                    break
                if metrics_ok and values["dht_peer_active"] > 0 and elapsed - last_progress >= 300:
                    self.reason = "active_peer_requests_without_completion_for_five_minutes"
                    break
                if endpoint_reached(elapsed, end_at, metrics_ok and health == 200, self.baseline_start):
                    self.reason = "baseline_window_complete"
                    break
        except Exception as error:  # evidence_write_error keeps its own reason
            if self.reason != "evidence_write_error":
                self.reason = "collector_error"
            self.note(f"collector failure ({type(error).__name__}); evidence may be incomplete")
            sys.stderr.write("Collector failed; starting protected finalization.\n")
        finally:
            self.finish(status_path, started)
        return self.exit_code()

    def probe(self, path):
        try:
            status, _ = self.fetch(self.args.url.rstrip("/") + path)
            return status
        except (OSError, http.client.HTTPException):
            return 0

    # -- finalization
    def finish(self, status_path, started):
        if self.args.stop and (self.verdict is None or self.verdict.kind == "unverified"):
            try:
                self.shutdown = self.instance.stop()
            except Exception:
                self.shutdown = dict(new_shutdown(), errors=["cleanup_failed"])
        elif self.verdict is not None and self.verdict.kind == "exited":
            self.shutdown = dict(new_shutdown(), exit_confirmed=True, exit_code=self.verdict.exit_code,
                                 stop_mode=f"already_exited_{self.verdict.exit_code if self.verdict.exit_code is not None else 'unknown'}")
        elif self.verdict is not None:  # identity changed: this process's fate is unconfirmed
            self.shutdown = dict(new_shutdown(), stop_mode="identity_changed", errors=["identity_changed"])
        else:
            self.shutdown = dict(new_shutdown(), stop_mode="left_running")
        result = {
            "schema_version": SCHEMA,
            "state": "stopped" if self.shutdown.get("exit_confirmed") else "unknown",
            "reason": self.reason, "stop_mode": self.shutdown["stop_mode"], "samples": self.sample,
            "shutdown": self.shutdown, "process_identity": self.instance.identity,
            "ended": iso(self.now()), "suppression_ended": self.suppression_end_at,
            "baseline_started": self.baseline_start, "elapsed_seconds": self.clock() - started,
            "assessment": "Evidence collected; review required before any acceptance or performance claim.",
        }
        if self.stop_signal and self.reason == "operator_stop":
            result["stop_signal"] = self.stop_signal
        if self.verdict is not None:
            result.update({k: v for k, v in self.verdict.detail.items()})
            if self.verdict.kind == "changed":
                result["identity_change"] = {"expected": self.instance.identity, **self.verdict.detail}
                result.pop("observed", None)
        if getattr(self.instance, "parent_changes", 0):
            result["network_parent_changes"] = self.instance.parent_changes
        self.finalization_errors = complete_evidence(result, status_path, self.root / "result.json")
        if self.finalization_errors:
            sys.stderr.write("Final evidence publication failed; completion remains uncertain.\n")
        self.note(f"done: {self.reason}; stop={self.shutdown['stop_mode']}")

    def exit_code(self):
        if self.reason != "baseline_window_complete" or self.finalization_errors:
            return 1
        if self.args.stop:
            s = self.shutdown
            return 0 if (s["stop_mode"] == "exited_0" and s["signal_delivered"] and not s["errors"]) else 1
        return 0


def dumps(value):
    return json.dumps(value, separators=(",", ":"), allow_nan=False)


def complete_evidence(result, status_path, result_path, save=save_json):
    """Publish status then result independently; the result carries any failure."""
    failures = 0
    result["publication_failures"] = []
    result["finalization_complete"] = False
    for path in (status_path, result_path):
        result["finalization_complete"] = path == result_path and failures == 0
        try:
            save(result, path)
        except (OSError, ValueError):
            failures += 1
            result["publication_failures"].append("status" if path == status_path else "result")
            result["finalization_complete"] = False
    if "result" in result["publication_failures"]:
        try:
            save(result, status_path)
        except (OSError, ValueError):
            pass
    return failures


def build_parser():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--mode", choices=("docker", "native"), required=True)
    p.add_argument("--container", help="Docker container name or id (docker mode)")
    p.add_argument("--docker-socket", default="/var/run/docker.sock")
    p.add_argument("--cgroup-root", default="/sys/fs/cgroup", help="Host cgroup v2 mount (docker mode)")
    p.add_argument("--cgroup-path", help="Explicit container cgroup directory (docker mode)")
    p.add_argument("--pid", type=int, help="Lodestar process id (native mode)")
    p.add_argument("--proc-root", default="/proc", help="proc mount to read (native mode)")
    p.add_argument("--url", default="http://127.0.0.1:8484", help="Lodestar base URL, as this collector reaches it")
    p.add_argument("--root", required=True, help="New, empty evidence directory")
    p.add_argument("--commit", required=True, help="Source commit under test (lowercase hex)")
    p.add_argument("--warmup-minutes", type=float, default=30)
    p.add_argument("--baseline-hours", type=float, default=24)
    p.add_argument("--rehearsal", action="store_true", help="Allow shorter windows; marked in run.json")
    p.add_argument("--storage-path", action="append", default=[], help="Extra path whose filesystem to guard (repeatable)")
    p.add_argument("--min-free-bytes", type=int, default=50 * GIB)
    p.add_argument("--warn-free-bytes", type=int, default=20 * GIB)
    p.add_argument("--stop-free-bytes", type=int, default=10 * GIB)
    p.add_argument("--stop", action="store_true", help="Stop the verified instance when collection ends")
    p.add_argument("--require-gluetun", action="store_true", help="Refuse to start unless gluetun is the network parent")
    p.add_argument("--user-timezone", default="America/Denver")
    p.add_argument("--tick-seconds", type=float, default=1.0, help=argparse.SUPPRESS)
    return p


def make_instance(args, parser):
    if args.mode == "docker":
        if not args.container:
            parser.error("--container is required in docker mode")
        return DockerInstance(DockerAPI(args.docker_socket), args.container, args.cgroup_root, args.cgroup_path)
    if args.pid is None:
        parser.error("--pid is required in native mode")
    return NativeInstance(args.pid, args.proc_root)


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    for name in ("warmup_minutes", "baseline_hours", "tick_seconds"):
        if not math.isfinite(getattr(args, name)) or getattr(args, name) <= 0:
            parser.error(f"--{name.replace('_', '-')} must be positive and finite")
    collector = Collector(args, make_instance(args, parser))
    for number in (signal.SIGTERM, signal.SIGINT):
        signal.signal(number, lambda n, _frame: collector.request_stop(signal.Signals(n).name))
    try:
        return collector.run()
    except SystemExit as error:
        if isinstance(error.code, str):
            sys.stderr.write(error.code + "\n")
            return 2
        raise


if __name__ == "__main__":
    sys.exit(main())
