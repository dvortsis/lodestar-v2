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
import ipaddress
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
from collections import Counter
from datetime import datetime, timedelta, timezone
from pathlib import Path

SCHEMA = 2
SAMPLE_SECONDS = 60.0
WATCH_SECONDS = 10.0
UNVERIFIED_LIMIT_SECONDS = 180.0  # three missed samples
GIB = 2**30
# The 1 GiB x 5 stop rule's basis (docs/decisions/2026-10-31-X-soak-docker-memrule.md).
# A Docker cgroup's working set includes page cache from file reads (active_file),
# so Docker mode judges the cgroup's anonymous memory; native mode keeps VmRSS.
MEMORY_STOP_BASIS = {"docker": "anon", "native": "rss"}
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
# Storage and cache sizes (R-gauges), kept in snapshots but never required.
STORAGE = (
    "store_db_bytes", "store_wal_bytes", "search_index_bytes", "storage_sampled_seconds",
    "storage_sample_errors_total", "search_file_cache_entries", "search_file_cache_bytes",
    "search_browse_cache_entries", "search_browse_cache_bytes", "search_result_cache_entries",
    "search_result_cache_bytes",
    "have_bytes", "hot_columns_bytes",
)
# CR-COL-3: the crawler gate's evidence (docs/decisions/2026-11-02-CR-SPEC.md rules
# 3, 4, 5 and 6.10). Each labelled series lists its label keys, in rendered order,
# with the only values it may carry (docs/metrics.md); any other label set is dropped.
LATENCY_EDGES = ("1", "2", "5", "10", "30", "60", "120", "300", "600", "1800", "3600", "7200", "+Inf")
SUPPRESSION_REASONS = frozenset((
    "off", "locked", "first_start", "corrupt", "version", "dirty", "off_run", "data_dir",
    "node_identity", "foreign_run", "snapshot_mismatch", "no_witness", "generation",
    "horizon_changed", "clock_backward", "stale", "invalidate_failed", "resumed"))
_ORIGIN = (("origin", frozenset(("network", "recovery", "import"))),)
_TRANSPORT = (("transport", frozenset(("tcp", "utp"))),)
_FAMILY = (("family", frozenset(("ipv4", "ipv6"))),)
_KIND = frozenset(("arrival", "retry", "requeue"))
LABELLED = {
    "searchable_new_total": _ORIGIN,
    "metadata_new_verified_total": _ORIGIN,
    "metadata_excluded_total": (("reason", frozenset((
        "already_stored", "rediscovered", "filtered", "quarantined", "invalid_metainfo",
        "failed_verification", "duplicate", "store_failed"))),),
    "fetch_failures_total": (("kind", frozenset((
        "unreachable", "timed_out", "refused", "encryption_failed", "invalid", "oversize",
        "hash_mismatch"))),),
    "fetch_lost_total": (("reason", frozenset((
        "no_peers", "timed_out", "refused", "encryption_failed", "oversize", "invalid", "blocked",
        "evicted", "gave_up", "unreachable"))),),
    "fetch_attempts_total": _TRANSPORT,
    "fetch_success_total": _TRANSPORT,
    "fetch_connect_timeouts_total": _TRANSPORT,
    "fetch_family_attempts_total": _FAMILY,
    "fetch_family_success_total": _FAMILY,
    "fetch_no_route_total": _FAMILY,
    "fetch_route_probe_down_total": _FAMILY,
    "fetch_route_skipped_total": _FAMILY,
    "fetch_route_down": _FAMILY,
    "fetch_admission_waits_total": (("kind", _KIND),),
    "fetch_admission_wait_cancelled_total": (("reason", frozenset(("evicted", "peers"))),),
    "fetch_admission_grants_total": (("kind", _KIND), ("class", frozenset(("interactive", "background")))),
    "fetch_admission_refusals_total": (("reason", frozenset(("refused", "unacknowledged"))),),
    "fetch_admission_permit_returns_total": (("boundary", frozenset(("finder_full", "finder_closed", "unused"))),),
    "dht_peer_permit_returns_total": (("boundary", frozenset((
        "completed", "cancelled_queued", "cancelled_active", "paused", "refused", "ack_failed",
        "handoff_failed", "shutdown"))),),
    "dht_peer_rejection_reasons_total": (("reason", frozenset(("capacity", "paused", "caller_closed"))),),
    "dht_peer_queued_dropped_total": (("reason", frozenset(("caller_closed", "paused"))),),
    "fetch_evicted_service_total": (("stage", frozenset(("unaccepted", "lookup", "peers", "fetch"))),),
    "dht_harvested_total": (("strategy", frozenset(("sample", "get_peers", "announce"))),),
    "dht_sample_suppression_reason": (("reason", SUPPRESSION_REASONS),),
    "discovery_to_searchable_seconds_bucket": (("le", frozenset(LATENCY_EDGES)),),
}
# Gate series that carry no labels.
PLAIN = frozenset((
    "discovery_to_searchable_seconds_count", "discovery_to_searchable_seconds_sum",
    "discovery_to_searchable_dropped_total", "fetch_admission_wakeups_total",
    "fetch_admission_holds_total", "fetch_admission_waiters", "fetch_admission_permits_in_use",
    "dht_utp_in_total", "utp_retransmits_total", "utp_connection_timeouts_total",
    "dht_sample_sent_total", "dht_throttled_total",
))
MAX_SERIES = 512  # a snapshot holding more is cut off and flagged; the allowlist renders about 250
KEEP = frozenset(REQUIRED) | frozenset(COUNTERS) | frozenset(STORAGE) | frozenset(LABELLED) | PLAIN
NUMBER = r"[0-9]+(?:\.[0-9]+)?"
SERIES = re.compile(r"^lodestar_([a-z_0-9]+)(\{[^{}\r\n]*\})? (" + NUMBER + r")\r?$")
SAFE_LABELS = re.compile(r'^\{[a-z_]+="[a-z_0-9]{1,24}"(,[a-z_]+="[a-z_0-9]{1,24}")*\}$')
LABEL_PAIR = re.compile(r'([a-z_]+)="([^"\\]*)"')
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


def labels_allowed(name, labels):
    """A gate series' labels must be exactly its keys, in order, with known values."""
    schema = LABELLED.get(name)
    if schema is None:
        if name in PLAIN:
            return not labels
        return not labels or bool(SAFE_LABELS.fullmatch(labels))  # CR-0's series, as before
    if not labels:
        return False
    pairs = LABEL_PAIR.findall(labels)
    if "{" + ",".join(f'{k}="{v}"' for k, v in pairs) + "}" != labels or len(pairs) != len(schema):
        return False
    return all(k == key and v in allowed for (k, v), (key, allowed) in zip(pairs, schema))


def sanitize_snapshot(text, flags=None):
    """Keep allowlisted series with validated labels; drop everything else.

    At most MAX_SERIES series are kept; past that the snapshot is cut off and
    `flags["series_capped"]` is set, so the analyzer can treat it as missing.
    """
    seen, out = set(), []
    for line in text.splitlines():
        match = SERIES.fullmatch(line)
        if not match or match[1] not in KEEP:
            continue
        name, labels, value = match.groups()
        if not labels_allowed(name, labels):
            continue
        if (name, labels) in seen:
            continue
        if len(out) >= MAX_SERIES:
            if flags is not None:
                flags["series_capped"] = True
            break
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


def host_class(host):
    """Evidence keeps the kind of host, never the name or address."""
    try:
        ip = ipaddress.ip_address(host.strip("[]"))
    except ValueError:
        return "loopback" if host == "localhost" else "name"
    return "loopback" if ip.is_loopback else "private" if (ip.is_private or ip.is_link_local) else "public"


def network_mode_class(mode):
    if mode.startswith("container:"):
        return "container"
    return {"default": "bridge"}.get(mode, mode) if mode in ("host", "bridge", "none", "default") else "custom"


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

    def request(self, method, path, timeout=None, body=None, raw=False):
        conn = UnixHTTPConnection(self.socket_path, timeout or self.timeout)
        try:
            if body is None:
                conn.request(method, path)
            else:
                conn.request(method, path, json.dumps(body), {"Content-Type": "application/json"})
            response = conn.getresponse()
            data = response.read(8 << 20)
            if raw:
                return response.status, data
            try:
                doc = json.loads(data) if data else None
            except ValueError:
                doc = None
            return response.status, doc
        finally:
            conn.close()

    def get(self, path):
        return self.request("GET", path)

    def post(self, path, timeout=None):
        return self.request("POST", path, timeout)

    def post_json(self, path, body, timeout=None):
        return self.request("POST", path, timeout, body=body)

    def post_raw(self, path, body, timeout=None):
        return self.request("POST", path, timeout, body=body, raw=True)


class DockerInstance:
    kind = "docker"

    def __init__(self, api, container, cgroup_root="/sys/fs/cgroup", cgroup_path=None,
                 stop_timeout=60, clock=time.monotonic, sleep=time.sleep, proc_root="/proc"):
        self.api, self.container, self.proc = api, container, Path(proc_root)
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
        info = {"mode": network_mode_class(mode), "host_network": mode == "host",
                "gluetun_network_parent": False}
        if self.parent_doc is not None:
            image = str((self.parent_doc.get("Config") or {}).get("Image", ""))
            info["gluetun_network_parent"] = "gluetun" in image.lower()
            info["network_parent_container_id"] = self.parent_id
        return info

    def describe(self):
        info = {"container_id": self.id, "image_id": self.doc.get("Image"),
                "restart_count": self.doc.get("RestartCount")}
        image_hex = str(self.doc.get("Image", "")).removeprefix("sha256:")
        if re.fullmatch(r"[0-9a-f]{64}", image_hex):
            info["sha256"], info["sha256_kind"] = image_hex, "image_id"
        try:
            status, image = self.api.get("/images/" + urllib.parse.quote(str(self.doc.get("Image")), safe="") + "/json")
            if status == 200 and image.get("RepoDigests"):
                info["image_digest"] = image["RepoDigests"][0].rsplit("@", 1)[-1]  # no registry host
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

    def environment(self):
        """The container's configured environment and mounts (docker inspect)."""
        env = {}
        for item in (self.doc.get("Config") or {}).get("Env") or []:
            key, sep, value = str(item).partition("=")
            if sep:
                env[key] = value
        mounts = [{"destination": str(m.get("Destination", "")), "type": str(m.get("Type", ""))}
                  for m in self.doc.get("Mounts") or [] if isinstance(m, dict)]
        return env, mounts

    def started_utc(self):
        try:
            text = re.sub(r"\.(\d{6})\d*", r".\1", str(self.started_at)).replace("Z", "+00:00")
            return datetime.fromisoformat(text)
        except ValueError:
            return None

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
                 opener=None, signaller=None, clk_tck=None, stop_timeout=60):
        self.pid, self.proc = int(pid), Path(proc_root)
        self.clock, self.sleep = clock, sleep
        # Signals go through a pidfd opened at attach and re-verified, so a recycled PID is never hit.
        self.opener = opener if opener is not None else getattr(os, "pidfd_open", None)
        self.signaller, self.pidfd = signaller, None
        self.real_signals = signaller is None
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
        if self.opener is not None:
            try:
                fd = self.opener(self.pid)
            except OSError:
                fd = None
            if fd is not None:
                again = self._stat()  # the pid could have been recycled before the open
                if again is None or again[2] != self.start_ticks:
                    self._close(fd)
                    raise SystemExit("Process identity changed while attaching.")
                self.pidfd = fd

    def _close(self, fd):
        if self.signaller is None:
            try:
                os.close(fd)
            except OSError:
                pass

    def close(self):
        if self.pidfd is not None:
            self._close(self.pidfd)
            self.pidfd = None

    def can_signal(self):
        return self.signaller is not None or self.pidfd is not None

    def _signal(self, number):
        if self.signaller is not None:
            self.signaller(number)
        else:
            signal.pidfd_send_signal(self.pidfd, number)

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

    def environment(self):
        """/proc/<pid>/environ (needs the same user or root); None if unreadable."""
        try:
            raw = (self.proc / str(self.pid) / "environ").read_bytes()
        except OSError:
            return None, None
        env = {}
        for item in raw.split(b"\0"):
            key, sep, value = item.decode("utf-8", "replace").partition("=")
            if sep:
                env[key] = value
        return env, None

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
        elif not self.can_signal():
            outcome["errors"].append("pidfd_unavailable")
        else:
            try:
                self._signal(signal.SIGTERM)
                outcome["signal_delivered"] = True
            except OSError:
                outcome["errors"].append("signal_failed")
            if self._wait_gone(self.stop_timeout):
                outcome.update(exit_confirmed=True, stop_mode="exited_unknown")
            elif self.check().kind == "ok":  # identity re-verified before the forced stop
                try:
                    self._signal(signal.SIGKILL)
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


def statvfs_volumes(labelled):
    """Free and total bytes per distinct filesystem, labelled by role, never by path."""
    seen, out = set(), []
    for label, path in labelled:
        device = os.stat(path).st_dev
        if device in seen:
            continue
        seen.add(device)
        stat = os.statvfs(path)
        out.append({"volume": label, "free_bytes": stat.f_bavail * stat.f_frsize,
                    "total_bytes": stat.f_blocks * stat.f_frsize})
    return out


def fsync_dir(path):
    try:
        fd = os.open(path, os.O_RDONLY)
    except OSError:
        return
    try:
        os.fsync(fd)
    except OSError:
        pass
    finally:
        os.close(fd)


def save_json(value, path):
    temp = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    try:
        with open(temp, "w", encoding="utf-8") as stream:
            stream.write(json.dumps(value, indent=2, allow_nan=False))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp, path)
        fsync_dir(path.parent)
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
        fsync_dir(path.parent)
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


# ------------------------------------------------- CR-COL-3: gate evidence (CR-SPEC)
# Run kinds measured under docs/decisions/2026-11-02-CR-SPEC.md; `cr0` keeps the old flow.
SPEC_KINDS = ("ab", "comb", "comparator")
SPEC_HOURS = {"ab": 6, "comb": 24, "comparator": 24}  # rule 1.3
SPEC_WARMUP_MINUTES = 60  # rule 1.1
TREATMENTS = {"adm": "coordinated_admission", "utp": "utp", "route": "route_backoff"}
# Every crawler switch and its environment variable (crates/lodestar/src/config.rs).
SWITCH_ENV = {
    "safe_restart": "LODESTAR_DHT_SAFE_RESTART",
    "coordinated_admission": "LODESTAR_DHT_COORDINATED_ADMISSION",
    "responder_hints": "LODESTAR_DHT_RESPONDER_HINTS",
    "streaming_peers": "LODESTAR_DHT_STREAMING_PEERS",
    "target_prefixes": "LODESTAR_DHT_TARGET_PREFIXES",
    "utp": "LODESTAR_DHT_UTP",
    "route_backoff": "LODESTAR_DHT_ROUTE_BACKOFF",
}
PRESETS = ("low", "balanced", "aggressive")
CHECKLIST_ITEMS = ("auto_update", "backup")
CHECKLIST_STATES = ("excluded", "not_applicable")
# Rule 6.11's fixed host-activity categories.
ACTIVITY_CATEGORIES = ("import", "other_crawler", "array_operation", "host_reboot",
                       "plugin_container_action", "link_outage", "router_outage",
                       "public_ip_change", "power_event")
# Rule 5.4: the comparator's count, read-only, taken at every scrape.
COMPARATOR_QUERY = "SELECT count(*) FROM torrents"
COMPARATOR_ENV = "PGOPTIONS=-c default_transaction_read_only=on"
DEFAULT_IP_ECHO = "https://api.ipify.org"
BOUNDARY_SECONDS = 90  # rule 1.4
FIRST_START_SECONDS = 6 * 3600


def open_eligible(values, process_age):
    """D6: the scrape that can be T_open. The DHT reads suppression 0 with reason
    `off` for about a second after start, before its restart check reports, so a
    zero beside `off` counts only once the full 6 h suppression could have run."""
    if values.get("dht_sample_suppression_seconds") != 0:
        return False
    return values.get("suppression_reason") != "off" or process_age >= FIRST_START_SECONDS


def suppression_reason(text):
    """The one reason label set to 1, or None when it isn't exactly one known reason."""
    found = [m[1] for m in re.finditer(r'(?m)^lodestar_dht_sample_suppression_reason\{reason="([a-z_]+)"\} 1\r?$', text)]
    return found[0] if len(found) == 1 and found[0] in SUPPRESSION_REASONS else None


def parse_bool(value):
    """Rust's bool parser: exactly `true` or `false`."""
    return {"true": True, "false": False}.get(value)


def config_identity(env, mounts=None):
    """Switch states and preset from the instance's environment (D1, rule 7.2).

    Only the variables named in SWITCH_ENV, LODESTAR_INTENSITY and the two paths
    are read; paths are reduced to facts, never kept. An unparsable value makes
    that field None, which the analyzer treats as missing identity."""
    switches, sources = {}, {}
    for name, var in SWITCH_ENV.items():
        if var in env:
            switches[name], sources[name] = parse_bool(env[var]), "env"
        else:
            switches[name], sources[name] = False, "default"
    preset = env.get("LODESTAR_INTENSITY")
    out = {"switches": switches, "switch_sources": sources,
           "env_preset": preset if preset in PRESETS else (None if preset is not None else "unset")}
    witness, data = env.get("LODESTAR_DHT_WITNESS_DIR") or None, env.get("LODESTAR_DATA_DIR") or None
    info = {"configured": witness is not None}
    if witness is not None and mounts is not None:
        def under(path, root):
            return path == root or path.startswith(root.rstrip("/") + "/")
        holder = max((m for m in mounts if under(witness, m["destination"])),
                     key=lambda m: len(m["destination"]), default=None)
        data_holder = max((m for m in mounts if data and under(data, m["destination"])),
                          key=lambda m: len(m["destination"]), default=None)
        info["host_mounted"] = bool(holder and holder["type"] == "bind")
        info["outside_data_dir"] = (data is not None and not under(witness, data)
                                    and (holder is None or holder is not data_holder))
    elif witness is not None:  # native: the host's own filesystem
        info["host_mounted"] = True
        info["outside_data_dir"] = data is not None and not (
            witness == data or witness.startswith(data.rstrip("/") + "/"))
    out["witness"] = info
    return out


def ntp_status(run_command):
    """D1a: the host's NTP state at a process start. chrony first, then
    systemd-timesyncd, then the kernel's own flag (works inside a container)."""
    out = run_command(["chronyc", "-n", "tracking"])
    if out:
        offset = re.search(r"(?m)^System time\s*:\s*([0-9.]+) seconds (fast|slow)", out)
        leap = re.search(r"(?m)^Leap status\s*:\s*(\S.*)$", out)
        if offset and leap:
            return {"source": "chronyc", "synchronized": leap[1].strip() == "Normal",
                    "offset_seconds": float(offset[1])}
    out = run_command(["timedatectl", "show", "-p", "NTPSynchronized", "--value"])
    if out and out.strip() in ("yes", "no"):
        status = {"source": "timedatectl", "synchronized": out.strip() == "yes", "offset_seconds": None}
        detail = run_command(["timedatectl", "timesync-status"]) or ""
        match = re.search(r"(?m)^\s*Offset:\s*([+-]?[0-9.]+)(us|ms|s)\b", detail)
        if match:
            status["offset_seconds"] = abs(float(match[1])) / {"us": 1e6, "ms": 1e3, "s": 1}[match[2]]
            return status
        kernel = kernel_ntp_status()  # ntpd under systemd: timesyncd has no offset to give
        return kernel if kernel["source"] == "kernel" else status
    return kernel_ntp_status()


def kernel_ntp_status():
    """ntp_adjtime(2) read-only: STA_UNSYNC and the estimated error."""
    try:
        import ctypes
        import ctypes.util

        class Timex(ctypes.Structure):
            _fields_ = [("modes", ctypes.c_uint), ("offset", ctypes.c_long), ("freq", ctypes.c_long),
                        ("maxerror", ctypes.c_long), ("esterror", ctypes.c_long), ("status", ctypes.c_int),
                        ("constant", ctypes.c_long), ("precision", ctypes.c_long),
                        ("tolerance", ctypes.c_long), ("time_sec", ctypes.c_long),
                        ("time_usec", ctypes.c_long), ("tick", ctypes.c_long),
                        ("pad", ctypes.c_byte * 256)]
        libc = ctypes.CDLL(ctypes.util.find_library("c") or "libc.so.6", use_errno=True)
        timex = Timex()
        state = libc.ntp_adjtime(ctypes.byref(timex))
    except (OSError, AttributeError, ImportError):
        return {"source": "unavailable", "synchronized": None, "offset_seconds": None}
    if state < 0:
        return {"source": "unavailable", "synchronized": None, "offset_seconds": None}
    unsync = bool(timex.status & 0x0040) or state == 5  # STA_UNSYNC, TIME_ERROR
    return {"source": "kernel", "synchronized": not unsync,
            "offset_seconds": None if unsync else max(timex.esterror, 0) / 1e6}


def subprocess_output(argv, timeout=5):
    import subprocess
    try:
        done = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, check=False)
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout if done.returncode == 0 else None


def net_counters(proc_root, pid):
    """Rule 7.1: the process's network namespace counters, summed over
    non-loopback interfaces, and its socket counts. Absent if unreadable."""
    if not pid:
        return None
    base = Path(proc_root) / str(pid) / "net"
    dev, sockstat = read_text(base / "dev"), read_text(base / "sockstat")
    if dev is None or sockstat is None:
        return None
    out = {"rx_bytes": 0, "tx_bytes": 0, "rx_packets": 0, "tx_packets": 0}
    for line in dev.splitlines()[2:]:
        name, _, fields = line.partition(":")
        parts = fields.split()
        if name.strip() == "lo" or len(parts) < 16:
            continue
        try:
            out["rx_bytes"] += int(parts[0])
            out["rx_packets"] += int(parts[1])
            out["tx_bytes"] += int(parts[8])
            out["tx_packets"] += int(parts[9])
        except ValueError:
            return None
    for key, pattern in (("sockets_used", r"(?m)^sockets: used (\d+)"),
                         ("tcp_inuse", r"(?m)^TCP: inuse (\d+)"), ("udp_inuse", r"(?m)^UDP: inuse (\d+)")):
        match = re.search(pattern, sockstat)
        if match:
            out[key] = int(match[1])
    return out


def ip_echo(url, timeout=10):
    """One lookup to a fixed IP-echo service, without any proxy (rule 6.5)."""
    status, body = http_get(url, timeout=timeout, limit=128)
    if status != 200:
        raise ValueError("ip echo status")
    return str(ipaddress.ip_address(body.decode("ascii").strip()))


class PublicIP:
    """Keeps the reference address in memory only; evidence gets the verdict."""

    def __init__(self, url, lookup=ip_echo):
        self.url, self.lookup, self.reference = url, lookup, None

    def check(self):
        try:
            address = self.lookup(self.url)
        except (OSError, ValueError, UnicodeError, http.client.HTTPException):
            return "failed"
        if self.reference is None:
            self.reference = address
            return "reference"
        return "unchanged" if address == self.reference else "changed"


class LodestarAPI:
    """A logged-in session for the settings and Insights APIs (rule 5.5, §8).

    API keys can't reach them, so the collector logs in with the admin password
    from a file. The password and the cookie stay in memory; neither is written."""

    def __init__(self, base, password_file, request=None):
        self.base = base.rstrip("/") + "/api/v1"
        self.password_file = password_file
        self.request = request or api_request
        self.cookie = None

    def _login(self):
        password = Path(self.password_file).read_text(encoding="utf-8").strip()
        status, headers, _ = self.request("POST", self.base + "/login", {"password": password}, None)
        cookie = re.search(r"(?:^|,\s*)(lodestar_session=[^;,\s]+)", headers.get("set-cookie", ""))
        if status != 200 or not cookie:
            raise ValueError("login refused")
        self.cookie = cookie[1]

    def get(self, path):
        for attempt in (0, 1):
            if self.cookie is None:
                self._login()
            status, _, body = self.request("GET", self.base + path, None, self.cookie)
            if status == 401 and attempt == 0:
                self.cookie = None
                continue
            if status != 200:
                raise ValueError("api status")
            return json.loads(body)
        raise ValueError("api login")

    def settings(self):
        """The effective preset and whether the crawler is paused, with their sources."""
        crawler = self.get("/settings/crawler")
        fields = crawler.get("fields") or {}
        out = {}
        for name in ("intensity", "paused"):
            field = fields.get(name) or {}
            value, source = field.get("value"), field.get("source")
            if name == "intensity" and value not in PRESETS:
                value = None
            if name == "paused" and not isinstance(value, bool):
                value = None
            out[name] = value
            out[name + "_source"] = source if source in ("default", "file", "db", "env") else "other"
        pending = crawler.get("restart_required")
        out["restart_pending"] = bool(pending) if isinstance(pending, list) else None
        return out

    def imports(self, from_unix, to_unix):
        """Counts of import, pause and preset annotations in the range; no job details."""
        doc = self.get(f"/insights/events?from={int(from_unix)}&to={int(to_unix)}")
        kinds = Counter(a.get("kind") for a in doc.get("annotations") or [] if isinstance(a, dict))
        return {"from_unix": int(from_unix), "to_unix": int(to_unix),
                "import_started": kinds["import_started"], "import_finished": kinds["import_finished"],
                "intensity_changes": kinds["intensity"], "paused": kinds["paused"] + len(doc.get("paused") or []),
                "truncated": bool(doc.get("truncated"))}


def api_request(method, url, body, cookie, timeout=10):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, method=method,
                                     headers={"Content-Type": "application/json", "Accept-Encoding": "identity"})
    if cookie:
        request.add_header("Cookie", cookie)
    try:
        with OPENER.open(request, timeout=timeout) as response:
            return response.status, {k.lower(): v for k, v in response.headers.items()}, response.read(1 << 20)
    except urllib.error.HTTPError as error:
        return error.code, {}, b""


def demux_docker_stream(raw):
    """stdout of a non-TTY exec: 8-byte frame headers (stream, 0, 0, 0, size)."""
    out, i = bytearray(), 0
    while i + 8 <= len(raw):
        stream, size = raw[i], int.from_bytes(raw[i + 4:i + 8], "big")
        if stream == 1:
            out += raw[i + 8:i + 8 + size]
        i += 8 + size
    return bytes(out)


def comparator_count(api, container, user, database):
    """Rule 5.4: bitmagnet's stored torrents, by a read-only query inside its
    Postgres container (Docker exec). Returns an int, or None on any failure."""
    cmd = ["psql", "-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-U", user, "-d", database,
           "-c", COMPARATOR_QUERY]
    try:
        status, doc = api.post_json(f"/containers/{urllib.parse.quote(container, safe='')}/exec",
                                    {"AttachStdout": True, "AttachStderr": True, "Env": [COMPARATOR_ENV],
                                     "Cmd": cmd})
        if status != 201 or not isinstance(doc, dict) or "Id" not in doc:
            return None
        status, raw = api.post_raw(f"/exec/{doc['Id']}/start", {"Detach": False, "Tty": False}, timeout=60)
        if status != 200:
            return None
        status, info = api.get(f"/exec/{doc['Id']}/json")
        if status != 200 or not isinstance(info, dict) or info.get("ExitCode") != 0:
            return None
    except (OSError, ValueError, http.client.HTTPException):
        return None
    text = demux_docker_stream(raw).decode("ascii", "replace").strip()
    return int(text) if re.fullmatch(r"[0-9]{1,15}", text) else None


def resolve_defaults(args):
    """Window defaults: CR-0's 30 min and 24 h, or CR-SPEC's 60 min and the kind's hours."""
    for name, default in (("run_kind", "cr0"), ("treatment", None), ("hour_seconds", 3600.0),
                          ("sample_seconds", SAMPLE_SECONDS), ("ip_echo_url", DEFAULT_IP_ECHO),
                          ("password_file", None), ("checklist", []), ("next_run", []),
                          ("postgres_container", None), ("postgres_user", "postgres"),
                          ("postgres_db", "bitmagnet"), ("spec_commit", None)):
        if not hasattr(args, name):
            setattr(args, name, default)
    spec = args.run_kind in SPEC_KINDS
    if args.warmup_minutes is None:
        args.warmup_minutes = SPEC_WARMUP_MINUTES if spec else 30
    if args.baseline_hours is None:
        args.baseline_hours = SPEC_HOURS[args.run_kind] if spec else 24
    return args


def parse_checklist(items, next_runs):
    """Rule 6.5's host checklist: each item excluded (with its next scheduled run)
    or not applicable, as the operator states it at launch."""
    out = {}
    for text in items:
        item, _, state = text.partition("=")
        if item not in CHECKLIST_ITEMS or state not in CHECKLIST_STATES or item in out:
            raise SystemExit(f"--checklist takes ITEM=STATE once per item; items {CHECKLIST_ITEMS}, "
                             f"states {CHECKLIST_STATES}.")
        out[item] = {"state": state}
    for text in next_runs:
        item, _, when = text.partition("=")
        try:
            stamp = datetime.fromisoformat(when.replace("Z", "+00:00"))
        except ValueError:
            stamp = None
        if item not in out or out[item]["state"] != "excluded" or stamp is None or stamp.tzinfo is None:
            raise SystemExit("--next-run takes ITEM=<UTC time> for an item checked as excluded.")
        out[item]["next_run_utc"] = iso(stamp.astimezone(timezone.utc))
    return out


def note_command(argv, now=utc_now, clock=time.monotonic, boot_id_path="/proc/sys/kernel/random/boot_id"):
    """`collect.py note`: a host-activity entry (rule 6.11), stamped with UTC and
    the collector's own elapsed_seconds, appended to the run's private log."""
    parser = argparse.ArgumentParser(prog="collect.py note", description=note_command.__doc__)
    parser.add_argument("--root", required=True, help="The running collector's evidence directory")
    parser.add_argument("--category", required=True, choices=ACTIVITY_CATEGORIES + ("other",))
    parser.add_argument("--text", default="", help="Private detail (at most 200 characters); never published")
    args = parser.parse_args(argv)
    if len(args.text) > 200 or any(ord(c) < 32 for c in args.text):
        parser.error("--text must be one line of at most 200 characters")
    root = Path(args.root)
    try:
        run = json.loads((root / "run.json").read_text(encoding="utf-8"))
        origin = run["clock"]["monotonic_origin"]
    except (OSError, ValueError, KeyError, TypeError):
        sys.stderr.write("Not a CR-COL-3 run directory (run.json has no clock origin).\n")
        return 2
    boot = (read_text(boot_id_path) or "").strip()
    entry = {"time": iso(now()), "elapsed_seconds": round(clock() - origin, 1), "category": args.category,
             "same_boot": bool(boot) and boot == run["clock"].get("boot_id"),
             "collector_finished": (root / "result.json").exists(), "text": args.text}
    append_line(root / "activity.jsonl", dumps(entry))
    print(f"Recorded {args.category} at elapsed {entry['elapsed_seconds']:.1f} s ({entry['time']}).")
    return 0


class Collector:
    def __init__(self, args, instance, *, clock=time.monotonic, sleep=time.sleep,
                 now=utc_now, fetch=http_get, volumes=statvfs_volumes, ip_lookup=ip_echo,
                 api_call=None, run_command=subprocess_output):
        self.args, self.instance = args, instance
        self.clock, self.sleep, self.now, self.fetch, self.volume_reader = clock, sleep, now, fetch, volumes
        self.root = Path(args.root)
        self.kind = getattr(args, "run_kind", "cr0")
        self.spec = self.kind in SPEC_KINDS
        resolve_defaults(args)
        self.ip = PublicIP(getattr(args, "ip_echo_url", DEFAULT_IP_ECHO), ip_lookup)
        self.api = (LodestarAPI(args.url, args.password_file, api_call)
                    if getattr(args, "password_file", None) else None)
        self.run_command = run_command
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
        return [("evidence", self.root)] + [(f"storage-{n}", Path(p)) for n, p in enumerate(self.args.storage_path, 1)]

    def prepare(self):
        a = self.args
        if a.leg == "gluetun" and not a.require_gluetun:
            raise SystemExit("--leg gluetun requires --require-gluetun.")
        hours = 2 if a.leg == "gluetun" else 24
        if self.spec:
            self.prepare_spec()
        elif (a.warmup_minutes < 30 or a.baseline_hours < hours) and not a.rehearsal:
            raise SystemExit(f"Baseline requires at least 30 minutes warmup and {hours} measured hours.")
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
        if (a.stop and self.instance.kind == "native" and self.instance.real_signals
                and self.instance.proc.resolve() != Path("/proc").resolve()):
            raise SystemExit("--stop needs the collector's own /proc: identity and signal must share a PID namespace.")
        try:
            self.instance.attach()
        except (OSError, KeyError, TypeError, AttributeError) as error:
            raise SystemExit(f"Cannot attach to the instance ({type(error).__name__}).") from None
        if a.stop and self.instance.kind == "native" and not self.instance.can_signal():
            raise SystemExit("--stop needs pidfd support (Linux 5.3+ with Python 3.9+) on the target process.")
        if a.require_gluetun and not self.instance.network()["gluetun_network_parent"]:
            raise SystemExit("--require-gluetun: the instance's network parent is not a gluetun container.")
        self.root.mkdir(parents=True, exist_ok=True)
        self.volumes_now = self.volume_reader(self.storage_paths())
        if any(v["free_bytes"] < a.min_free_bytes for v in self.volumes_now):
            raise SystemExit("Insufficient free disk space for soak launch.")
        (self.root / "metrics").mkdir()

    def prepare_spec(self):
        """CR-SPEC run kinds: fixed windows outside rehearsals, and the sources §8 needs."""
        a = self.args
        if a.leg != "main" or a.require_gluetun:
            raise SystemExit("CR-SPEC run kinds have no gluetun leg.")
        if (a.run_kind == "ab") != (a.treatment is not None):
            raise SystemExit("--treatment is required with --run-kind ab, and only with it.")
        if not a.rehearsal and (a.warmup_minutes != SPEC_WARMUP_MINUTES or a.baseline_hours != SPEC_HOURS[a.run_kind]
                                or a.hour_seconds != 3600 or a.sample_seconds != SAMPLE_SECONDS):
            raise SystemExit(f"--run-kind {a.run_kind} measures {SPEC_HOURS[a.run_kind]} h after a "
                             f"{SPEC_WARMUP_MINUTES} min warmup (CR-SPEC rule 1); other values need --rehearsal.")
        if a.baseline_hours != int(a.baseline_hours) or a.hour_seconds < 60:
            raise SystemExit("The window must be whole hours of at least 60 s.")
        echo = urllib.parse.urlsplit(a.ip_echo_url)
        if echo.scheme != "https" and not (a.rehearsal and echo.scheme == "http"):
            raise SystemExit("--ip-echo-url must be https (http only in a rehearsal).")
        if a.run_kind == "comparator":
            if self.instance.kind != "docker" or not a.postgres_container:
                raise SystemExit("--run-kind comparator needs --mode docker and --postgres-container.")
        elif not a.password_file or not Path(a.password_file).is_file():
            raise SystemExit("--password-file is required: the settings and Insights checks need a login.")
        if not a.spec_commit or not COMMIT.fullmatch(a.spec_commit):
            raise SystemExit("--spec-commit must name the commit whose CR-SPEC governs this run (rule 7.2).")
        self.checklist = parse_checklist(a.checklist, a.next_run)
        if not (read_text("/proc/sys/kernel/random/boot_id") or "").strip():
            raise SystemExit("Cannot read the boot ID; host-activity notes couldn't be placed (rule 6.11).")

    def run_json(self):
        a, inst = self.args, self.instance
        stamp = self.now()
        local = stamp.astimezone()
        collector = self.root / "collector.py"
        shutil.copyfile(os.path.abspath(__file__), collector)
        settings = {
            "mode": inst.kind, "leg": a.leg, "warmup_minutes": a.warmup_minutes,
            "baseline_hours": a.baseline_hours, "rehearsal": bool(a.rehearsal),
            "storage_guardrails": {"start_free_bytes": a.min_free_bytes, "warn_free_bytes": a.warn_free_bytes,
                                   "stop_free_bytes": a.stop_free_bytes, "check_seconds": WATCH_SECONDS,
                                   "volumes": self.volumes_now},
            "stop_instance_at_end": bool(a.stop),
            "memory_stop_basis": MEMORY_STOP_BASIS[inst.kind],
        }
        # The preset sets the analyzer's outstanding-RPC limit. Recorded only
        # as the operator states it; absent, the analyzer flags its fallback.
        if a.intensity is not None:
            settings["intensity"] = a.intensity
        parsed = urllib.parse.urlsplit(a.url)
        network = dict(inst.network(), http_host_class=host_class(parsed.hostname), http_port=parsed.port,
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
        if self.spec:
            settings.update(run_kind=a.run_kind, treatment=a.treatment, hour_seconds=a.hour_seconds,
                            sample_seconds=a.sample_seconds, spec_commit=a.spec_commit,
                            spec="docs/decisions/2026-11-02-CR-SPEC.md")
            run["clock"] = {"monotonic_origin": self.started,
                            "boot_id": (read_text("/proc/sys/kernel/random/boot_id") or "").strip() or None,
                            "ntp_start": ntp_status(self.run_command)}
            run["checklist"] = self.checklist
            if a.run_kind == "comparator":
                started = inst.started_utc()
                run["process_started_utc"] = iso(started) if started else None
                run["comparator"] = {"query": COMPARATOR_QUERY, "session": COMPARATOR_ENV.split("=", 1)[1]}
            else:
                env, mounts = inst.environment()
                identity = config_identity(env, mounts) if env is not None else None
                if identity is not None:
                    try:
                        identity["settings_api"] = self.api.settings()
                    except (OSError, ValueError, KeyError, TypeError, AttributeError,
                            http.client.HTTPException):
                        identity["settings_api"] = None
                        self.note("settings API unavailable at start")
                run["config"] = identity
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
        self.started = started
        phase = "startup"
        self.sample = 0
        suppression_seen = False
        self.suppression_end_at = self.baseline_start = baseline_at = end_at = None
        bad_health = bad_metrics = bad_bounds = bad_memory = disk_failures = 0
        self.working_set_over_gib = 0
        anon_basis, memory_reason, fallback_noted = MEMORY_STOP_BASIS[self.instance.kind] == "anon", None, False
        unverified_since = None
        last_completed, last_progress = 0.0, 0.0
        self.verdict = self.start_time = None
        history, volumes = {}, self.volumes_now
        next_sample, next_watch = 5.0, 0.0
        hour_seconds, next_k, ip_retry = a.hour_seconds, 0, None
        horizon = (8 + a.baseline_hours) * 3600 if not self.spec else (
            7 * 3600 + (a.warmup_minutes * 60 + a.baseline_hours * hour_seconds) + 600)
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
                if elapsed >= horizon:
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
                next_sample = elapsed + a.sample_seconds
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
                elapsed = self.clock() - started  # D1a: UTC and elapsed read together
                stamp = self.now()
                values, metrics_ok, text, flags = {}, False, "", {}
                if self.kind == "comparator":
                    health = None
                    count = comparator_count(self.instance.api, a.postgres_container, a.postgres_user, a.postgres_db)
                    if count is None:
                        bad_metrics += 1
                        self.note("comparator count unavailable")
                    else:
                        values, metrics_ok, bad_metrics = {"comparator_torrents": count}, True, 0
                        if baseline_at is None:
                            started_utc = self.instance.started_utc()
                            if started_utc is not None:  # rule 1.2: ws = its process start + 60 min
                                process_elapsed = elapsed - (stamp - started_utc).total_seconds()
                                baseline_at = process_elapsed + a.warmup_minutes * 60
                                end_at = baseline_at + a.baseline_hours * hour_seconds
                                self.suppression_end_at = iso(started_utc)
                    if baseline_at is not None and elapsed >= baseline_at and self.baseline_start is None:
                        self.baseline_start = iso(stamp)
                        self.note(f"window started; collecting {a.baseline_hours:g} hours")
                else:
                    health = self.probe("/healthz")
                    bad_health = 0 if health == 200 else bad_health + 1
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
                if metrics_ok and self.kind != "comparator":
                    start_time = values["start_time_seconds"]
                    if self.start_time is None:
                        self.start_time = start_time
                    elif start_time != self.start_time:  # Lodestar restarted inside the same container
                        self.verdict = Verdict("changed", "process_exited_instance_restarted",
                                               expected_start_time_seconds=self.start_time,
                                               observed_start_time_seconds=start_time)
                        self.reason = self.verdict.reason
                        break
                    snapshot = sanitize_snapshot(text, flags)
                    self.write(self.root / "metrics" / f"{self.sample:05d}.prom", snapshot)
                    reason = suppression_reason(text)
                    if values["dht_sample_suppression_seconds"] > 0:
                        suppression_seen = True
                    if self.spec:  # D6: the first eligible zero, however the run started
                        opened = open_eligible({"dht_sample_suppression_seconds": values["dht_sample_suppression_seconds"],
                                                "suppression_reason": reason},
                                               stamp.timestamp() - values["start_time_seconds"])
                    else:
                        opened = suppression_seen and values["dht_sample_suppression_seconds"] == 0
                    if opened and baseline_at is None:
                        self.suppression_end_at = iso(stamp)
                        baseline_at = elapsed + a.warmup_minutes * 60
                        end_at = baseline_at + a.baseline_hours * hour_seconds
                        self.note(f"sampling suppression ended; warming up for {a.warmup_minutes:g} minutes")
                    if baseline_at is not None and elapsed >= baseline_at and self.baseline_start is None:
                        self.baseline_start = iso(stamp)
                        if not self.spec:  # CR-SPEC's window ends at ws + hours, not t_s + hours (rule 1.4)
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
                working_set = resources.get("working_set_bytes", 0)
                if anon_basis and working_set > GIB:  # evidence only; never a stop in Docker mode
                    if not self.working_set_over_gib:
                        self.note("cgroup working set passed 1 GiB (page cache included); recorded, not a stop")
                    self.working_set_over_gib += 1
                if anon_basis and isinstance(resources.get("private_bytes"), int):
                    memory, sample_reason = resources["private_bytes"], "process_memory_over_1GiB_for_five_samples"
                else:  # native VmRSS, or Docker without anon: fail safe on the working set
                    memory, sample_reason = working_set, "rss_over_1GiB_for_five_samples"
                    if anon_basis and not fallback_noted:
                        fallback_noted = True
                        self.note("cgroup anon unavailable; memory stop rule fell back to working set")
                bad_memory = bad_memory + 1 if memory > GIB else 0
                memory_reason = sample_reason if bad_memory else None
                phase = ("baseline" if self.baseline_start else "warmup" if baseline_at is not None else "startup")
                row = {"schema_version": SCHEMA, "sequence": self.sample, "process_identity": self.instance.identity,
                       "time": iso(stamp), "elapsed_seconds": round(elapsed, 1), "phase": phase,
                       "process_id": self.instance.process_id, "health": health, "metrics_ok": metrics_ok,
                       **resources, **self.instance.row_extra(), "volumes": volumes, "values": values}
                family = ip_family_counters(text) if metrics_ok else {}
                if family:
                    row["ip_family_counters"] = family
                if self.spec:
                    if health is None:
                        row.pop("health")
                    if metrics_ok and self.kind != "comparator":
                        row["suppression_reason"] = reason
                    if flags:
                        row.update(flags)
                    net = net_counters(self.instance.proc, self.instance.process_id)
                    if net is not None:
                        row["net"] = net
                    if self.ip.reference is None or ip_retry is not None:  # the reference, or a boundary retry
                        row["public_ip"] = self.ip.check()
                        if row["public_ip"] != "failed" or (ip_retry is not None and elapsed > ip_retry):
                            ip_retry = None
                    passed = []
                    while (baseline_at is not None and metrics_ok and next_k <= a.baseline_hours
                           and elapsed >= baseline_at + next_k * hour_seconds):
                        passed.append(next_k)
                        next_k += 1
                    config = self.run.get("config")
                    if metrics_ok and isinstance(config, dict) and config.get("settings_api") is None:
                        try:  # identity's preset: retried until Lodestar's API answers
                            config["settings_api"] = dict(self.api.settings(), read_at_elapsed=round(elapsed, 1))
                            self.save(self.run, self.root / "run.json")
                        except (OSError, ValueError, KeyError, TypeError, AttributeError,
                                http.client.HTTPException):
                            pass
                    if passed:  # rule 1.4's boundary scrape: public IP, imports, settings
                        row["boundary"] = passed
                        if "public_ip" not in row:
                            row["public_ip"] = self.ip.check()
                        ip_retry = (baseline_at + passed[-1] * hour_seconds + BOUNDARY_SECONDS
                                    if row["public_ip"] == "failed" else None)
                        if self.kind != "comparator":
                            row["api"] = self.api_checks(values["start_time_seconds"], stamp)
                self.write(self.root / "samples.jsonl", dumps(row), append=True)
                shown = row
                if self.spec:  # rule 6.11: progress and checks only, never counts or rates
                    shown = {k: v for k, v in row.items()
                             if k not in ("values", "net", "api", "ip_family_counters", "volumes")}
                    shown["window"] = {"opened": self.suppression_end_at is not None,
                                       "boundaries_passed": next_k, "hours": a.baseline_hours}
                self.save({"state": "running", "phase": phase, "sample": self.sample, "latest": shown,
                           **self.memory_evidence(),
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
                    self.reason = memory_reason
                    break
                if (metrics_ok and self.kind != "comparator" and values["dht_peer_active"] > 0
                        and elapsed - last_progress >= 300):
                    self.reason = "active_peer_requests_without_completion_for_five_minutes"
                    break
                if endpoint_reached(elapsed, end_at, metrics_ok and health in (200, None), self.baseline_start):
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

    def api_checks(self, start_time, stamp):
        """Rule 5.5: imports and preset changes since the process started, and the
        settings now. A failure is recorded as unavailable, never as a pass."""
        try:
            return {"imports": self.api.imports(start_time - 60, stamp.timestamp() + 60),
                    "settings": self.api.settings()}
        except (OSError, ValueError, KeyError, TypeError, AttributeError, http.client.HTTPException):
            self.note("settings or Insights API unavailable at a boundary")
            return {"error": "unavailable"}

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
        result.update(self.memory_evidence())
        if self.spec:
            result["ntp_end"] = ntp_status(self.run_command)
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
        close = getattr(self.instance, "close", None)
        if close:
            close()

    def memory_evidence(self):
        if MEMORY_STOP_BASIS[self.instance.kind] != "anon":
            return {}
        return {"working_set_over_1GiB_samples": getattr(self, "working_set_over_gib", 0)}

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
    p.add_argument("--proc-root", default="/proc",
                   help="proc mount to read (native mode; docker mode: the host's, for network counters)")
    p.add_argument("--url", default="http://127.0.0.1:8484", help="Lodestar base URL, as this collector reaches it")
    p.add_argument("--root", required=True, help="New, empty evidence directory")
    p.add_argument("--commit", required=True, help="Source commit under test (lowercase hex)")
    p.add_argument("--warmup-minutes", type=float, default=None,
                   help="Default 30 (cr0) or 60 (CR-SPEC kinds, fixed outside rehearsals)")
    p.add_argument("--baseline-hours", type=float, default=None,
                   help="Default 24 (cr0, comb, comparator) or 6 (ab)")
    p.add_argument("--run-kind", choices=("cr0",) + SPEC_KINDS, default="cr0",
                   help="cr0: the v0.5.1 soak. ab, comb, comparator: a CR-SPEC crawler run")
    p.add_argument("--treatment", choices=sorted(TREATMENTS), help="The A/B's treatment (--run-kind ab)")
    p.add_argument("--spec-commit", help="Commit of docs/decisions/2026-11-02-CR-SPEC.md in force (CR-SPEC kinds)")
    p.add_argument("--password-file", help="Admin password for the settings and Insights checks (CR-SPEC kinds)")
    p.add_argument("--ip-echo-url", default=DEFAULT_IP_ECHO, help="Fixed IP-echo service for the public-IP check")
    p.add_argument("--checklist", action="append", default=[], metavar="ITEM=STATE",
                   help="Host checklist: auto_update or backup = excluded or not_applicable (repeatable)")
    p.add_argument("--next-run", action="append", default=[], metavar="ITEM=UTC",
                   help="An excluded item's next scheduled run, in UTC (repeatable)")
    p.add_argument("--postgres-container", help="bitmagnet's Postgres container (--run-kind comparator)")
    p.add_argument("--postgres-user", default="postgres")
    p.add_argument("--postgres-db", default="bitmagnet")
    p.add_argument("--hour-seconds", type=float, default=3600.0, help=argparse.SUPPRESS)  # rehearsal only
    p.add_argument("--sample-seconds", type=float, default=SAMPLE_SECONDS, help=argparse.SUPPRESS)
    p.add_argument("--rehearsal", action="store_true", help="Allow shorter windows; marked in run.json")
    p.add_argument("--storage-path", action="append", default=[], help="Extra path whose filesystem to guard (repeatable)")
    p.add_argument("--min-free-bytes", type=int, default=50 * GIB)
    p.add_argument("--warn-free-bytes", type=int, default=20 * GIB)
    p.add_argument("--stop-free-bytes", type=int, default=10 * GIB)
    p.add_argument("--stop", action="store_true", help="Stop the verified instance when collection ends")
    p.add_argument("--leg", choices=("main", "gluetun"), default="main",
                   help="gluetun: the 2 h gluetun leg (needs --require-gluetun; baseline minimum 2 h)")
    p.add_argument("--require-gluetun", action="store_true", help="Refuse to start unless gluetun is the network parent")
    p.add_argument("--intensity", choices=("low", "balanced", "aggressive"),
                   help="The intensity preset Lodestar runs at, recorded in run.json for the analyzer")
    p.add_argument("--user-timezone", default="America/Denver")
    p.add_argument("--tick-seconds", type=float, default=1.0, help=argparse.SUPPRESS)
    return p


def make_instance(args, parser):
    if args.mode == "docker":
        if not args.container:
            parser.error("--container is required in docker mode")
        return DockerInstance(DockerAPI(args.docker_socket), args.container, args.cgroup_root, args.cgroup_path,
                              proc_root=args.proc_root)
    if args.pid is None:
        parser.error("--pid is required in native mode")
    return NativeInstance(args.pid, args.proc_root)


def main(argv=None):
    argv = sys.argv[1:] if argv is None else list(argv)
    if argv[:1] == ["note"]:
        return note_command(argv[1:])
    parser = build_parser()
    args = resolve_defaults(parser.parse_args(argv))
    for name in ("warmup_minutes", "baseline_hours", "tick_seconds", "hour_seconds", "sample_seconds"):
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
