"""X-cr-round1-collect: the round-1 coordinator (infra/fleet/steps/vm/round1-coordinator.py)
and the 10 s host sampler (host_samples.py), against fakes.

Docker, the namespaces, the clock and each arm's HTTP endpoint are replaced by FakeHost: the
real ones need root, Docker and a VPN, so the logic is tested here and the box's rehearsal
exercises the rest (docs/soak/runsheets/host-round1-runsheet.md).
"""

import importlib.util
import json
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "fixtures" / "round1"))

import collect  # noqa: E402
import host_samples as hs  # noqa: E402
import make_archive as mk  # noqa: E402

SPEC = importlib.util.spec_from_file_location("round1_coordinator", ROOT / "infra/fleet/steps/vm/round1-coordinator.py")
co = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(co)

SALT = b"coordinator test salt, never real"
UPLINK = "198.51.100.7"
EXIT_IP = {"X": "203.0.113.21", "Y": "203.0.113.22", "Z": "203.0.113.23"}
T0 = datetime(2026, 11, 29, 8, 0, tzinfo=timezone.utc)
IMAGE = "lodestar@" + mk.DIGEST


def iso(t):
    return t.isoformat().replace("+00:00", "Z")


class FakeHost:
    """Docker, namespaces, HTTP and the clock, as the coordinator calls them."""

    def __init__(self, t=T0):
        self.t, self.calls, self.spawned, self.signals = t, [], [], []
        self.pids = {"gluetun-b": 101, "gluetun-a": 102, "gluetun-c": 103}
        self.ip_of_pid = {}  # pid -> address
        self.free, self.used = 10 << 40, 0
        self.health = {}  # arm port -> fn(t) -> status
        self.counter = lambda port, t: (t - T0).total_seconds()
        self.dead = set()  # PIDs that have exited

    def alive(self, pid, needle):
        return pid is not None and pid not in self.dead

    def now(self):
        return self.t

    def sleep(self, seconds):
        self.t += timedelta(seconds=seconds)

    def run(self, argv, env=None, timeout=120):
        self.calls.append((list(argv), dict(env or {})))
        if argv[:2] == ["docker", "inspect"]:
            pid = self.pids.get(argv[-1])
            return (0, f"{pid}\n") if pid else (1, "")
        return 0, ""

    def spawn(self, argv, log, env=None):
        self.spawned.append(argv)
        return 9000 + len(self.spawned)

    def signal(self, pid, sig=None):
        self.signals.append(pid)
        return True

    def get(self, url, timeout=5):
        port = int(url.split(":")[2].split("/")[0])
        status = self.health.get(port, lambda t: 200)(self.t)
        if status != 200:
            return status, b""
        if url.endswith("/healthz"):
            return 200, b"ok"
        return 200, f"lodestar_dht_queries_out_total {self.counter(port, self.t):.0f}\n".encode()

    def free_bytes(self, path):
        return self.free

    def tree_bytes(self, path):
        return self.used

    def uplink_ip(self, url):
        return UPLINK

    def egress_ip(self, pid, url):
        return self.ip_of_pid[pid]

    def commands(self):
        return [" ".join(c[0]) for c in self.calls if c[0][:2] != ["docker", "inspect"]]


class CoordinatorBase(unittest.TestCase):
    layout = "L3"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.tmp = Path(self.temp.name)
        self.round = self.tmp / "round1"
        self.round.mkdir()
        a = mk.archive(self.layout)
        self.decl = a["declaration"]
        (self.round / "declaration.json").write_text(json.dumps(self.decl))
        comp = a["comparator"]
        comp["timeline"]["window_end_utc"] = iso(T0 - timedelta(hours=13))
        (self.round / "comparator.json").write_text(json.dumps(comp))
        salt = self.tmp / "salt"
        salt.write_bytes(SALT)
        salt.chmod(0o600)
        arms = {}
        for arm, port in (("B", 8091), ("A", 8092), ("C", 8093)):
            stack = self.tmp / f"stack-{arm.lower()}"
            (stack / "secrets").mkdir(parents=True)
            (stack / "secrets" / "wg0.conf").write_text(f"[Peer]\nEndpoint = 192.0.2.{port - 8000}:1337\n")
            arms[arm] = {"stack": str(stack), "suffix": "-" + arm.lower(), "port": port, "tunnel_target": "10.0.0.1",
                         "password_file": str(self.tmp / "pw")}
        self.cfg_path = self.round / "coordinator.json"
        self.cfg_path.write_text(json.dumps({
            "schema": 1, "kind": "cr_round1_coordinator", "declaration": "declaration.json",
            "comparator_report": "comparator.json", "salt_file": str(salt), "data_volume": str(self.tmp),
            "round_dir": str(self.round), "image": IMAGE, "commit": "e" * 40, "uplink_interface": "eth0",
            "expected_growth_bytes": {"default": 20 << 30}, "arms": arms}))
        self.host = FakeHost()
        self.place(1)

    def place(self, day):
        """Each arm's gluetun egress on its declared exit for this day."""
        for arm, label in mk.LAYOUTS[self.layout][day].items():
            self.host.ip_of_pid[self.host.pids["gluetun-" + arm.lower()]] = EXIT_IP[label]

    def day_dir(self, day=1, repeat=0):
        return self.round / "r1-a1" / (f"day{day}" + (f"-repeat{repeat}" if repeat else ""))

    def go(self, command, *extra, day=1, repeat=0):
        argv = [command, str(self.cfg_path)]
        if command not in ("end-attempt", "manifest"):
            argv += ["--day-dir", str(self.day_dir(day, repeat)), "--attempt", "r1-a1", "--day", str(day),
                     "--repeat", str(repeat)]
        argv += list(extra)
        import io
        from contextlib import redirect_stdout
        out = io.StringIO()
        with redirect_stdout(out):
            rc = co.main(argv, host=self.host)
        return rc, out.getvalue().splitlines()

    def leak_log(self, ok=True):
        p = self.tmp / ("leak-ok.log" if ok else "leak-fail.log")
        p.write_text("ok tunnel_healthy\ncount captured_packets_down 0\n" + ("ok leak_test\n" if ok else "fail no_egress_captured_down\nfail leak_test\n"))
        return p

    def pass_leaks(self, day=1, phase="before"):
        for arm in mk.LAYOUTS[self.layout][day]:
            rc, out = self.go("leak", "--arm", arm, "--phase", phase, "--log", str(self.leak_log()), day=day)
            self.assertEqual(rc, 0, out)

    def state(self, day=1):
        return json.loads((self.day_dir(day) / "state.json").read_text())


class StartGateTests(CoordinatorBase):
    def test_no_arm_starts_before_the_comparators_window_end_plus_12h(self):
        self.pass_leaks()
        self.host.t = T0 - timedelta(hours=1, minutes=1)
        rc, out = self.go("check-start")
        self.assertEqual(rc, 1)
        self.assertIn("too early", out[-1])
        rc, out = self.go("up")
        self.assertEqual(rc, 1)
        self.assertFalse([c for c in self.host.commands() if "up" in c.split()])
        self.host.t = T0
        rc, out = self.go("check-start")
        self.assertEqual(rc, 0, out)

    def test_every_arm_needs_its_own_vpn_server(self):
        self.assertEqual(self.go("check-endpoints")[0], 0)
        stack = Path(json.loads(self.cfg_path.read_text())["arms"]["C"]["stack"])
        (stack / "secrets" / "wg0.conf").write_text("[Peer]\nEndpoint = 192.0.2.91:1337\n")
        rc, out = self.go("check-endpoints")
        self.assertEqual(rc, 1)
        self.assertIn("same VPN server", out[-1])
        self.assertNotIn("192.0.2", "\n".join(out))  # compared, never printed

    def test_the_disk_budget_is_one_host_check_with_a_20pct_margin(self):
        self.pass_leaks()
        self.host.used = 100 << 30
        self.host.free = int(1.2 * (3 * (20 << 30) + (100 << 30))) - 1
        rc, out = self.go("check-start")
        self.assertIn("disk budget", out[-1])
        self.host.free += 1
        self.assertEqual(self.go("check-start")[0], 0)

    def test_every_arm_needs_a_passing_leak_test_and_fresh_directories(self):
        self.go("leak", "--arm", "B", "--phase", "before", "--log", str(self.leak_log()))
        rc, out = self.go("check-start")
        self.assertIn("no passing leak test", out[-1])
        self.pass_leaks()
        data = Path(json.loads(self.cfg_path.read_text())["arms"]["A"]["stack"]) / "data"
        data.mkdir()
        (data / "lodestar.db").write_text("x")
        rc, out = self.go("check-start")
        self.assertIn("isn't empty", out[-1])

    def test_up_starts_each_arm_with_its_declared_settings(self):
        self.pass_leaks()
        rc, out = self.go("up")
        self.assertEqual(rc, 0, out)
        ups = [(argv, env) for argv, env in self.host.calls if "up" in argv]
        self.assertEqual(len(ups), 3)
        env = {argv[argv.index("-p") + 1]: e for argv, e in ups}
        self.assertEqual(env["lodestar-a"]["LODESTAR_INTENSITY"], "aggressive")
        self.assertNotIn("LODESTAR_INTENSITY", env["lodestar-b"])
        self.assertEqual(env["lodestar-b"]["LODESTAR_DHT_ROUTE_BACKOFF"], "false")
        self.assertEqual(env["lodestar-b"]["LODESTAR_DHT_UTP"], "true")
        self.assertEqual(env["lodestar-c"]["LODESTAR_DHT_UTP"], "false")
        self.assertTrue(all(e["LODESTAR_DHT_SAFE_RESTART"] == "true" for e in env.values()))
        self.assertEqual((env["lodestar-c"]["LODESTAR_ARM_SUFFIX"], env["lodestar-c"]["LODESTAR_UI_PORT"]), ("-c", "8093"))
        self.assertEqual({e["LODESTAR_IMAGE"] for e in env.values()}, {IMAGE})


class RehearsalAndIdentityTests(CoordinatorBase):
    def test_the_rehearsal_never_runs_beside_the_comparator(self):
        comp = json.loads((self.round / "comparator.json").read_text())
        comp["timeline"]["window_end_utc"] = iso(T0 + timedelta(hours=1))
        (self.round / "comparator.json").write_text(json.dumps(comp))
        day = self.round / "rehearsal"
        argv = ["--day-dir", str(day), "--attempt", "r1-a1", "--day", "1", "--rehearsal"]
        for arm in mk.LAYOUTS["L3"][1]:
            co.main(["leak", str(self.cfg_path), *argv, "--arm", arm, "--phase", "before", "--log", str(self.leak_log())],
                    host=self.host)
        self.assertEqual(co.main(["check-start", str(self.cfg_path), "--day-dir", str(day)], host=self.host), 1)

    def test_the_rehearsal_runs_inside_the_12h_and_never_reaches_the_manifest(self):
        self.host.t = T0 - timedelta(hours=5)  # inside the comparator's 12 h
        day = self.round / "rehearsal"
        argv = ["--day-dir", str(day), "--attempt", "r1-a1", "--day", "1", "--rehearsal"]
        for arm in mk.LAYOUTS["L3"][1]:
            self.assertEqual(co.main(["leak", str(self.cfg_path), *argv, "--arm", arm, "--phase", "before",
                                      "--log", str(self.leak_log())], host=self.host), 0)
        self.assertEqual(co.main(["check-start", str(self.cfg_path), "--day-dir", str(day)], host=self.host), 0)
        co.main(["bring-up", str(self.cfg_path), *argv], host=self.host)
        co.main(["collect", str(self.cfg_path), "--day-dir", str(day)], host=self.host)
        collector = next(a for a in self.host.spawned if "B" in a and "collect.py" in a[2])
        self.assertIn("--rehearsal", collector)
        self.assertEqual(collector[collector.index("--run-id") + 1], "r1-r1-a1-d1-B-rehearsal")
        self.assertEqual(co.list_days(co.load_config(self.cfg_path)), [])

    def test_identity_prints_hashes_and_names_only(self):
        (self.round / "declaration.json").unlink()  # the runner writes it from these
        with mock.patch.object(co.collect, "host_key", lambda: "0" * 32):
            rc, out = self.go("identity")
        self.assertEqual(rc, 0, out)
        self.assertEqual(out[0], "host_identity " + collect.salted_hash(SALT, "host", "0" * 32))
        self.assertEqual(out[1], "uplink_hash " + collect.salted_hash(SALT, "ip", UPLINK))
        self.assertNotIn(UPLINK, "\n".join(out))
        rc, out = self.go("check-start")
        self.assertIn("isn't there yet", out[-1])


class LeakTests(CoordinatorBase):
    def test_a_failed_leak_test_before_the_start_stops_that_arm_only_until_a_passing_retest(self):
        """§5.2's fixture: a failed leak test stops crawling on the affected arm."""
        self.pass_leaks()
        rc, out = self.go("leak", "--arm", "A", "--phase", "before", "--log", str(self.leak_log(ok=False)))
        self.assertIn("ok crawling_stopped arm=A", out)
        stops = [c for c in self.host.commands() if c.startswith("docker stop")]
        self.assertEqual(stops, ["docker stop -t 60 lodestar-a"])
        s = self.state()
        self.assertTrue(s["arms"]["A"]["blocked"])
        self.assertFalse(s["arms"]["B"]["blocked"] or s["arms"]["C"]["blocked"])
        self.assertEqual(s["events"], [])  # nothing measured yet: noted, and the day can still run
        self.assertIn("before the start", s["notes"][-1])
        self.assertIn("stopped after a failed leak test", self.go("check-start")[1][-1])
        self.go("bring-up")
        self.assertIn("stopped after a failed leak test", self.go("collect")[1][-1])
        self.assertFalse(self.host.spawned)
        self.go("leak", "--arm", "A", "--phase", "retest", "--log", str(self.leak_log()))
        self.assertFalse(self.state()["arms"]["A"]["blocked"])
        self.assertEqual(self.go("check-start")[0], 0)

    def test_a_failure_after_the_day_is_an_infrastructure_event(self):
        self.pass_leaks()
        self.go("bring-up")
        self.go("collect")
        for arm in ("A", "B", "C"):  # the collectors have closed
            (self.day_dir() / arm).mkdir(exist_ok=True)
            (self.day_dir() / arm / "result.json").write_text("{}")
        self.go("leak", "--arm", "A", "--phase", "after", "--log", str(self.leak_log(ok=False)))
        ev = self.state()["events"][-1]
        self.assertEqual((ev["class"], ev["cause"], ev["arms"]), ("infrastructure", "leak_test_failed", ["A"]))

    def test_no_leak_test_while_a_window_may_be_open(self):
        self.pass_leaks()
        self.go("bring-up")
        self.go("collect")
        rc, out = self.go("leak", "--arm", "B", "--phase", "before", "--log", str(self.leak_log(ok=False)))
        self.assertEqual(rc, 1)
        self.assertIn("still measuring", out[-1])
        self.assertFalse([c for c in self.host.commands() if c.startswith("docker stop")])

    def test_a_refusal_before_testing_is_not_a_verdict(self):
        log = self.tmp / "refused.log"
        log.write_text("fail block_running\nfail leak_test\n")
        rc, out = self.go("leak", "--arm", "B", "--phase", "before", "--log", str(log))
        self.assertIn("skip leak_test", out[-1])
        self.assertEqual(self.state()["arms"]["B"]["leak_tests"], [])
        self.assertFalse(self.host.commands())
        self.assertEqual(co.leak_verdict("fail tool_missing_tcpdump\nfail leak_test\n"), "not_run")
        self.assertEqual(co.leak_verdict("fail block_running\nfail no_egress_captured_down\nfail leak_test\n"), "fail")

    def test_a_host_level_failure_stops_every_arm(self):
        self.pass_leaks()
        rc, out = self.go("leak", "--arm", "C", "--phase", "before", "--log", str(self.leak_log(ok=False)), "--host-level")
        stops = sorted(c for c in self.host.commands() if c.startswith("docker stop"))
        self.assertEqual(stops, ["docker stop -t 60 lodestar-a", "docker stop -t 60 lodestar-b",
                                 "docker stop -t 60 lodestar-c"])
        self.assertTrue(all(a["blocked"] for a in self.state()["arms"].values()))
        self.assertEqual(co.leak_verdict("ok a\nok leak_test\n"), "pass")
        self.assertEqual(co.leak_verdict("ok leak_test\nfail guards_released\n"), "fail")
        self.assertEqual(co.leak_verdict(""), "fail")


class BringUpTests(CoordinatorBase):
    def test_exits_are_salted_hashes_by_label(self):
        self.pass_leaks()
        rc, out = self.go("bring-up")
        self.assertEqual(rc, 0, out)
        s = self.state()
        self.assertEqual(s["exits"], {label: collect.salted_hash(SALT, "ip", ip) for label, ip in EXIT_IP.items()})
        self.assertEqual(s["arms"]["A"]["egress"][0]["hash"], s["exits"]["Y"])
        text = (self.day_dir() / "state.json").read_text()
        for ip in list(EXIT_IP.values()) + [UPLINK]:
            self.assertNotIn(ip, text)

    def test_a_repair_before_the_start_rechecks_the_exits_and_keeps_both(self):
        self.pass_leaks()
        self.go("bring-up")
        first = self.state()["exits"]["Y"]
        self.host.ip_of_pid[102] = "203.0.113.99"  # A's server replaced during a repair
        rc, out = self.go("bring-up")
        self.assertEqual(rc, 0, out)
        s = self.state()
        self.assertEqual([c["exits"]["Y"] for c in s["exit_checks"]], [first, s["exits"]["Y"]])
        self.assertIn("Y changed", s["notes"][-1])
        self.go("collect")
        self.host.ip_of_pid[102] = EXIT_IP["Y"]
        self.assertIn("after its start", self.go("bring-up")[1][-1])

    def test_two_arms_on_one_exit_or_an_untunnelled_arm_are_refused(self):
        self.pass_leaks()
        self.host.ip_of_pid[103] = EXIT_IP["X"]
        rc, out = self.go("bring-up")
        self.assertIn("share an egress", out[-1])
        self.host.ip_of_pid[103] = UPLINK
        rc, out = self.go("bring-up")
        self.assertIn("uplink", out[-1])
        self.assertEqual(self.state()["exits"], {})


def write_collector(run_dir, arm, start, run_id, egress_hash, hours=24, reason="baseline_window_complete",
                    end_early=None):
    """A collector's run directory, abridged to what the coordinator reads."""
    run_dir.mkdir(parents=True)
    (run_dir / "run.json").write_text(json.dumps({"run_id": run_id, "settings": {"baseline_hours": hours}}))
    ws = start + timedelta(minutes=61)
    rows = [{"time": iso(start + timedelta(seconds=5)), "egress": {"read": "ok", "hash": egress_hash}}]
    for k in range(hours + 1):
        t = ws + timedelta(hours=k, seconds=20)
        if end_early is not None and t > end_early:
            break
        rows.append({"time": iso(t), "boundary": [k], "egress": {"read": "ok", "hash": egress_hash}})
    (run_dir / "samples.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
    (run_dir / "result.json").write_text(json.dumps({"reason": reason}))
    return ws + timedelta(hours=hours, seconds=20)


class WatchTests(CoordinatorBase):
    def setUp(self):
        super().setUp()
        self.pass_leaks()
        self.go("bring-up")
        self.go("collect")
        s = self.state()
        self.finals = {}
        for arm, label in mk.LAYOUTS[self.layout][1].items():
            start = T0 + timedelta(minutes=mk.STAGGER[arm])
            self.finals[arm] = write_collector(self.day_dir() / arm, arm, start, s["arms"][arm]["run_id"], s["exits"][label])

    def test_staggered_finishes_keep_every_arm_crawling_until_the_latest_final_boundary_plus_2_min(self):
        rc, out = self.go("watch")
        self.assertEqual(rc, 0, out)
        latest = max(self.finals.values())
        self.assertGreaterEqual(self.host.t, latest + timedelta(minutes=2))
        self.assertLess(self.host.t, latest + timedelta(minutes=4))
        stops = [c for c in self.host.commands() if c.endswith(" stop")]
        self.assertEqual(len(stops), 3)
        self.assertTrue(all(" -p lodestar-" in c for c in stops))
        self.assertEqual(self.host.signals, [self.state()["sampler_pid"]])
        archive = json.loads((self.day_dir() / "archive.json").read_text())
        for arm, info in archive["arms"].items():
            self.assertLessEqual(info["active_from_utc"], iso(T0 + timedelta(minutes=10)))
            self.assertGreaterEqual(info["active_through_utc"], iso(latest))
            self.assertEqual(info["leak_test"], {"before": "pass", "after": None})
            self.assertEqual(info["egress"][-1]["hash"], archive["exits"][info["exit"]])  # read at shutdown
            self.assertEqual(info["collector"]["reason"], "baseline_window_complete")
        self.assertEqual(archive["events"], [])

    def test_a_peer_that_exits_during_the_overhang_shows_where_its_activity_ended(self):
        """§5.2's fixture: B's process fails after its own final boundary, before A's and C's."""
        port_b = json.loads(self.cfg_path.read_text())["arms"]["B"]["port"]
        died = self.finals["B"] + timedelta(seconds=30)
        self.host.health[port_b] = lambda t: 200 if t < died else 0
        self.go("watch")
        archive = json.loads((self.day_dir() / "archive.json").read_text())
        through = datetime.fromisoformat(archive["arms"]["B"]["active_through_utc"].replace("Z", "+00:00"))
        self.assertLess(through, self.finals["A"])
        self.assertGreaterEqual(through, self.finals["B"] - timedelta(minutes=1))

    def test_an_early_close_is_an_event(self):
        run = self.day_dir() / "C"
        (run / "result.json").write_text(json.dumps({"reason": "three_consecutive_health_failures"}))
        self.go("archive")
        archive = json.loads((self.day_dir() / "archive.json").read_text())
        self.assertEqual(archive["events"], [{"class": "product", "arms": ["C"], "cause": "three_consecutive_health_failures",
                                              "diagnosis": "pending"}])
        (run / "result.json").write_text(json.dumps({"reason": "collector_error"}))
        self.go("archive")
        archive = json.loads((self.day_dir() / "archive.json").read_text())
        self.assertEqual(archive["events"][0]["class"], "unclassified")  # gate-round: unknown_event, fails closed

    def test_a_collector_that_died_without_a_result_closes_the_day_failed_closed(self):
        (self.day_dir() / "C" / "result.json").unlink()
        self.host.dead.add(self.state()["arms"]["C"]["collector_pid"])
        rc, out = self.go("watch")
        self.assertEqual(rc, 0, out)
        archive = json.loads((self.day_dir() / "archive.json").read_text())
        self.assertEqual(archive["arms"]["C"]["collector"]["reason"], "collector_died")
        self.assertEqual(archive["events"], [{"class": "unclassified", "arms": ["C"], "cause": "collector_died"}])

    def test_watch_runs_once_and_stop_ends_a_day_early(self):
        self.go("watch")
        rc, out = self.go("watch")
        self.assertIn("already shut down", out[-1])

    def test_stop_ends_the_day_now(self):
        (self.day_dir() / "C" / "result.json").unlink()
        rc, out = self.go("stop")
        self.assertEqual(rc, 0, out)
        self.assertEqual(len([c for c in self.host.commands() if c.endswith(" stop")]), 3)
        self.assertIn("stopped by `stop`", self.state()["notes"][-1])
        self.assertTrue((self.day_dir() / "archive.json").exists())

    def test_watch_waits_while_any_collector_is_still_measuring(self):
        (self.day_dir() / "C" / "result.json").unlink()
        args = co.build_parser().parse_args(["watch", str(self.cfg_path), "--day-dir", str(self.day_dir())])
        cfg = co.load_config(self.cfg_path)
        out = co.cmd_watch(cfg, co.Day(cfg, self.day_dir()), self.host, args, max_loops=5)
        self.assertEqual(out, ["ok watch paused (test bound)"])
        self.assertFalse([c for c in self.host.commands() if c.endswith(" stop")])


class LaunchPathTests(unittest.TestCase):
    def test_the_commands_it_launches_exist_in_this_checkout(self):
        self.assertTrue((co.TOOLS / "collect.py").is_file())
        self.assertTrue((co.TOOLS / "host_samples.py").is_file())


class ActivityTests(unittest.TestCase):
    def rows(self, spec):
        return [{"utc": iso(T0 + timedelta(seconds=s)), "health": h, "queries_out": q} for s, h, q in spec]

    def test_the_span_needs_health_and_rising_counters(self):
        rows = self.rows([(0, 200, 1), (60, 200, 5), (120, 200, 9), (180, 200, 9), (240, 0, None), (300, 200, 12)])
        start, end = co.active_span(rows)  # one flat and one failed scrape in between don't end it
        self.assertEqual((start, end), (T0 + timedelta(seconds=60), T0 + timedelta(seconds=300)))

    def test_over_180s_without_working_ends_it_for_good(self):
        rows = self.rows([(0, 200, 1), (60, 200, 2), (120, 200, 2), (180, 200, 2), (300, 200, 3), (360, 200, 4)])
        self.assertEqual(co.active_span(rows)[1], T0 + timedelta(seconds=60))

    def test_the_collectors_own_scrapes_bridge_a_coordinator_gap(self):
        """A watch restarted after a few minutes: the collector's rows carry the span."""
        rows = self.rows([(0, 200, 1), (60, 200, 2), (600, 200, 3), (660, 200, 4)])
        col = [{"utc": iso(T0 + timedelta(seconds=s)), "health": 200, "lookups_completed": s} for s in range(0, 700, 60)]
        self.assertEqual(co.active_span(rows, collector_rows=col)[1], T0 + timedelta(seconds=660))
        self.assertEqual(co.active_span(rows)[1], T0 + timedelta(seconds=60))

    def test_an_unhealthy_first_scrape_starts_it_later(self):
        start, end = co.active_span(self.rows([(0, 0, None), (60, 200, 2), (120, 200, 3)]))
        self.assertEqual((start, end), (T0 + timedelta(seconds=120),) * 2)


class ManifestTests(CoordinatorBase):
    def make_day(self, day, repeat=0, run_ids=None):
        d = self.day_dir(day, repeat)
        d.mkdir(parents=True, exist_ok=True)
        arms = mk.LAYOUTS[self.layout][day]
        (d / "state.json").write_text(json.dumps({"attempt": "r1-a1", "day": day, "repeat": repeat,
                                                  "arms": {a: {"run_id": "x"} for a in arms}}))
        for arm in arms:
            rid = (run_ids or {}).get(arm, f"r1-a1-d{day}-{arm}" + (f"-rep{repeat}" if repeat else ""))
            (d / f"{arm}.report.json").write_text(json.dumps({"identity": {"image_digest": mk.DIGEST},
                                                             "binding": {"run_id": rid}}))
        (d / "archive.json").write_text("{}")
        (d / "host-samples.json").write_text("{}")

    def test_the_manifest_lists_every_input_by_hash(self):
        self.make_day(1)
        self.make_day(2)
        rc, out = self.go("manifest")
        self.assertEqual(rc, 0, out)
        man = json.loads((self.round / "manifest.json").read_text())
        self.assertEqual([(d["day"], d["repeat"]) for d in man["days"]], [(1, 0), (2, 0)])
        ref = man["days"][0]["reports"]["B"]
        self.assertEqual(ref["path"], "r1-a1/day1/B.report.json")
        self.assertEqual(ref["sha256"], co.sha256(self.round / ref["path"]))
        self.assertEqual(man["declaration"]["path"], "declaration.json")

    def test_duplicate_run_ids_repeat_gaps_and_another_image_are_refused(self):
        self.make_day(1, run_ids={"A": "r1-a1-d1-B"})
        self.assertIn("unique", self.go("manifest")[1][-1])
        self.make_day(1)
        self.make_day(1, repeat=2)
        self.assertIn("no gap", self.go("manifest")[1][-1])
        (self.day_dir(1, 2) / "state.json").unlink()
        rep = self.day_dir(1) / "B.report.json"
        rep.write_text(json.dumps({"identity": {"image_digest": "sha256:" + "0" * 64}, "binding": {"run_id": "x"}}))
        self.assertIn("image digest", self.go("manifest")[1][-1])

    def test_attempt_ends_records_the_days_run(self):
        self.make_day(1)
        rc, out = self.go("end-attempt", "--attempt", "r1-a1", "--reason", "drop_c")
        self.assertEqual(rc, 0, out)
        self.assertEqual(json.loads((self.round / "attempt-ends.json").read_text()),
                         {"r1-a1": {"reason": "drop_c", "days_run": [1]}})
        self.go("manifest")
        self.assertEqual(json.loads((self.round / "manifest.json").read_text())["attempt_ends"]["r1-a1"]["days_run"], [1])


class HostSamplerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.proc = Path(self.temp.name) / "proc"
        self.sys = Path(self.temp.name) / "sys"
        (self.sys / "class/net/eth0/statistics").mkdir(parents=True)
        self.proc.mkdir()
        self.pings = {}

    def write(self, user, steal, idle, writes, wms, inflight, drops):
        (self.proc / "stat").write_text(f"cpu  {user} 0 0 {idle} 0 0 0 {steal} 0 0\ncpu0 1 0 0 1 0 0 0 0 0 0\n")
        (self.proc / "diskstats").write_text(f" 259 0 nvme0n1 10 0 80 5 {writes} 0 800 {wms} {inflight} 9 9 0 0 0 0\n")
        (self.sys / "class/net/eth0/statistics/rx_dropped").write_text(f"{drops}\n")
        (self.sys / "class/net/eth0/statistics/tx_dropped").write_text("0\n")

    def run_cmd(self, argv, timeout):
        if argv[0] == "docker":
            return 0, {"gluetun-b": "101\n", "gluetun-c": "103\n"}.get(argv[-1], "")
        import re
        pid = re.search(r"/(\d+)/ns/net$", argv[1])[1]
        r = self.pings.get(pid, (0, "rtt min/avg/max/mdev = 30.5/30.5/30.5/0.0 ms\n"))
        return r

    def test_slots_carry_deltas_and_unreadable_values_stay_missing(self):
        s = hs.Sampler("nvme0n1", "eth0", {"B": ("gluetun-b", "10.0.0.1"), "C": ("gluetun-c", "10.0.0.1")},
                       proc_root=self.proc, sys_root=self.sys, run=self.run_cmd)
        self.write(100, 0, 900, 10, 50, 0, 0)
        first = s.sample(T0)
        self.assertIsNone(first["cpu"])  # no delta yet: missing, never good
        self.assertIsNone(first["disk_inflight"])
        self.write(140, 5, 955, 14, 70, 2, 3)
        row = s.sample(T0 + timedelta(seconds=10))
        self.assertEqual((row["cpu"], row["steal"]), (0.4, 0.05))
        self.assertEqual((row["disk_write_ms"], row["disk_inflight"]), (5.0, 2))
        self.assertEqual((row["uplink_drops_total"], row["uplink_rx_dropped"], row["uplink_tx_dropped"]), (3, 3, 0))
        self.assertEqual(row["tunnels"]["B"], {"rtt_ms": 30.5, "lost": False})
        self.pings["103"] = (1, "")  # no reply
        self.write(150, 5, 1045, 14, 70, 0, 3)
        row = s.sample(T0 + timedelta(seconds=20))
        self.assertEqual(row["tunnels"]["C"], {"rtt_ms": None, "lost": True})
        self.assertIsNone(row["disk_write_ms"])  # no write completed
        self.pings["103"] = (2, "")  # ping's own error: unknown, so the slot's tunnel reading is missing
        (self.sys / "class/net/eth0/statistics/rx_dropped").unlink()
        row = s.sample(T0 + timedelta(seconds=30))
        self.assertNotIn("C", row["tunnels"])
        self.assertIsNone(row["uplink_drops_total"])

    def test_drop_deltas_are_per_hour_and_per_series(self):
        rows = [{"utc": iso(T0 + timedelta(minutes=m)), "uplink_rx_dropped": rx, "uplink_tx_dropped": 0,
                 "uplink_rx_missed_errors": 0} for m, rx in ((0, 5), (30, 5), (59, 9), (60, 9), (90, 9))]
        self.assertEqual(hs.drop_deltas(rows), [
            {"hour_utc": iso(T0), "uplink_rx_dropped": 4, "uplink_rx_missed_errors": 0, "uplink_tx_dropped": 0},
            {"hour_utc": iso(T0 + timedelta(hours=1)), "uplink_rx_dropped": 0, "uplink_rx_missed_errors": 0,
             "uplink_tx_dropped": 0}])

    def test_assemble_dedups_computes_medians_and_hourly_coverage(self):
        rows = [{"utc": iso(T0 + timedelta(seconds=10 * k)), "cpu": 0.4, "steal": 0.0, "disk_write_ms": 3.0,
                 "disk_inflight": 0, "uplink_drops_total": 0, "tunnels": {"B": {"rtt_ms": 30.0, "lost": False}}}
                 for k in range(360) if k % 50]  # a stall every 50th slot: those stay missing
        rows.append(dict(rows[0]))
        rehearsal = [{"utc": iso(T0), "tunnels": {"B": {"rtt_ms": v, "lost": False}}} for v in (20, 30, 90)]
        doc = hs.assemble(rows, "nvme0n1", rehearsal, vcpus=8)
        self.assertEqual(doc["rehearsal_rtt_median_ms"], {"B": 30})
        self.assertEqual(len(doc["samples"]), 360 - 8)
        cov = doc["coverage_by_hour"][0]["coverage"]
        self.assertEqual(cov["cpu"], round(352 / 360, 4))
        self.assertLess(cov["tunnel_B"], 0.98)

    def test_a_late_slot_is_skipped_not_backfilled(self):
        import threading
        out = Path(self.temp.name) / "samples.jsonl"
        times = iter([T0 + timedelta(seconds=s) for s in (0.2, 0.3, 12.5, 12.6, 20.1, 20.2, 30.0, 31.0)])

        class S:
            def sample(self, slot):
                return {"utc": iso(slot)}
        stop = threading.Event()
        hs.run_loop(S(), out, stop, now=lambda: next(times), sleep=lambda s: None, until=T0 + timedelta(seconds=30))
        self.assertEqual([r["utc"] for r in hs.load_lines(out)], [iso(T0), iso(T0 + timedelta(seconds=20))])


if __name__ == "__main__":
    unittest.main()
