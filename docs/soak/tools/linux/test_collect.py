"""Tests for collect.py: fake HTTP on 127.0.0.1, fake cgroup and /proc trees, virtual clock.

No test sleeps for a fixed time. The collector runs on a virtual clock, and the
two subprocess tests poll for a condition with a 30 s deadline.
"""

import json
import os
import signal
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

import analyze_cr0  # noqa: E402
import collect  # noqa: E402

GIB = 2**30
CONTAINER_ID = "c" * 64
IMAGE_ID = "sha256:" + "a" * 64
COMMIT = "b" * 40
SECRETS = ("Ubuntu 24.04 ISO", "203.0.113.9", "s3cr3t-token", "0123456789abcdef0123456789abcdef01234567")


def wait_for(condition, what, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if condition():
            return
        time.sleep(0.05)  # pacing a poll, not assuming work has finished
    raise AssertionError(f"timed out waiting for {what}")


class FakeClock:
    """Virtual monotonic clock; sleeping advances it and fires scheduled hooks."""

    def __init__(self):
        self.t = 0.0
        self.base = datetime(2026, 11, 1, tzinfo=timezone.utc)
        self.hooks = []

    def clock(self):
        return self.t

    def now(self):
        return self.base + timedelta(seconds=self.t)

    def sleep(self, seconds):
        self.t += seconds
        for hook in list(self.hooks):
            when, fn = hook
            if self.t >= when:
                self.hooks.remove(hook)
                fn()

    def at(self, when, fn):
        self.hooks.append((when, fn))


def metrics_text(t, suppression_until=100, stuck=False, extra=""):
    vals = {name: 0 for name in collect.REQUIRED}
    vals.update(start_time_seconds=1000, torrents=int(t // 60), dht_sample_suppression_seconds=max(0, suppression_until - t),
                dht_peer_active=3, dht_lookups_completed_total=0 if stuck else int(t),
                dht_peer_admitted_total=int(t * 2))
    lines = ["# HELP lodestar_torrents Torrents stored."] + [f"lodestar_{k} {v:g}" for k, v in vals.items()]
    lines += [f"lodestar_writer_stored_total {int(t // 10)}", f"lodestar_fetch_attempts_total {int(t // 5)}",
              f"lodestar_fetch_success_total {int(t // 10)}", f"lodestar_writer_errors_total 0",
              'lodestar_fetch_failures_total{kind="timed_out"} ' + str(int(t // 30)),
              'lodestar_fetch_failures_total{kind="203.0.113.9"} 1',
              'lodestar_torrent_info{name="Ubuntu 24.04 ISO",hash="0123456789abcdef0123456789abcdef01234567"} 1',
              'lodestar_peer_seen{addr="203.0.113.9:6881"} 1', "lodestar_unknown_thing 5"]
    return "\n".join(lines) + "\n" + extra


class FakeLodestar:
    """HTTP server on 127.0.0.1 whose socket stays bound for the test's lifetime."""

    def __init__(self, clock):
        self.clock = clock
        self.health = 200
        self.metrics = lambda t: (200, metrics_text(t).encode())
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                status, body = outer.respond(self.path)
                if status is None:
                    self.close_connection = True
                    self.connection.close()
                    return
                self.send_response(status)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def respond(self, path):
        if path == "/healthz":
            return (self.health, b"ok") if self.health else (None, b"")
        if path == "/metrics":
            return self.metrics(self.clock.t)
        return 404, b""

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class FakeDocker:
    def __init__(self, parent_image=None):
        self.calls = []
        self.fail = False
        self.container = {
            "Id": CONTAINER_ID, "Image": IMAGE_ID, "RestartCount": 0,
            "State": {"Running": True, "StartedAt": "2026-11-01T00:00:00Z", "ExitCode": 0, "Pid": 4242,
                      "Health": {"Status": "healthy"}},
            "HostConfig": {"NetworkMode": "bridge"},
            "Config": {"Image": "lodestar:test", "Env": ["LODESTAR_SETUP_TOKEN=s3cr3t-token"],
                       "Cmd": ["serve", "--token", "s3cr3t-token"]},
        }
        self.parent = None
        if parent_image:
            self.parent = {"Id": "d" * 64, "Image": "sha256:" + "e" * 64,
                           "State": {"Running": True, "StartedAt": "2026-11-01T00:00:00Z"},
                           "Config": {"Image": parent_image, "Env": ["VPN_PASSWORD=s3cr3t-token"]}}
            self.container["HostConfig"]["NetworkMode"] = "container:gluetun"
        self.stop_exit_code = 0

    def get(self, path):
        self.calls.append(("GET", path))
        if self.fail:
            raise ConnectionRefusedError("docker api down")
        if path.startswith("/containers/gluetun/") and self.parent:
            return 200, self.parent
        if path.startswith(f"/containers/{CONTAINER_ID}/stats"):
            return 200, {"memory_stats": {"usage": 300, "stats": {"inactive_file": 100, "anon": 150}},
                         "cpu_stats": {"cpu_usage": {"total_usage": 2_000_000_000}}, "pids_stats": {"current": 9}}
        if path.startswith("/containers/") and path.endswith("/json"):
            if self.container is None:
                return 404, {"message": "no such container"}
            name = path.split("/")[2]
            return (200, self.container) if name in ("lodestar", CONTAINER_ID) else (404, {})
        if path.startswith("/images/"):
            return 200, {"RepoDigests": ["lodestar@sha256:" + "f" * 64]}
        return 404, {}

    def post(self, path, timeout=None):
        self.calls.append(("POST", path))
        self.post_timeout = timeout
        if path.startswith(f"/containers/{CONTAINER_ID}/stop"):
            self.container["State"].update(Running=False, ExitCode=self.stop_exit_code)
            return 204, None
        return 404, None

    def posts(self):
        return [c for c in self.calls if c[0] == "POST"]


def write_cgroup(root, memory=400, inactive=100, anon=250, usage_usec=3_000_000, pids=11):
    base = Path(root) / "system.slice" / f"docker-{CONTAINER_ID}.scope"
    base.mkdir(parents=True, exist_ok=True)
    (Path(root) / "cgroup.controllers").write_text("cpu memory pids\n")
    (base / "memory.current").write_text(f"{memory}\n")
    (base / "memory.stat").write_text(f"anon {anon}\nfile 123\ninactive_file {inactive}\n")
    (base / "cpu.stat").write_text(f"usage_usec {usage_usec}\nuser_usec 1\n")
    (base / "pids.current").write_text(f"{pids}\n")
    return base


def write_proc(root, pid=777, start=5000, comm="lodestar", state="S", rss_kb=2048, anon_kb=1024, threads=7, fds=4):
    base = Path(root) / str(pid)
    base.mkdir(parents=True, exist_ok=True)
    tail = [state, "1", "1", "1", "0", "-1", "0", "0", "0", "0", "0", "300", "100", "0", "0", "20", "0",
            str(threads), "0", str(start), "0"]
    (base / "stat").write_text(f"{pid} ({comm}) " + " ".join(tail) + "\n")
    (base / "status").write_text(f"Name:\t{comm}\nVmRSS:\t{rss_kb} kB\nRssAnon:\t{anon_kb} kB\nThreads:\t{threads}\n")
    fd = base / "fd"
    fd.mkdir(exist_ok=True)
    for n in range(fds):
        (fd / str(n)).write_text("")
    exe = Path(root) / "lodestar-binary"
    exe.write_bytes(b"binary")
    link = base / "exe"
    if not link.exists():
        link.symlink_to(exe)
    boot = Path(root) / "sys/kernel/random/boot_id"
    boot.parent.mkdir(parents=True, exist_ok=True)
    boot.write_text("boot-1\n")
    return base


def args_for(root, *extra, mode="docker", **kw):
    argv = ["--mode", mode, "--root", str(root), "--commit", COMMIT, "--rehearsal", "--warmup-minutes", "1",
            "--baseline-hours", "0.05", "--min-free-bytes", "3", "--warn-free-bytes", "2",
            "--stop-free-bytes", "1"]
    for key, value in kw.items():
        argv += [f"--{key.replace('_', '-')}", str(value)]
    return collect.build_parser().parse_args(argv + list(extra))


def fixed_volumes(free=100 * GIB):
    return lambda paths: [{"volume": "evidence", "free_bytes": free, "total_bytes": 200 * GIB}]


class Base(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.tmp = Path(self.temp.name)
        self.root = self.tmp / "run"
        self.cgroup = self.tmp / "cgroup"
        self.clock = FakeClock()
        self.lodestar = FakeLodestar(self.clock)
        self.addCleanup(self.lodestar.close)

    def docker(self, **kw):
        write_cgroup(self.cgroup)
        self.api = FakeDocker(**kw)
        return collect.DockerInstance(self.api, "lodestar", self.cgroup, clock=self.clock.clock, sleep=self.clock.sleep)

    def collector(self, instance, *extra, volumes=None, **kw):
        args = args_for(self.root, "--url", self.lodestar.url, *extra, **kw)
        return collect.Collector(args, instance, clock=self.clock.clock, sleep=self.clock.sleep, now=self.clock.now,
                                 volumes=volumes or fixed_volumes())

    def run_docker(self, *extra, **kw):
        c = self.collector(self.docker(), *extra, **kw)
        return c, c.run()

    def result(self):
        return json.loads((self.root / "result.json").read_text())

    def rows(self):
        return [json.loads(line) for line in (self.root / "samples.jsonl").read_text().splitlines()]

    def report(self):
        return analyze_cr0.analyze(self.root, now=self.clock.now())


class CleanRunTests(Base):
    def test_docker_run_is_read_only_and_analyzer_reads_it(self):
        c, rc = self.run_docker()
        self.assertEqual(rc, 0)
        self.assertEqual(self.api.posts(), [], "an observer must never stop the container")
        result = self.result()
        self.assertEqual(result["reason"], "baseline_window_complete")
        self.assertEqual(result["stop_mode"], "left_running")
        self.assertTrue(result["finalization_complete"])
        rows = self.rows()
        self.assertEqual([r["sequence"] for r in rows], list(range(len(rows))))
        self.assertEqual({r["phase"] for r in rows}, {"startup", "warmup", "baseline"})
        self.assertTrue(all(r["process_identity"] == f"{CONTAINER_ID}:2026-11-01T00:00:00Z" for r in rows))
        self.assertEqual(rows[0]["working_set_bytes"], 300)  # memory.current minus inactive_file
        self.assertEqual(rows[0]["private_bytes"], 250)
        self.assertEqual(rows[0]["threads"], 11)
        self.assertEqual(rows[0]["cpu_seconds"], 3.0)
        self.assertNotIn("handles", rows[0], "a field this host cannot provide stays absent")
        self.assertEqual(rows[0]["process_id"], 4242)
        for row in rows:
            self.assertTrue((self.root / "metrics" / f"{row['sequence']:05d}.prom").exists())
        self.assertTrue((self.root / "baseline-start.prom").exists())
        report = self.report()
        self.assertEqual(report["baseline_status"], "collector_window_complete_review_required")
        self.assertEqual(report["issues"], {})
        self.assertEqual(report["run_state"], "completion_unknown")
        self.assertFalse(report["clean_shutdown"], "no --stop, so no clean-shutdown evidence")
        base = report["phases"]["baseline"]
        self.assertGreater(base["counters"]["writer_stored_total"]["delta"], 0)
        self.assertEqual(base["counters"]["dht_peer_admitted_total"]["covered_seconds"], base["valid_interval_seconds"])

    def test_run_json_records_identity_and_network(self):
        self.run_docker()
        run = json.loads((self.root / "run.json").read_text())
        self.assertEqual(run["container_id"], CONTAINER_ID)
        self.assertEqual(run["sha256"], "a" * 64)
        self.assertEqual(run["sha256_kind"], "image_id")
        self.assertEqual(run["image_digest"], "sha256:" + "f" * 64)
        self.assertEqual(run["settings"]["leg"], "main")
        self.assertEqual(run["network"]["http_host_class"], "loopback")
        self.assertEqual(run["commit"], COMMIT)
        self.assertEqual(run["network"]["mode"], "bridge")
        self.assertFalse(run["network"]["host_network"])
        self.assertFalse(run["network"]["gluetun_network_parent"])
        self.assertEqual(run["settings"]["baseline_hours"], 0.05)
        self.assertTrue(run["settings"]["rehearsal"])
        self.assertEqual(run["collector_sha256"], collect.sha256_file(self.root / "collector.py"))
        self.assertEqual((self.root / "collector.py").read_bytes(), Path(collect.__file__).read_bytes())

    def test_stop_flag_stops_verified_container_and_records_clean_shutdown(self):
        c, rc = self.run_docker("--stop")
        self.assertEqual(rc, 0)
        self.assertEqual(len(self.api.posts()), 1)
        self.assertGreaterEqual(self.api.post_timeout, 90, "stop blocks until exit; it needs a long timeout")
        result = self.result()
        self.assertEqual(result["stop_mode"], "exited_0")
        self.assertTrue(result["shutdown"]["signal_delivered"])
        report = self.report()
        self.assertEqual(report["run_state"], "stopped")
        self.assertTrue(report["clean_shutdown"])

    def test_forced_stop_is_not_a_clean_shutdown(self):
        instance = self.docker()
        instance.api.stop_exit_code = 137
        c = self.collector(instance, "--stop")
        self.assertEqual(c.run(), 1)
        self.assertEqual(self.result()["stop_mode"], "exited_137")
        self.assertFalse(self.report()["clean_shutdown"])

    def test_full_production_window_on_virtual_clock(self):
        args = collect.build_parser().parse_args(
            ["--mode", "docker", "--root", str(self.root), "--commit", COMMIT, "--url", self.lodestar.url,
             "--min-free-bytes", "3", "--warn-free-bytes", "2", "--stop-free-bytes", "1"])
        self.assertEqual((args.warmup_minutes, args.baseline_hours), (30, 24))
        direct = lambda url: self.lodestar.respond(url.rsplit("/", 1)[1] and "/" + url.rsplit("/", 1)[1])
        c = collect.Collector(args, self.docker(), clock=self.clock.clock, sleep=self.clock.sleep, now=self.clock.now,
                              fetch=direct, volumes=fixed_volumes())
        self.assertEqual(c.run(), 0)
        report = self.report()
        self.assertEqual(report["baseline_status"], "collector_window_complete_review_required")
        self.assertEqual(report["issues"], {})
        self.assertTrue(report["collection_quality"]["full_duration_observed"])
        self.assertTrue(report["collection_quality"]["gap_limit_met"])
        self.assertGreaterEqual(report["baseline_coverage_fraction"], 0.99)


class FaultInjectionTests(Base):
    def test_short_health_failures_are_recorded_and_survived(self):
        self.clock.at(60, lambda: setattr(self.lodestar, "health", 503))
        self.clock.at(130, lambda: setattr(self.lodestar, "health", 200))
        c, rc = self.run_docker()
        self.assertEqual(rc, 0)
        failed = [r for r in self.rows() if r["health"] != 200]
        self.assertEqual(len(failed), 2)
        phases = self.report()["phases"]
        self.assertEqual(sum(p["health_failed_samples"] for p in phases.values()), 2)

    def test_three_health_failures_end_the_run(self):
        self.clock.at(60, lambda: setattr(self.lodestar, "health", 0))  # connection dropped
        c, rc = self.run_docker()
        self.assertEqual(rc, 1)
        self.assertEqual(self.result()["reason"], "three_consecutive_health_failures")
        self.assertEqual([r["health"] for r in self.rows()][-3:], [0, 0, 0])

    def test_missing_and_malformed_metrics_fail_the_sample_not_the_run(self):
        bad = [lambda t: (500, b""), lambda t: (200, b"lodestar_torrents 1\n"),
               lambda t: (200, b"\xff\xfe not utf8"), lambda t: (200, metrics_text(t).replace("lodestar_torrents 1", "lodestar_torrents 1.2.3"))]
        calls = []

        def metrics(t):
            if 100 <= t < 100 + 60 * 2:
                calls.append(t)
                return bad[len(calls) % len(bad)](t)
            return 200, metrics_text(t).encode()
        self.lodestar.metrics = metrics
        c, rc = self.run_docker()
        self.assertEqual(rc, 0)
        failed = [r for r in self.rows() if not r["metrics_ok"]]
        self.assertEqual(len(failed), 2)
        for row in failed:
            self.assertEqual(row["values"], {})
            self.assertFalse((self.root / "metrics" / f"{row['sequence']:05d}.prom").exists())
        report = self.report()
        self.assertEqual(sum(p["metrics_failed_samples"] for p in report["phases"].values()), 2)

    def test_three_metrics_failures_end_the_run(self):
        self.lodestar.metrics = lambda t: (200, b"garbage\n")
        c, rc = self.run_docker()
        self.assertEqual(rc, 1)
        self.assertEqual(self.result()["reason"], "three_consecutive_metrics_failures")

    def test_identity_gap_skips_samples_and_the_analyzer_sees_the_gap(self):
        instance = self.docker()
        self.clock.at(100, lambda: setattr(self.api, "fail", True))
        self.clock.at(190, lambda: setattr(self.api, "fail", False))
        c = self.collector(instance)
        self.assertEqual(c.run(), 0)
        rows = self.rows()
        self.assertEqual([r["sequence"] for r in rows], list(range(len(rows))))
        gaps = [b["elapsed_seconds"] - a["elapsed_seconds"] for a, b in zip(rows, rows[1:])]
        self.assertGreater(max(gaps), 90)
        report = self.report()
        self.assertGreaterEqual(report["issues"].get("sampling_gaps", 0), 1)
        self.assertNotIn("identity_changes", report["issues"])

    def test_unverifiable_identity_for_three_minutes_ends_the_run(self):
        instance = self.docker()
        self.clock.at(100, lambda: setattr(self.api, "fail", True))
        c = self.collector(instance)
        self.assertEqual(c.run(), 1)
        self.assertEqual(self.result()["reason"], "collector_error")

    def test_stuck_peer_requests_end_the_run(self):
        self.lodestar.metrics = lambda t: (200, metrics_text(t, stuck=True).encode())
        c, rc = self.run_docker()
        self.assertEqual(rc, 1)
        self.assertEqual(self.result()["reason"], "active_peer_requests_without_completion_for_five_minutes")

    def test_capacity_violations_end_the_run(self):
        self.lodestar.metrics = lambda t: (200, metrics_text(t).replace("lodestar_dht_outstanding 0", "lodestar_dht_outstanding 2000").encode())
        c, rc = self.run_docker()
        self.assertEqual(rc, 1)
        self.assertEqual(self.result()["reason"], "three_consecutive_capacity_violations")

    def test_rss_over_one_gib_ends_the_run(self):
        instance = self.docker()
        write_cgroup(self.cgroup, memory=2 * GIB + 100, inactive=100)
        c = self.collector(instance)
        self.assertEqual(c.run(), 1)
        self.assertEqual(self.result()["reason"], "rss_over_1GiB_for_five_samples")

    def test_disk_reserve_and_monitor_failure(self):
        launched = []

        def volumes(paths):  # plenty at launch, then the reserve is reached
            free = 100 * GIB if not launched else 0
            launched.append(free)
            return fixed_volumes(free=free)(paths)
        self.assertEqual(self.collector(self.docker(), volumes=volumes).run(), 1)
        self.assertEqual(self.result()["reason"], "disk_reserve_reached")
        self.assertEqual(self.report()["disk"]["minimum_free_bytes"], 0)

    def test_disk_monitor_failure_after_launch(self):
        state = {"n": 0}

        def volumes(paths):
            state["n"] += 1
            if state["n"] > 1:
                raise OSError("statvfs failed")
            return fixed_volumes()(paths)
        c = self.collector(self.docker(), volumes=volumes)
        self.assertEqual(c.run(), 1)
        self.assertEqual(self.result()["reason"], "disk_monitor_failed")

    def test_launch_refused_without_free_space(self):
        with self.assertRaises(SystemExit):
            self.collector(self.docker(), volumes=fixed_volumes(free=1)).run()
        self.assertFalse((self.root / "run.json").exists())

    def test_evidence_write_error_still_publishes_result(self):
        real = collect.append_line

        def failing(path, line):
            if Path(path).name == "samples.jsonl":
                raise OSError("disk full")
            return real(path, line)
        with mock.patch.object(collect, "append_line", failing):
            c, rc = self.run_docker()
        self.assertEqual(rc, 1)
        self.assertEqual(self.result()["reason"], "evidence_write_error")

    def test_result_publication_failure_is_reported_and_status_marked(self):
        saved = {}

        def save(value, path):
            if Path(path).name == "result.json":
                raise OSError("read-only")
            saved[Path(path).name] = json.loads(json.dumps(value))
        result = {"reason": "x"}
        failures = collect.complete_evidence(result, self.tmp / "status.json", self.tmp / "result.json", save)
        self.assertEqual(failures, 1)
        self.assertEqual(result["publication_failures"], ["result"])
        self.assertFalse(result["finalization_complete"])
        self.assertEqual(saved["status.json"]["publication_failures"], ["result"])

    def test_atomic_write_keeps_previous_content_and_leaves_no_temp(self):
        path = self.tmp / "status.json"
        collect.save_json({"v": 1}, path)
        with mock.patch.object(os, "replace", side_effect=OSError("locked")):
            with self.assertRaises(OSError):
                collect.save_json({"v": 2}, path)
        self.assertEqual(json.loads(path.read_text()), {"v": 1})
        self.assertEqual([p.name for p in self.tmp.iterdir() if p.name.startswith(".")], [])


class InterruptionTests(Base):
    def test_interrupted_run_without_stop_leaves_exit_unknown(self):
        c = self.collector(self.docker())
        self.clock.at(300, lambda: c.request_stop("SIGTERM"))
        self.assertEqual(c.run(), 1)
        result = self.result()
        self.assertEqual(result["reason"], "operator_stop")
        self.assertEqual(result["stop_signal"], "SIGTERM")
        self.assertEqual(self.api.posts(), [])
        report = self.report()
        self.assertEqual(report["stop_reason"], "operator_stop")
        self.assertEqual(report["baseline_status"], "interruption_exit_unknown")

    def test_interrupted_after_baseline_start_with_stop(self):
        c = self.collector(self.docker(), "--stop")
        self.clock.at(330, lambda: c.request_stop("SIGINT"))
        self.assertEqual(c.run(), 1)
        report = self.report()
        self.assertEqual(report["baseline_status"], "interrupted")
        self.assertEqual(report["run_state"], "stopped")
        self.assertEqual(report["issues"], {})
        self.assertEqual(len(self.api.posts()), 1)

    def test_stop_request_file(self):
        c = self.collector(self.docker())
        self.clock.at(60, lambda: (self.root / "stop.request").write_text(""))
        self.assertEqual(c.run(), 1)
        self.assertEqual(self.result()["reason"], "operator_stop")

    def _signal_subprocess(self, signum):
        proc_root = self.tmp / "proc"
        pid = os.getpid()
        write_proc(proc_root, pid=pid, comm="python3")
        (proc_root / str(pid) / "stat").write_text((proc_root / str(pid) / "stat").read_text())
        cmd = [sys.executable, "-I", str(HERE / "collect.py"), "--mode", "native", "--pid", str(pid),
               "--proc-root", str(proc_root), "--root", str(self.root), "--commit", COMMIT,
               "--url", self.lodestar.url, "--rehearsal", "--min-free-bytes", "3", "--warn-free-bytes", "2",
               "--stop-free-bytes", "1"]
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(lambda: proc.poll() is None and proc.kill())
        wait_for(lambda: (self.root / "run.json").exists(), "run.json")
        proc.send_signal(signum)
        out, err = proc.communicate(timeout=60)
        self.assertEqual(proc.returncode, 1, err.decode())
        result = self.result()
        self.assertEqual(result["reason"], "operator_stop")
        self.assertEqual(result["stop_signal"], signal.Signals(signum).name)
        self.assertEqual(result["stop_mode"], "left_running")
        self.assertTrue(result["finalization_complete"])
        rows = [l for l in (self.root / "samples.jsonl").read_text().splitlines() if l]
        self.assertEqual(result["samples"], len(rows))
        analyze_cr0.analyze(self.root)

    def test_sigterm_writes_result(self):
        self._signal_subprocess(signal.SIGTERM)

    def test_sigint_writes_result(self):
        self._signal_subprocess(signal.SIGINT)


class IdentityTests(Base):
    def test_container_restart_is_flagged_never_stitched(self):
        instance = self.docker()

        def restart():
            self.api.container["State"]["StartedAt"] = "2026-11-01T00:03:00Z"
            self.api.container["RestartCount"] = 1
        self.clock.at(200, restart)
        c = self.collector(instance, "--stop")
        self.assertEqual(c.run(), 1)
        result = self.result()
        self.assertEqual(result["reason"], "process_exited_identity_changed")
        self.assertEqual(self.api.posts(), [], "never stop a container whose identity changed")
        self.assertEqual(result["identity_change"]["expected"], f"{CONTAINER_ID}:2026-11-01T00:00:00Z")
        self.assertEqual({r["process_identity"] for r in self.rows()}, {f"{CONTAINER_ID}:2026-11-01T00:00:00Z"})
        report = self.report()
        self.assertEqual(report["run_state"], "completion_unknown")
        self.assertEqual(report["stop_reason"], "process_exited")

    def test_container_exit_records_code_and_oom(self):
        instance = self.docker()

        def die():
            self.api.container["State"].update(Running=False, ExitCode=137, OOMKilled=True)
        self.clock.at(200, die)
        c = self.collector(instance)
        self.assertEqual(c.run(), 1)
        result = self.result()
        self.assertEqual(result["reason"], "process_exited_137")
        self.assertTrue(result["oom_killed"])
        self.assertEqual(result["stop_mode"], "already_exited_137")
        self.assertEqual(self.report()["run_state"], "stopped")

    def test_container_removed(self):
        instance = self.docker()
        self.clock.at(200, lambda: setattr(self.api, "container", None))
        self.assertEqual(self.collector(instance).run(), 1)
        self.assertEqual(self.result()["reason"], "process_exited_container_removed")

    def test_pid_reuse_is_flagged_and_never_signalled(self):
        proc = self.tmp / "proc"
        write_proc(proc, pid=777, start=5000)
        kills = []
        instance = collect.NativeInstance(777, proc, clock=self.clock.clock, sleep=self.clock.sleep,
                                          opener=lambda pid: None, signaller=kills.append, clk_tck=100)
        self.clock.at(200, lambda: write_proc(proc, pid=777, start=9999))  # same pid, new process
        c = self.collector(instance, "--stop")
        c.args.mode = "native"
        self.assertEqual(c.run(), 1)
        self.assertEqual(self.result()["reason"], "process_exited_identity_changed")
        self.assertEqual(kills, [])
        self.assertEqual(len({r["process_identity"] for r in self.rows()}), 1)

    def test_native_process_gone(self):
        proc = self.tmp / "proc"
        write_proc(proc, pid=777)
        instance = collect.NativeInstance(777, proc, clock=self.clock.clock, sleep=self.clock.sleep, clk_tck=100)
        self.clock.at(200, lambda: (proc / "777" / "stat").unlink())
        self.assertEqual(self.collector(instance).run(), 1)
        result = self.result()
        self.assertEqual(result["reason"], "process_exited_unknown")
        self.assertIsNone(result["shutdown"]["exit_code"])
        self.assertEqual(result["stop_mode"], "already_exited_unknown")


class NativeTests(Base):
    def instance(self, kills, **kw):
        proc = self.tmp / "proc"
        write_proc(proc, **kw)
        return collect.NativeInstance(777, proc, clock=self.clock.clock, sleep=self.clock.sleep, opener=lambda pid: None,
                                      signaller=kills.append, clk_tck=100, stop_timeout=60)

    def test_pid_plus_start_time_identity_and_proc_resources(self):
        kills = []
        inst = self.instance(kills)
        inst.attach()
        self.assertEqual(inst.identity, "777:boot-1:5000")
        res = inst.resources()
        self.assertEqual(res, {"working_set_bytes": 2048 * 1024, "private_bytes": 1024 * 1024, "threads": 7,
                               "cpu_seconds": 4.0, "handles": 4})
        self.assertEqual(inst.describe()["sha256"], collect.sha256_file(self.tmp / "proc" / "lodestar-binary"))

    def test_run_never_signals_without_stop(self):
        kills = []
        c = self.collector(self.instance(kills))
        self.assertEqual(c.run(), 0)
        self.assertEqual(kills, [])
        run = json.loads((self.root / "run.json").read_text())
        self.assertEqual(run["network"], {**run["network"], "mode": "native_host", "gluetun_network_parent": False})
        self.assertEqual(run["process_identity"], "777:boot-1:5000")
        self.assertEqual(self.report()["issues"], {})

    def test_stop_sends_term_after_identity_check(self):
        kills = []
        inst = self.instance(kills)
        inst.signaller = lambda sig: (kills.append(sig), (self.tmp / "proc" / "777" / "stat").unlink())
        c = self.collector(inst, "--stop")
        self.assertEqual(c.run(), 1)  # exit code of a non-child is unknown, so never "clean"
        self.assertEqual(kills, [signal.SIGTERM])
        shutdown = self.result()["shutdown"]
        self.assertEqual((shutdown["stop_mode"], shutdown["exit_confirmed"], shutdown["signal_delivered"]),
                         ("exited_unknown", True, True))
        self.assertFalse(self.report()["clean_shutdown"])

    def test_stop_escalates_to_kill_only_after_reverifying_identity(self):
        kills = []
        inst = self.instance(kills)
        inst.attach()

        def kill(sig):
            kills.append(sig)
            if sig == signal.SIGKILL:
                (self.tmp / "proc" / "777" / "stat").unlink()
        inst.signaller = kill
        out = inst.stop()
        self.assertEqual(kills, [signal.SIGTERM, signal.SIGKILL])
        self.assertEqual(out["stop_mode"], "forced_kill")
        self.assertTrue(out["exit_confirmed"])

    def test_stop_does_not_escalate_after_term_when_the_pid_was_reused(self):
        kills = []
        inst = self.instance(kills)
        inst.attach()

        def kill(sig):
            kills.append(sig)
            write_proc(self.tmp / "proc", pid=777, start=1)  # pid reused after SIGTERM
        inst.signaller = kill
        out = inst.stop()
        self.assertEqual(kills, [signal.SIGTERM])
        self.assertTrue(out["exit_confirmed"])

    def test_stop_is_refused_with_a_foreign_proc_root(self):
        proc = self.tmp / "proc"
        write_proc(proc)
        inst = collect.NativeInstance(777, proc, clk_tck=100)  # real os.kill, fake proc tree
        with self.assertRaises(SystemExit):
            self.collector(inst, "--stop").run()
        self.assertFalse(self.root.exists())

    @unittest.skipUnless(hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"), "needs pidfd")
    def test_pidfd_closes_the_check_to_kill_window(self):
        child = subprocess.Popen([sys.executable, "-I", "-c", "import sys; sys.stdin.read()"], stdin=subprocess.PIPE)
        self.addCleanup(lambda: (child.poll() is None and child.kill(), child.wait(), child.stdin.close()))
        inst = collect.NativeInstance(child.pid, clock=self.clock.clock, sleep=self.clock.sleep, stop_timeout=5)
        inst.attach()
        self.addCleanup(inst.close)
        self.assertIsNotNone(inst.pidfd)
        child.kill()
        child.wait()  # reaped: the pid is free to be recycled
        real, calls = inst.check, []

        def check():  # the identity check passes, then the process dies before the signal
            calls.append(1)
            return collect.OK if len(calls) == 1 else real()
        inst.check = check
        out = inst.stop()
        self.assertIn("signal_failed", out["errors"], "pidfd refuses a dead process instead of hitting a recycled pid")
        self.assertFalse(out["signal_delivered"])
        self.assertTrue(out["exit_confirmed"])

    def test_native_stop_without_pidfd_is_refused(self):
        proc = self.tmp / "proc"
        write_proc(proc)
        inst = collect.NativeInstance(777, proc, clk_tck=100, opener=lambda pid: None)
        inst.real_signals = False  # skip the proc-root check; fake tree on purpose
        with self.assertRaises(SystemExit):
            self.collector(inst, "--stop").run()

    def test_stop_with_wrong_identity_does_nothing(self):
        kills = []
        inst = self.instance(kills)
        inst.attach()
        write_proc(self.tmp / "proc", pid=777, start=42)
        out = inst.stop()
        self.assertEqual(kills, [])
        self.assertIn("process_identity_mismatch", out["errors"])

    def test_attach_refuses_zombie_and_missing(self):
        proc = self.tmp / "proc"
        write_proc(proc, pid=777, state="Z")
        with self.assertRaises(SystemExit):
            collect.NativeInstance(777, proc).attach()
        with self.assertRaises(SystemExit):
            collect.NativeInstance(31337, proc).attach()


class DockerDetailTests(Base):
    def test_gluetun_network_parent_is_recorded_and_changes_counted(self):
        instance = self.docker(parent_image="qmcgaw/gluetun:v3")
        self.clock.at(200, lambda: self.api.parent["State"].update(StartedAt="2026-11-01T00:03:00Z"))
        c = self.collector(instance, "--require-gluetun")
        self.assertEqual(c.run(), 0)
        run = json.loads((self.root / "run.json").read_text())
        self.assertEqual(run["network"]["mode"], "container")
        self.assertTrue(run["network"]["gluetun_network_parent"])
        self.assertEqual(self.result()["network_parent_changes"], 1)
        self.assertIn("network_parent_identity", self.rows()[0])
        self.assertEqual(self.report()["issues"], {})

    def test_require_gluetun_refuses_other_parents(self):
        for kwargs in ({}, {"parent_image": "alpine"}):
            root = self.tmp / f"run-{len(kwargs)}"
            self.root = root
            with self.assertRaises(SystemExit):
                self.collector(self.docker(**kwargs), "--require-gluetun").run()
            self.assertFalse(root.exists())

    def test_host_network_mode_recorded(self):
        instance = self.docker()
        self.api.container["HostConfig"]["NetworkMode"] = "host"
        self.collector(instance).run()
        run = json.loads((self.root / "run.json").read_text())
        self.assertTrue(run["network"]["host_network"])
        self.assertEqual(run["network"]["mode"], "host")

    def test_resources_fall_back_to_docker_stats_when_cgroup_missing(self):
        inst = collect.DockerInstance(FakeDocker(), "lodestar", self.tmp / "nowhere")
        inst.attach()
        self.assertEqual(inst.resources(), {"working_set_bytes": 200, "private_bytes": 150, "cpu_seconds": 2.0, "threads": 9})

    def test_nothing_is_invented_when_no_source_exists(self):
        api = FakeDocker()
        api.get = lambda path, orig=api.get: (404, {}) if "stats" in path else orig(path)
        inst = collect.DockerInstance(api, "lodestar", self.tmp / "nowhere")
        inst.attach()
        self.assertEqual(inst.resources(), {})

    def test_ip_families_are_separate_and_only_when_exposed(self):
        extra = ('lodestar_dht_queries_out_total{family="v4"} 10\nlodestar_dht_queries_out_total{family="v6"} 3\n'
                 'lodestar_dht_packets_in_total{ip_family="ipv6"} 7\n')
        self.lodestar.metrics = lambda t: (200, metrics_text(t, extra=extra).encode())
        self.run_docker()
        rows = self.rows()
        self.assertEqual(rows[0]["ip_family_counters"], {"v4": {"dht_queries_out_total": 10.0},
                                                         "v6": {"dht_queries_out_total": 3.0, "dht_packets_in_total": 7.0}})
        self.assertTrue(all("ip_family_counters" in r for r in rows))

    def test_ip_families_absent_without_labels(self):
        self.run_docker()
        self.assertTrue(all("ip_family_counters" not in r for r in self.rows()))

    def test_real_unix_socket_client(self):
        sock_dir = tempfile.mkdtemp(prefix="dk")
        self.addCleanup(lambda: __import__("shutil").rmtree(sock_dir, ignore_errors=True))
        path = os.path.join(sock_dir, "d.sock")
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def address_string(self):
                return "unix"

            def _reply(self, status, doc):
                body = json.dumps(doc).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                seen.append(("GET", self.path))
                self._reply(200, {"Id": CONTAINER_ID})

            def do_POST(self):
                seen.append(("POST", self.path))
                self.send_response(204)
                self.send_header("Content-Length", "0")
                self.end_headers()

        class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
            daemon_threads = True
        server = Server(path, Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(lambda: (server.shutdown(), server.server_close(), thread.join()))
        api = collect.DockerAPI(path)
        self.assertEqual(api.get("/containers/x/json"), (200, {"Id": CONTAINER_ID}))
        self.assertEqual(api.post("/containers/x/stop?t=1"), (204, None))
        self.assertEqual(seen, [("GET", "/containers/x/json"), ("POST", "/containers/x/stop?t=1")])
        with self.assertRaises(OSError):
            collect.DockerAPI(os.path.join(sock_dir, "missing.sock")).get("/x")


class LaunchGuardTests(Base):
    def test_non_empty_run_directory_is_refused_untouched(self):
        self.root.mkdir()
        keep = self.root / "keep.txt"
        keep.write_text("previous evidence")
        with self.assertRaises(SystemExit):
            self.collector(self.docker()).run()
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["keep.txt"])
        self.assertEqual(keep.read_text(), "previous evidence")

    def test_previous_run_is_never_reused(self):
        self.run_docker()
        before = (self.root / "samples.jsonl").read_bytes()
        self.clock.t = 0
        with self.assertRaises(SystemExit):
            self.collector(self.docker()).run()
        self.assertEqual((self.root / "samples.jsonl").read_bytes(), before)

    def test_empty_existing_directory_is_accepted(self):
        self.root.mkdir()
        self.assertEqual(self.collector(self.docker()).run(), 0)

    def test_unsafe_urls_commits_and_short_windows_are_refused(self):
        for url in ("https://127.0.0.1:8484", "http://u:p@127.0.0.1:8484", "http://127.0.0.1:8484/x",
                    "http://127.0.0.1:8484/?q=1", "127.0.0.1:8484", "http://"):
            with self.subTest(url=url), self.assertRaises(SystemExit):
                collect.Collector(args_for(self.tmp / "u", "--url", url), self.docker()).prepare()
        for commit in ("", "XYZ", "g" * 40, "a" * 41):
            args = args_for(self.tmp / "c", "--url", self.lodestar.url)
            args.commit = commit
            with self.subTest(commit=commit), self.assertRaises(SystemExit):
                collect.Collector(args, self.docker()).prepare()
        args = collect.build_parser().parse_args(["--mode", "docker", "--root", str(self.tmp / "w"), "--commit", COMMIT,
                                                  "--baseline-hours", "1"])
        with self.assertRaises(SystemExit):
            collect.Collector(args, self.docker()).prepare()
        self.assertFalse((self.tmp / "w").exists())

    def test_gluetun_leg_rules(self):
        base = ["--mode", "docker", "--root", str(self.tmp / "l"), "--commit", COMMIT, "--url", self.lodestar.url,
                "--min-free-bytes", "3", "--warn-free-bytes", "2", "--stop-free-bytes", "1"]
        parse = collect.build_parser().parse_args
        with self.assertRaises(SystemExit):  # the leg needs --require-gluetun
            collect.Collector(parse(base + ["--leg", "gluetun", "--baseline-hours", "2"]), self.docker(parent_image="gluetun")).prepare()
        with self.assertRaises(SystemExit):  # and at least 2 h
            collect.Collector(parse(base + ["--leg", "gluetun", "--require-gluetun", "--baseline-hours", "1"]),
                              self.docker(parent_image="gluetun")).prepare()
        collect.Collector(parse(base + ["--leg", "gluetun", "--require-gluetun", "--baseline-hours", "2"]),
                          self.docker(parent_image="gluetun")).prepare()  # 2 h is allowed outside --rehearsal
        with self.assertRaises(SystemExit):  # the main leg still needs 24 h
            collect.Collector(parse(base + ["--baseline-hours", "2"]), self.docker()).prepare()

    def test_leg_is_recorded(self):
        c = self.collector(self.docker(parent_image="qmcgaw/gluetun"), "--require-gluetun", "--leg", "gluetun")
        self.assertEqual(c.run(), 0)
        self.assertEqual(json.loads((self.root / "run.json").read_text())["settings"]["leg"], "gluetun")

    def test_guardrails_must_be_ordered(self):
        args = args_for(self.tmp / "g", "--url", self.lodestar.url)
        args.stop_free_bytes, args.warn_free_bytes = 5, 4
        with self.assertRaises(SystemExit):
            collect.Collector(args, self.docker()).prepare()

    def test_missing_container_and_stopped_container_are_refused(self):
        api = FakeDocker()
        inst = collect.DockerInstance(api, "no-such")
        with self.assertRaises(SystemExit):
            inst.attach()
        api.container["State"]["Running"] = False
        with self.assertRaises(SystemExit):
            collect.DockerInstance(api, "lodestar").attach()
        api.fail = True
        with self.assertRaises(SystemExit):
            collect.DockerInstance(api, "lodestar").attach()

    def test_prepare_failure_after_launch_publishes_a_result(self):
        c = self.collector(self.docker())
        with mock.patch.object(collect, "save_json", side_effect=OSError("read-only")):
            self.assertEqual(c.run(), 1)
        self.assertEqual(self.result()["reason"], "evidence_write_error")

    def test_cli_requires_target(self):
        for argv in (["--mode", "docker"], ["--mode", "native"]):
            with self.subTest(argv=argv), self.assertRaises(SystemExit) as ctx, mock.patch("sys.stderr"):
                collect.main(argv + ["--root", str(self.tmp / "x"), "--commit", COMMIT])
            self.assertEqual(ctx.exception.code, 2)

    def test_cli_reports_refusals_with_status_two(self):
        self.root.mkdir()
        (self.root / "x").write_text("")
        proc = self.tmp / "proc"
        write_proc(proc)
        done = subprocess.run([sys.executable, "-I", str(HERE / "collect.py"), "--mode", "native", "--pid", "777",
                               "--proc-root", str(proc), "--root", str(self.root), "--commit", COMMIT, "--rehearsal"],
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 2)
        self.assertIn("not empty", done.stderr)


class PublicationTests(Base):
    def test_evidence_has_only_aggregates(self):
        instance = self.docker()
        c = self.collector(instance, "--stop")
        c.run()
        expected = {"run.json", "samples.jsonl", "status.json", "result.json", "disk.jsonl", "events.log",
                    "collector.py", "baseline-start.prom", "metrics"}
        self.assertEqual({p.name for p in self.root.iterdir()}, expected)
        self.assertEqual([p for p in self.root.rglob("*") if p.name.endswith(".tmp")], [])
        for path in self.root.rglob("*"):
            if not path.is_file() or path.name == "collector.py":
                continue
            text = path.read_text()
            for secret in SECRETS + ("LODESTAR_SETUP_TOKEN", "VPN_PASSWORD", "--token", "Env", "Cmd"):
                self.assertNotIn(secret, text, f"{secret!r} leaked into {path.name}")
        for prom in (self.root / "metrics").glob("*.prom"):
            names = {line.split("{")[0].split(" ")[0] for line in prom.read_text().splitlines()}
            self.assertTrue(all(n.removeprefix("lodestar_") in collect.KEEP for n in names), names)
            self.assertIn('lodestar_fetch_failures_total{kind="timed_out"}', prom.read_text())

    def test_lodestar_restart_inside_the_container_ends_the_run(self):
        def metrics(t):
            text = metrics_text(t)
            return 200, (text.replace("lodestar_start_time_seconds 1000", "lodestar_start_time_seconds 5000") if t >= 200 else text).encode()
        self.lodestar.metrics = metrics
        c, rc = self.run_docker("--stop")
        self.assertEqual(rc, 1)
        result = self.result()
        self.assertEqual(result["reason"], "process_exited_instance_restarted")
        self.assertEqual(result["observed_start_time_seconds"], 5000)
        self.assertEqual(self.api.posts(), [])
        self.assertEqual({r["values"]["start_time_seconds"] for r in self.rows()}, {1000})
        self.assertNotIn("identity_changes", self.report()["issues"])

    def test_no_personal_paths_or_hostnames_in_evidence(self):
        storage = self.tmp / "mnt" / "user" / "alice" / "appdata"
        storage.mkdir(parents=True)
        instance = self.docker()
        self.api.container["Config"]["Image"] = "registry.nas.home.lan/alice/lodestar:1"
        self.api.container["HostConfig"]["NetworkMode"] = "alices-private-net"
        url = "http://nas.home.lan:" + self.lodestar.url.rsplit(":", 1)[1]
        direct = lambda u: self.lodestar.respond("/" + u.rsplit("/", 1)[1])
        args = args_for(self.root, "--url", url, "--storage-path", str(storage), "--stop")
        c = collect.Collector(args, instance, clock=self.clock.clock, sleep=self.clock.sleep, now=self.clock.now,
                              fetch=direct, volumes=lambda labelled: collect.statvfs_volumes(labelled))
        self.assertEqual(c.run(), 0)
        for path in self.root.rglob("*"):
            if path.is_file() and path.name != "collector.py":
                text = path.read_text()
                for personal in ("alice", "nas.home.lan", str(self.tmp), "/mnt/user"):
                    self.assertNotIn(personal, text, f"{personal!r} leaked into {path.name}")
        run = json.loads((self.root / "run.json").read_text())
        self.assertEqual(run["network"]["http_host_class"], "name")
        self.assertEqual(run["network"]["mode"], "custom")
        labels = {v["volume"] for v in run["settings"]["storage_guardrails"]["volumes"]}
        self.assertTrue(labels <= {"evidence", "storage-1"}, labels)

    def test_atomic_writes_fsync_their_directory(self):
        with mock.patch.object(collect, "fsync_dir", wraps=collect.fsync_dir) as spy:
            self.run_docker()
        synced = {Path(c.args[0]) for c in spy.call_args_list}
        self.assertEqual(synced, {self.root, self.root / "metrics"})
        self.assertGreaterEqual(spy.call_count, 2)  # run.json and result.json at least

    def test_host_and_network_classes(self):
        for host, cls in (("127.0.0.1", "loopback"), ("[::1]", "loopback"), ("localhost", "loopback"),
                          ("192.168.1.5", "private"), ("8.8.8.8", "public"), ("nas.home.lan", "name")):
            self.assertEqual(collect.host_class(host), cls, host)
        for mode, cls in (("host", "host"), ("bridge", "bridge"), ("container:abc", "container"),
                          ("br0", "custom"), ("default", "bridge")):
            self.assertEqual(collect.network_mode_class(mode), cls, mode)

    def test_native_run_records_process_name_not_command_line(self):
        proc = self.tmp / "proc"
        write_proc(proc)
        (proc / "777" / "cmdline").write_bytes(b"lodestar\0serve\0--token\0s3cr3t-token\0")
        inst = collect.NativeInstance(777, proc, clock=self.clock.clock, sleep=self.clock.sleep, clk_tck=100)
        self.collector(inst).run()
        for path in self.root.rglob("*"):
            if path.is_file() and path.name != "collector.py":
                self.assertNotIn("s3cr3t-token", path.read_text())

    def test_snapshot_sanitizer(self):
        text = metrics_text(100, extra="lodestar_dht_timeouts_total 5\nlodestar_dht_timeouts_total 9\n")
        clean = collect.sanitize_snapshot(text)
        self.assertEqual(clean.count("lodestar_dht_timeouts_total"), 1)
        for secret in SECRETS + ("unknown_thing", "# HELP"):
            self.assertNotIn(secret, clean)
        self.assertEqual(analyze_cr0.parse_prom(clean)["dht_timeouts_total"], {"": 5.0})

    def test_counter_list_matches_the_analyzer(self):
        self.assertEqual(collect.COUNTERS, analyze_cr0.COUNTERS)

    def test_required_metrics_match_the_windows_collector(self):
        import re
        source = (HERE.parent / "cr0-soak.ps1").read_text()
        block = next(b for b in re.findall(r"foreach \(\$name in @\((.*?)\)\) \{", source, re.S) if "start_time_seconds" in b)
        self.assertEqual(set(re.findall(r"'([a-z_]+)'", block)), set(collect.REQUIRED))

    def test_analyzer_cli_reads_the_output_end_to_end(self):
        self.run_docker("--stop")
        done = subprocess.run([sys.executable, "-I", str(HERE.parent / "analyze_cr0.py"), str(self.root),
                               "--format", "json"], capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stderr)
        report = json.loads(done.stdout)
        self.assertEqual(report["provenance"], {"commit": COMMIT, "sha256": "a" * 64,
                                                 "collector_sha256": collect.sha256_file(self.root / "collector.py")})
        self.assertEqual(report["baseline_status"], "collector_window_complete_review_required")
        self.assertEqual(report["issues"], {})
        self.assertTrue(report["clean_shutdown"])
        self.assertEqual(report["stop_reason"], "baseline_window_complete")
        text = subprocess.run([sys.executable, "-I", str(HERE.parent / "analyze_cr0.py"), str(self.root)],
                              capture_output=True, text=True, timeout=60).stdout
        self.assertIn("# CR-0 soak evidence", text)
        for secret in SECRETS:
            self.assertNotIn(secret, text)


if __name__ == "__main__":
    unittest.main()
