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
IMAGE = mk.DIGEST  # name@sha256:<hex>, as the declaration names it


def iso(t):
    return t.isoformat().replace("+00:00", "Z")


NETNS = {
    "addr_dev": "4: wg0    inet 10.1.2.3/32 scope global wg0\\       valid_lft forever preferred_lft forever\n",
    "addr_all": "1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever\n"
                "3: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0\\       valid_lft forever\n"
                "4: wg0    inet 10.1.2.3/32 scope global wg0\\       valid_lft forever preferred_lft forever\n",
    "route_get": "TARGET dev wg0 table 51820 src 10.1.2.3 uid 0 \n    cache \n",
    "routes": "default dev wg0 table 51820 scope link \n",
    "ping": (0, "PING 10.0.0.1 (10.0.0.1) 56(84) bytes of data.\n64 bytes from 10.0.0.1: icmp_seq=1 ttl=64 time=31.0 ms\n"),
}
WG_PRIVATE = "kPZ3hA5lq0dYkR0Jv3m1yYbqC8b0xq1v5m6iHq0mH2E="
GLUETUN_LOG = ("2026-11-29T08:00:01.123456789Z 2026-11-29T08:00:01Z INFO [wireguard] Connecting to 192.0.2.91:1337\n"
               "2026-11-29T08:00:02.000000000Z 2026-11-29T08:00:02Z INFO [wireguard] peer key " + WG_PRIVATE + "\n"
               "2026-11-29T08:00:03.000000000Z 2026-11-29T08:00:03Z INFO [ip getter] Public IP address is 203.0.113.21\n"
               "2026-11-29T08:00:04.000000000Z 2026-11-29T08:00:04Z INFO [dns] listening on [2001:db8::53%eth0]:53, "
               "fe80::1 and ::ffff:198.51.100.7\n"
               "2026-11-29T09:10:00.000000000Z 2026-11-29T09:10:00Z WARN [wireguard] handshake older than 180s\n").encode()


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
        self.leak_out, self.leak_runs = "", []  # what leak-test.sh prints, and each run
        self.leak_rc, self.leak_timeouts = None, []  # a forced exit code, and each run's limits
        # Each gluetun namespace, as `ip` and `ping` show it (X-cr-round1-spec-fixes item 4):
        # PIA's point-to-point /32 wg0, whose gateway 10.0.0.1 is its first hop.
        self.netns = {pid: dict(NETNS) for pid in self.pids.values()}
        self.netns_calls = []
        self.logs, self.log_calls = {}, []  # container -> its log (bytes), and each read

    def alive(self, pid, needle):
        return pid is not None and pid not in self.dead

    def now(self):
        return self.t

    def sleep(self, seconds):
        self.t += timedelta(seconds=seconds)

    def run(self, argv, env=None, timeout=120):
        self.calls.append((list(argv), dict(env or {})))
        if argv[0] == "timeout" and argv[4] == "bash":  # the leak test, from its private work folder
            self.leak_timeouts.append((argv[:4], timeout))
            if self.leak_rc is not None:
                return self.leak_rc, self.leak_out
            work = Path(argv[6])
            self.leak_runs.append({"argv": list(argv), "env": dict(env or {}), "work": work,
                                   "files": sorted(p.name for p in work.iterdir()), "mode": work.stat().st_mode & 0o777})
            return (0 if self.leak_out.rstrip().endswith("ok leak_test") else 1), self.leak_out + "a stray stderr-like line\n"
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

    def netns_run(self, pid, argv, timeout=15):
        self.netns_calls.append((pid, list(argv)))
        ns = self.netns[pid]
        if argv[:2] == ["ping", "-n"]:
            return ns["ping"]
        key = {("ip", "-4", "-o", "addr", "show", "dev"): "addr_dev", ("ip", "-4", "-o", "addr", "show"): "addr_all",
               ("ip", "-4", "route", "get"): "route_get", ("ip", "-4", "route", "show", "table"): "routes"}
        for prefix, name in key.items():
            if tuple(argv[:len(prefix)]) == prefix and (name != "addr_all" or len(argv) == len(prefix)):
                return 0, ns[name].replace("TARGET", argv[-1])
        return 1, ""

    def container_logs(self, name, since, until):
        self.log_calls.append((name, since, until))
        return self.logs.get(name, GLUETUN_LOG)

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
        self.cfg_path = self.tmp / "coordinator.json"  # outside the round's folder (it names host paths)
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

    def go(self, command, *extra, day=1, repeat=0, placement=True):
        argv = [command, str(self.cfg_path)]
        if command not in ("end-attempt", "manifest"):
            argv += ["--day-dir", str(self.day_dir(day, repeat))]
            if placement:
                argv += ["--attempt", "r1-a1", "--day", str(day), "--repeat", str(repeat)]
        argv += list(extra)
        import io
        from contextlib import redirect_stdout
        out = io.StringIO()
        with redirect_stdout(out):
            rc = self.main(argv)
        return rc, out.getvalue().splitlines()

    def main(self, argv):
        """co.main, with a test's `--log FILE` turned into what the stub leak test prints."""
        argv = list(argv)
        if "--log" in argv:
            i = argv.index("--log")
            self.host.leak_out = Path(argv[i + 1]).read_text()
            del argv[i:i + 2]
        return co.main(argv, host=self.host)

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
        self.go("bring-up")
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
        self.go("bring-up")
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
        self.go("bring-up")
        data = Path(json.loads(self.cfg_path.read_text())["arms"]["A"]["stack"]) / "data"
        data.mkdir()
        (data / "lodestar.db").write_text("x")
        rc, out = self.go("check-start")
        self.assertIn("isn't empty", out[-1])

    def test_up_starts_each_arm_with_its_declared_settings(self):
        self.pass_leaks()
        self.go("bring-up")
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


class ReviewFixTests(CoordinatorBase):
    """The runner's review of #1150 at 36ba68cf."""

    def test_a_repeated_day_runs_without_repeating_its_repeat_number(self):
        for arm in mk.LAYOUTS["L3"][1]:
            rc, out = self.go("leak", "--arm", arm, "--phase", "before", "--log", str(self.leak_log()), repeat=1)
            self.assertEqual(rc, 0, out)
        rc, out = self.go("bring-up", repeat=1, placement=False)  # as the run sheet runs it
        self.assertEqual(rc, 0, out)
        self.assertEqual(self.state_of(self.day_dir(1, 1))["repeat"], 1)
        rc, out = self.go("bring-up", "--repeat", "2", repeat=1, placement=False)
        self.assertIn("not", out[-1])
        self.assertEqual(rc, 1)

    def state_of(self, d):
        return json.loads((d / "state.json").read_text())

    def test_the_coordinator_file_stays_out_of_the_round_folder(self):
        inside = self.round / "coordinator.json"
        inside.write_text(self.cfg_path.read_text())
        rc = co.main(["check-endpoints", str(inside)], host=self.host)
        self.assertEqual(rc, 1)

    def test_the_leak_test_runs_privately_with_the_arms_settings_and_its_grammar_is_kept(self):
        rc, out = self.go("leak", "--arm", "C", "--phase", "before", "--log", str(self.leak_log()))
        self.assertEqual(rc, 0, out)
        run = self.host.leak_runs[-1]
        self.assertEqual(run["files"], sorted(co.LEAK_FILES))
        self.assertEqual(run["mode"], 0o700)
        self.assertFalse(run["work"].exists())  # removed after: its captures hold addresses
        self.assertEqual(run["argv"][7], json.loads(self.cfg_path.read_text())["arms"]["C"]["stack"])
        self.assertEqual((run["env"]["LODESTAR_ARM"], run["env"]["LODESTAR_UI_PORT"],
                          run["env"]["LODESTAR_GLUETUN_RESTART"]), ("c", "8093", "no"))
        test = self.state()["arms"]["C"]["leak_tests"][-1]
        log = (self.day_dir() / test["log"]).read_text()
        self.assertNotIn("stray", log)  # only the test's own grammar lines
        self.assertTrue(log.rstrip().endswith("ok leak_test"))

    def test_a_hung_leak_test_is_stopped_by_term_and_reads_as_a_failure(self):
        self.host.leak_out, self.host.leak_rc = "ok tunnel_healthy\nok leak_test\n", 124  # even a pass line
        rc, out = self.go("leak", "--arm", "B", "--phase", "before")
        self.assertIn("fail leak_test arm=B", out[0])
        limits, py_timeout = self.host.leak_timeouts[-1]
        self.assertEqual(limits, ["timeout", "-s", "TERM", str(co.LEAK_TIMEOUT)])
        self.assertGreater(py_timeout, co.LEAK_TIMEOUT)
        log = (self.day_dir() / self.state()["arms"]["B"]["leak_tests"][-1]["log"]).read_text()
        self.assertIn("fail leak_test_timed_out", log)

    def test_a_not_run_result_gets_its_own_log(self):
        self.host.leak_out = "fail block_running\nfail leak_test\n"
        self.go("leak", "--arm", "B", "--phase", "before")
        self.host.leak_out = "ok leak_test\n"
        self.go("leak", "--arm", "B", "--phase", "before")
        logs = sorted(p.name for p in self.day_dir().glob("leak-B-*.log"))
        self.assertEqual(logs, ["leak-B-before-1.log", "leak-B-before-2.log"])
        self.assertIn("block_running", (self.day_dir() / "leak-B-before-1.log").read_text())

    def test_arms_are_leak_tested_one_at_a_time(self):
        lock = co.leak_lock(co.load_config(self.cfg_path))
        try:
            rc, out = self.go("leak", "--arm", "B", "--phase", "before", "--log", str(self.leak_log()))
        finally:
            lock.close()
        self.assertIn("one at a time", out[-1])
        self.assertFalse(self.host.leak_runs)

    def test_an_exit_check_must_follow_the_latest_leak_tests(self):
        self.pass_leaks()
        self.assertIn("no exit check", self.go("check-start")[1][-1])
        self.go("bring-up")
        self.assertEqual(self.go("check-start")[0], 0)
        self.host.t += timedelta(minutes=5)
        self.go("leak", "--arm", "A", "--phase", "before", "--log", str(self.leak_log()))
        self.assertIn("after the last exit check", self.go("check-start")[1][-1])
        self.assertIn("after the last exit check", self.go("up")[1][-1])
        self.assertFalse([c for c in self.host.commands() if " up -d " in c])

    def test_round1_stacks_never_restart_on_their_own(self):
        self.pass_leaks()
        self.go("bring-up")
        self.go("up")
        ups = [env for argv, env in self.host.calls if "up" in argv]
        self.assertTrue(ups and all(e["LODESTAR_GLUETUN_RESTART"] == "no" for e in ups))

    def test_a_partial_up_stops_what_it_started(self):
        self.pass_leaks()
        self.go("bring-up")
        real = self.host.run

        def run(argv, env=None, timeout=120):
            if "up" in argv and "lodestar-c" in argv:
                self.host.calls.append((list(argv), dict(env or {})))
                return 1, ""
            return real(argv, env, timeout)
        self.host.run = run
        rc, out = self.go("up")
        self.assertEqual(rc, 1)
        stopped = sorted(c for c in self.host.commands() if c.endswith(" stop"))
        self.assertEqual(len(stopped), 3)  # A and B, and C itself: its containers may be up
        self.assertIn("ok stack_stopped arm=A", out)
        self.assertIn("ok stack_stopped arm=C", out)

    def test_a_stack_that_wont_stop_fails_the_step_after_saving(self):
        self.pass_leaks()
        self.go("bring-up")
        self.go("collect")
        real = self.host.run
        self.host.run = lambda argv, env=None, timeout=120: (1, "") if argv[-1] == "stop" else real(argv, env, timeout)
        rc, out = self.go("stop")
        self.assertEqual(rc, 1)
        self.assertIn("didn't stop", out[-1])
        self.assertTrue(self.state()["shutdown_utc"])
        self.assertTrue((self.day_dir() / "archive.json").exists())

    def test_a_dead_sampler_is_started_again(self):
        self.pass_leaks()
        self.go("bring-up")
        self.go("collect")
        pid = self.state()["sampler_pid"]
        self.host.dead.add(pid)
        self.go("collect")
        self.assertNotEqual(self.state()["sampler_pid"], pid)
        self.assertIn("started again", self.state()["notes"][-1])

    def test_commands_run_in_a_scrubbed_environment(self):
        with mock.patch.dict(co.os.environ, {"LODESTAR_DHT_UTP": "true", "HTTPS_PROXY": "http://x"}):
            env = co.base_env({"LODESTAR_ARM": "b"})
        self.assertNotIn("LODESTAR_DHT_UTP", env)
        self.assertNotIn("HTTPS_PROXY", env)
        self.assertEqual(env["LODESTAR_ARM"], "b")


class BindComparatorTests(CoordinatorBase):
    def comparator_run(self, ended=True, kind="comparator"):
        run = self.tmp / "cr-ab-comp-2"
        run.mkdir(exist_ok=True)
        (run / "run.json").write_text(json.dumps({"settings": {"run_kind": kind}}))
        if ended:
            (run / "result.json").write_text(json.dumps({"ended": iso(T0 - timedelta(hours=12, minutes=59))}))
        return run

    def setUp(self):
        super().setUp()
        comp = json.loads((self.round / "comparator.json").read_text())
        comp["identity"] = {"binary_sha256": "b" * 64}
        (self.round / "comparator.json").write_text(json.dumps(comp))
        self.segmented = []

    def segment(self, run_dir, **change):
        # The comparator run's own report, as analyze_cr.py segment makes it (faked here: the
        # analyzer's segment tests cover the real thing).
        self.segmented.append(Path(run_dir))
        comp = json.loads((self.round / "comparator.json").read_text())
        return dict({k: comp.get(k) for k in ("kind", "timeline", "window_hours", "identity")}, **change)

    def bind(self, run, segment=None):
        argv = ["bind-comparator", str(self.cfg_path), "--comparator-run", str(run)]
        with mock.patch.object(co.collect, "host_key", lambda: "0" * 32), \
                mock.patch.object(co, "comparator_segment", segment or self.segment):
            import io
            from contextlib import redirect_stdout
            out = io.StringIO()
            with redirect_stdout(out):
                rc = co.main(argv, host=self.host)
        return rc, out.getvalue().splitlines()

    def test_binds_once_from_the_finished_run_with_identities_computed_now(self):
        rc, out = self.bind(self.comparator_run())
        self.assertEqual(rc, 0, out)
        b = json.loads((self.round / "comparator-binding.json").read_text())
        self.assertEqual(b["run_id"], "comparator-cr-ab-comp-2")
        self.assertEqual(b["host_identity"], collect.salted_hash(SALT, "host", "0" * 32))
        self.assertEqual(b["network_identity"], {"uplink_hash": collect.salted_hash(SALT, "ip", UPLINK),
                                                 "provider": "pia", "protocol": "wireguard"})
        self.assertEqual((b["ended_utc"], b["bound_after_window"]), (iso(T0 - timedelta(hours=12, minutes=59)), True))
        self.assertNotIn(UPLINK, json.dumps(b))
        rc, out = self.bind(self.comparator_run())
        self.assertIn("never changed", out[-1])

    def test_refuses_a_run_directory_other_than_the_reports(self):
        run = self.comparator_run()
        comp = json.loads((self.round / "comparator.json").read_text())
        other_window = dict(comp["timeline"], window_start_utc=iso(T0 - timedelta(days=3)))
        for change in ({"timeline": other_window}, {"identity": {"binary_sha256": "c" * 64}},
                       {"kind": "comb"}, {"window_hours": 12}):
            with self.subTest(change=sorted(change)):
                rc, out = self.bind(run, lambda d, change=change: self.segment(d, **change))
                self.assertEqual(rc, 1)
                self.assertIn("isn't the comparator", out[-1])
                self.assertFalse((self.round / "comparator-binding.json").exists())

        def broken(d):
            raise ValueError("no samples")
        self.assertIn("can't be segmented", self.bind(run, broken)[1][-1])
        self.assertEqual(self.bind(run)[0], 0)
        self.assertEqual(self.segmented[-1], run)

    def test_the_real_segment_runs_on_the_run_directory(self):
        sys.path.insert(0, str(HERE.parent))
        import analyze_cr
        with mock.patch.object(analyze_cr, "analyze_segment", lambda root: {"root": str(root)}):
            self.assertEqual(co.comparator_segment(self.tmp), {"root": str(self.tmp)})

    def test_refuses_a_running_comparator_or_another_kind(self):
        rc, out = self.bind(self.comparator_run(ended=False))
        self.assertEqual(rc, 1)
        (self.tmp / "cr-ab-comp-2" / "result.json").write_text(json.dumps({"ended": None}))
        self.assertIn("hasn't finished", self.bind(self.tmp / "cr-ab-comp-2")[1][-1])
        rc, out = self.bind(self.comparator_run(kind="comb"))
        self.assertIn("isn't a comparator", out[-1])

    def test_refuses_a_comparator_that_ended_before_its_window(self):
        run = self.comparator_run()
        (run / "result.json").write_text(json.dumps({"ended": iso(T0 - timedelta(hours=14))}))
        rc, out = self.bind(run)
        self.assertIn("interrupted", out[-1])
        self.assertFalse((self.round / "comparator-binding.json").exists())

    def test_the_manifest_carries_the_binding_and_the_rehearsal_gate_reads_it(self):
        comp = json.loads((self.round / "comparator.json").read_text())
        comp.pop("binding")  # as the old collector's report: no binding
        (self.round / "comparator.json").write_text(json.dumps(comp))
        self.bind(self.comparator_run())
        self.host.t = T0 - timedelta(hours=5)
        day = self.round / "rehearsal"
        argv = ["--day-dir", str(day), "--attempt", "r1-a1", "--day", "1", "--rehearsal"]
        for arm in mk.LAYOUTS["L3"][1]:
            self.main(["leak", str(self.cfg_path), *argv, "--arm", arm, "--phase", "before", "--log", str(self.leak_log())])
        self.main(["bring-up", str(self.cfg_path), *argv])
        self.assertEqual(co.main(["check-start", str(self.cfg_path), "--day-dir", str(day)], host=self.host), 0)
        ManifestTests.make_day(self, 1)
        ManifestTests.make_day(self, 2)
        rc, out = self.go("manifest")
        self.assertEqual(rc, 0, out)
        man = json.loads((self.round / "manifest.json").read_text())
        self.assertEqual(man["comparator_binding"]["path"], "comparator-binding.json")


class RehearsalAndIdentityTests(CoordinatorBase):
    def test_the_rehearsal_never_runs_beside_the_comparator(self):
        comp = json.loads((self.round / "comparator.json").read_text())
        comp["timeline"]["window_end_utc"] = iso(T0 + timedelta(hours=1))
        (self.round / "comparator.json").write_text(json.dumps(comp))
        day = self.round / "rehearsal"
        argv = ["--day-dir", str(day), "--attempt", "r1-a1", "--day", "1", "--rehearsal"]
        for arm in mk.LAYOUTS["L3"][1]:
            self.main(["leak", str(self.cfg_path), *argv, "--arm", arm, "--phase", "before", "--log", str(self.leak_log())],
                    )
        self.main(["bring-up", str(self.cfg_path), *argv])
        self.assertEqual(co.main(["check-start", str(self.cfg_path), "--day-dir", str(day)], host=self.host), 1)

    def test_the_rehearsal_runs_inside_the_12h_and_never_reaches_the_manifest(self):
        self.host.t = T0 - timedelta(hours=5)  # inside the comparator's 12 h
        day = self.round / "rehearsal"
        argv = ["--day-dir", str(day), "--attempt", "r1-a1", "--day", "1", "--rehearsal"]
        for arm in mk.LAYOUTS["L3"][1]:
            self.assertEqual(self.main(["leak", str(self.cfg_path), *argv, "--arm", arm, "--phase", "before",
                                      "--log", str(self.leak_log())]), 0)
        co.main(["bring-up", str(self.cfg_path), *argv], host=self.host)
        self.assertEqual(co.main(["check-start", str(self.cfg_path), "--day-dir", str(day)], host=self.host), 0)
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
        self.assertFalse([c for c in self.host.commands() if not c.startswith("timeout ")])  # nothing stopped
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

    def test_a_check_is_stamped_with_its_first_reading_not_its_end(self):
        # The reads take time; a check stamped at its end would put its own readings before
        # it, and the gate would judge them against the check in force before this one.
        self.pass_leaks()
        read = self.host.egress_ip

        def slow(pid, url):
            ip = read(pid, url)
            self.host.t += timedelta(seconds=7)
            return ip
        self.host.egress_ip = slow
        self.go("bring-up")
        self.host.ip_of_pid[102] = "203.0.113.99"
        rc, out = self.go("bring-up")
        self.assertEqual(rc, 0, out)
        s = self.state()
        check = s["exit_checks"][-1]
        mine = [r["utc"] for a in s["arms"].values() for r in a["egress"][-1:]]
        self.assertEqual(check["utc"], min(mine))
        self.assertLess(co.parse_utc(check["utc"]), self.host.now())
        self.assertTrue(all(co.parse_utc(check["utc"]) <= co.parse_utc(u) for u in mine))

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


class RehearsalWindowTests(CoordinatorBase):
    """X-cr-round1-spec-fixes item 1: a 30 min warm-up and a 3 h window for the rehearsal only,
    and the 4 h cap from the first start."""

    def rehearsal(self, stagger=timedelta(0)):
        self.host.t = T0 - timedelta(hours=5)
        day = self.round / "rehearsal"
        argv = ["--day-dir", str(day), "--attempt", "r1-a1", "--day", "1", "--rehearsal"]
        for arm in mk.LAYOUTS["L3"][1]:
            self.assertEqual(self.main(["leak", str(self.cfg_path), *argv, "--arm", arm, "--phase", "before",
                                        "--log", str(self.leak_log())]), 0)
        self.assertEqual(co.main(["bring-up", str(self.cfg_path), *argv], host=self.host), 0)
        self.first = self.host.t
        spawn = self.host.spawn

        def slow_spawn(argv_, log, env=None):  # the coordinator's own stagger between collectors
            pid = spawn(argv_, log, env)
            if "collect.py" in argv_[2]:
                (Path(argv_[argv_.index("--root") + 1])).mkdir(parents=True, exist_ok=True)
                (Path(argv_[argv_.index("--root") + 1]) / "run.json").write_text("{}")
                self.host.t += stagger
            return pid
        self.host.spawn = slow_spawn
        import io
        from contextlib import redirect_stdout
        out = io.StringIO()
        with redirect_stdout(out):
            self.assertEqual(co.main(["collect", str(self.cfg_path), "--day-dir", str(day)], host=self.host), 0)
        return day, out.getvalue().splitlines()

    def test_the_rehearsals_collectors_run_30_min_then_3_h(self):
        self.rehearsal()
        for argv in (a for a in self.host.spawned if "collect.py" in a[2]):
            self.assertEqual(argv[argv.index("--warmup-minutes") + 1], "30")
            self.assertEqual(argv[argv.index("--baseline-hours") + 1], "3")
            self.assertIn("--rehearsal", argv)

    def test_a_measurement_day_keeps_rule_1_1s_warm_up(self):
        self.pass_leaks()
        self.go("bring-up")
        self.go("collect")
        collectors = [a for a in self.host.spawned if "collect.py" in a[2]]
        self.assertEqual(len(collectors), 3)
        for argv in collectors:
            for flag in ("--warmup-minutes", "--baseline-hours", "--rehearsal"):
                self.assertNotIn(flag, argv)

    def test_the_worst_case_end_is_reported_against_the_cap(self):
        _, out = self.rehearsal()
        self.assertTrue(any(ln.startswith("ok rehearsal ends about") for ln in out), out)
        _, out = self.rehearsal_again(timedelta(minutes=20))
        self.assertTrue(any(ln.startswith("warn rehearsal ends about") for ln in out), out)

    def rehearsal_again(self, stagger):
        import shutil
        shutil.rmtree(self.round / "rehearsal")
        self.host.spawned.clear()
        del self.host.spawn
        return self.rehearsal(stagger)

    def test_watch_stops_every_arm_at_the_cap(self):
        day, _ = self.rehearsal(timedelta(minutes=20))  # the last window would end after 4 h
        self.first = co.first_start(json.loads((day / "state.json").read_text()))
        argv = ["watch", str(self.cfg_path), "--day-dir", str(day)]
        self.assertEqual(co.main(argv, host=self.host), 0)
        self.assertGreaterEqual(self.host.t, self.first + timedelta(hours=4))
        self.assertLess(self.host.t, self.first + timedelta(hours=4, seconds=5))
        self.assertEqual(len([c for c in self.host.commands() if c.endswith(" stop")]), 3)
        state = json.loads((day / "state.json").read_text())
        self.assertIn("4 h cap", state["notes"][-1])
        self.assertTrue((day / "archive.json").exists())

    def test_a_measurement_day_has_no_cap(self):
        self.pass_leaks()
        self.go("bring-up")
        self.go("collect")
        self.host.t = T0 + timedelta(hours=5)  # past 4 h from the start: a measurement day keeps going
        rc, out = self.main_watch(max_loops=2)
        self.assertEqual(out, ["ok watch paused (test bound)"])

    def main_watch(self, max_loops):
        cfg = co.load_config(self.cfg_path)
        day = co.Day(cfg, self.day_dir())
        return 0, co.cmd_watch(cfg, day, self.host, None, max_loops=max_loops)


class GatewayTests(CoordinatorBase):
    """Item 4: bring-up refuses a tunnel_target that isn't the arm's WireGuard gateway, and
    records only that it was."""

    def bring_up(self):
        self.pass_leaks()
        return self.go("bring-up")

    def test_the_first_hop_gateway_is_recorded_without_its_address(self):
        rc, out = self.bring_up()
        self.assertEqual(rc, 0, out)
        s = self.state()
        for arm in s["arms"].values():
            self.assertEqual((arm["tunnel_target_is_gateway"], arm["tunnel_gateway_check"]), (True, "first_hop"))
        self.assertIn(["ping", "-n", "-c", "1", "-t", "1", "-W", "2", "10.0.0.1"], [c[1] for c in self.host.netns_calls])
        self.assertNotIn("10.0.0.1", (self.day_dir() / "state.json").read_text())
        self.go("archive", placement=False)
        archive = (self.day_dir() / "archive.json").read_text()
        self.assertNotIn("10.0.0.1", archive)
        self.assertTrue(all(a["tunnel_target_is_gateway"] for a in json.loads(archive)["arms"].values()))

    def test_a_target_beyond_the_gateway_is_refused(self):
        self.host.netns[102]["ping"] = (1, "From 10.9.0.1 icmp_seq=1 Time to live exceeded\n")
        rc, out = self.bring_up()
        self.assertEqual(rc, 1)
        self.assertIn("arm A's tunnel_target isn't its WireGuard gateway (first_hop)", out[-1])
        self.assertEqual(self.state()["exits"], {})

    def test_a_namespace_address_is_refused(self):
        for addr in ("10.0.0.1/32 scope global wg0", "10.0.0.1/16 brd 10.0.255.255 scope global eth0"):
            self.host.netns[101]["addr_all"] = NETNS["addr_all"] + f"5: x    inet {addr}\n"
            self.assertIn("target_is_a_namespace_address", self.bring_up()[1][-1])

    def test_a_target_outside_the_tunnel_is_refused(self):
        for route in ("local 10.0.0.1 dev lo table local src 10.0.0.1\n", "10.0.0.1 dev eth0 src 172.18.0.2\n"):
            self.host.netns[103]["route_get"] = route
            self.assertIn("target_not_through_the_tunnel", self.bring_up()[1][-1])

    def test_a_peer_address_or_a_via_route_must_equal_the_target(self):
        for key, ok, bad in (("addr_dev", "inet 10.1.2.3 peer 10.0.0.1/32 scope global wg0\n",
                              "inet 10.1.2.3 peer 10.0.0.9/32 scope global wg0\n"),
                             ("routes", "default via 10.0.0.1 dev wg0 table 51820\n",
                              "default via 10.0.0.9 dev wg0 table 51820\n")):
            self.setUp()
            self.host.netns[101][key] = ok
            self.host.netns[101]["ping"] = (1, "")  # never reached: the interface says it
            rc, out = self.bring_up()
            self.assertEqual(rc, 0, out)
            self.assertEqual(self.state()["arms"]["B"]["tunnel_gateway_check"],
                             "peer_address" if key == "addr_dev" else "route_via")
            self.host.netns[101][key] = bad
            self.assertIn("isn't its WireGuard gateway", self.go("bring-up")[1][-1])

    def test_collect_refuses_a_target_changed_after_bring_up(self):
        self.bring_up()
        cfg = json.loads(self.cfg_path.read_text())
        cfg["arms"]["C"]["tunnel_target"] = "10.0.0.7"
        self.cfg_path.write_text(json.dumps(cfg))
        rc, out = self.go("collect", placement=False)
        self.assertEqual(rc, 1)
        self.assertIn("arm C's tunnel_target isn't the one bring-up checked", out[-1])
        self.assertEqual(self.host.spawned, [])
        self.assertNotIn("10.0.0.1", (self.day_dir() / "state.json").read_text())

    def test_the_interface_is_always_wg0(self):
        cfg = json.loads(self.cfg_path.read_text())
        cfg["arms"]["B"]["tunnel_interface"] = "eth0"  # ignored: only the tunnel's interface counts
        self.cfg_path.write_text(json.dumps(cfg))
        self.bring_up()
        self.assertTrue(all(c[1][-1] == "wg0" for c in self.host.netns_calls if c[1][:6] == ["ip", "-4", "-o", "addr", "show", "dev"]))
        self.host.netns[101]["route_get"] = "10.0.0.1 via 172.18.0.1 dev eth0 src 172.18.0.2\n"
        self.assertIn("target_not_through_the_tunnel", self.go("bring-up")[1][-1])

    def test_no_tunnel_interface_is_refused(self):
        self.host.netns[101]["addr_dev"] = ""
        self.assertIn("tunnel_interface_missing", self.bring_up()[1][-1])


class GluetunLogTests(CoordinatorBase):
    """Item 6: each arm's gluetun log for the day, redacted, in the archive and the manifest."""

    def test_redaction_removes_addresses_and_keys_and_keeps_times(self):
        text = co.redact_log(GLUETUN_LOG.decode())
        for secret in ("192.0.2.91", "203.0.113.21", "198.51.100.7", "2001:db8::53", "fe80::1", WG_PRIVATE):
            self.assertNotIn(secret, text)
        self.assertEqual(text.count("[ipv4]"), 2)
        self.assertEqual(text.count("[ipv6]"), 3)
        self.assertEqual(text.count("[key]"), 1)
        self.assertIn("2026-11-29T09:10:00.000000000Z 2026-11-29T09:10:00Z WARN [wireguard] handshake older than 180s",
                      text)
        self.assertIn("[ipv4]:1337", text)

    def test_a_key_is_only_a_44_character_base64_value(self):
        self.assertEqual(co.redact_log("x" * 43 + "=="), "x" * 43 + "==")
        self.assertEqual(co.redact_log("a" * 43 + "="), "a" * 43 + "=")  # its last bits aren't zero
        self.assertEqual(co.redact_log("A" * 43 + "="), "[key]")
        self.assertEqual(co.redact_log("v3.40.0 10:00:00 10:00:00.5 1.2.3"), "v3.40.0 10:00:00 10:00:00.5 1.2.3")

    def test_ipv6_redaction_fails_closed(self):
        for text in ("ip:2001:db8::1", "hostname:fe80::1", "server 2001:db8::1:51820", "x=::1", "[::ffff:10.0.0.1]:53"):
            out = co.redact_log(text)
            self.assertIn("[ipv6]", out, text)
            for part in ("db8", "fe80", "::1", "10.0.0.1"):
                self.assertNotIn(part, out, text)

    def test_a_stale_log_is_removed_when_the_days_cant_be_read(self):
        self.pass_leaks()
        self.go("bring-up")
        self.go("archive", placement=False)
        self.assertTrue((self.day_dir() / "A" / "gluetun.log").exists())
        self.host.logs["gluetun-a"] = None
        self.go("archive", placement=False)
        self.assertFalse((self.day_dir() / "A" / "gluetun.log").exists())

    def test_the_archive_saves_each_arms_log_from_its_start_to_the_shutdown(self):
        self.pass_leaks()
        self.go("bring-up")
        self.go("up", placement=False)
        up = self.host.t
        self.go("collect", placement=False)
        self.host.t += timedelta(hours=25)
        self.go("stop", placement=False)
        s = self.state()
        self.assertEqual(sorted(self.host.log_calls), sorted((f"gluetun-{a.lower()}", iso(up), s["shutdown_utc"])
                                                             for a in "BAC"))
        archive = json.loads((self.day_dir() / "archive.json").read_text())
        for arm in "BAC":
            log = self.day_dir() / arm / "gluetun.log"
            self.assertEqual(archive["arms"][arm]["gluetun_log_sha256"], co.sha256(log))
            for secret in ("192.0.2.91", "203.0.113.21", WG_PRIVATE):
                self.assertNotIn(secret, log.read_text())

    def test_an_unreadable_log_is_noted_once(self):
        self.pass_leaks()
        self.go("bring-up")
        self.host.logs["gluetun-a"] = None
        self.go("archive", placement=False)
        self.go("archive", placement=False)
        notes = [n for n in self.state()["notes"] if "gluetun's log" in n]
        self.assertEqual(len(notes), 1)
        self.assertNotIn("gluetun_log_sha256", json.loads((self.day_dir() / "archive.json").read_text())["arms"]["A"])


class EndAttemptTests(CoordinatorBase):
    """Item 2: end-attempt's reasons, which gate-round reads for an S attempt."""

    def test_reasons_are_tokens_naming_the_attempts_arms(self):
        for reason, ok in (("rehearsal_drop:A,rehearsal_drop:C", True), ("product_failure:A", True),
                           ("other:runner_ruled", True), ("drop_c", False), ("product_failure:D", False),
                           ("rehearsal_drop:a", False), ("A failed", False)):
            rc, out = self.go("end-attempt", "--attempt", "r1-a1", "--reason", reason)
            self.assertEqual(rc, 0 if ok else 1, (reason, out))

    def test_a_rehearsal_drop_is_only_before_the_first_day(self):
        ManifestTests.make_day(self, 1)
        rc, out = self.go("end-attempt", "--attempt", "r1-a1", "--reason", "rehearsal_drop:C")
        self.assertEqual(rc, 1)
        self.assertIn("before the attempt's first day", out[-1])


class LaunchPathTests(unittest.TestCase):
    def test_the_commands_it_launches_exist_in_this_checkout(self):
        self.assertTrue((co.TOOLS / "collect.py").is_file())
        self.assertTrue((co.TOOLS / "host_samples.py").is_file())


class LeakTimeoutTests(unittest.TestCase):
    """A real subprocess: `timeout` sends TERM, so the test's EXIT trap runs its cleanup while
    its work folder is still there, and the result is a failure. The stand-in script mirrors
    leak-test.sh's traps (TERM exits 143, EXIT cleans up)."""

    def test_term_runs_the_tests_own_cleanup_before_the_folder_goes(self):
        import shutil as sh
        with tempfile.TemporaryDirectory() as d:
            here, marker = Path(d) / "steps", Path(d) / "cleanup-saw"
            here.mkdir()
            (here / "leak-test.sh").write_text(
                "#!/usr/bin/env bash\n"
                f"trap 'ls \"$1\" > {marker}; echo ok cleanup_ran' EXIT\n"
                "trap 'exit 143' TERM\n"
                "echo ok started\n"
                "sleep 30 & wait $!\n"
                "echo ok leak_test\n")
            for name in ("block-lib.sh", "compose.yaml"):
                (here / name).write_text("")
            cfg = {"arms": {"B": {"suffix": "-b", "port": 8091, "stack": d}}}
            if sh.which("timeout") is None:
                self.skipTest("coreutils timeout not installed")
            with mock.patch.object(co, "HERE", here), mock.patch.object(co, "LEAK_TIMEOUT", 2), \
                    mock.patch.object(co, "LEAK_CLEANUP_SECONDS", 30):
                lines = co.run_leak_test(cfg, co.Host(), "B")
            self.assertIn("leak-test.sh", marker.read_text())  # the folder was there for the cleanup
        self.assertEqual(lines[-2:], ["fail leak_test_timed_out", "fail leak_test"])
        self.assertIn("ok cleanup_ran", lines)
        self.assertEqual(co.leak_verdict("\n".join(lines)), "fail")


class CollectorStateTests(unittest.TestCase):
    def test_the_final_boundarys_retries_reach_the_archives_egress(self):
        with tempfile.TemporaryDirectory() as d:
            run = Path(d)
            (run / "run.json").write_text(json.dumps({"settings": {"baseline_hours": 2}}))
            rows = [{"time": iso(T0), "boundary": [2], "egress": {"read": "failed"},
                     "egress_retries": [{"read": "failed", "utc": iso(T0 + timedelta(seconds=20))},
                                        {"read": "ok", "hash": "ab" * 32, "utc": iso(T0 + timedelta(seconds=40))}]}]
            (run / "samples.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
            st = co.collector_state(run)
        self.assertEqual(st["egress"], [{"utc": iso(T0 + timedelta(seconds=40)), "hash": "ab" * 32}])
        self.assertEqual(st["final_boundary_utc"], iso(T0))


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
        for arm in arms:
            (d / arm).mkdir(exist_ok=True)
            (d / arm / "gluetun.log").write_text(f"redacted log of {arm}\n")

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
        rc, out = self.go("end-attempt", "--attempt", "r1-a1", "--reason", "product_failure:C")
        self.assertEqual(rc, 0, out)
        self.assertEqual(json.loads((self.round / "attempt-ends.json").read_text()),
                         {"r1-a1": {"reason": "product_failure:C", "days_run": [1]}})
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
                 "uplink_rx_missed_errors": 0} for m, rx in ((0, 5), (30, 5), (59, 9), (61, 12), (90, 12))]
        self.assertEqual(hs.drop_deltas(rows), [
            {"hour_utc": iso(T0), "uplink_rx_dropped": 4, "uplink_rx_missed_errors": 0, "uplink_tx_dropped": 0},
            # carried across the hour: 9 (the last reading before it) to 12, not 12 to 12
            {"hour_utc": iso(T0 + timedelta(hours=1)), "uplink_rx_dropped": 3, "uplink_rx_missed_errors": 0,
             "uplink_tx_dropped": 0}])

    def test_assemble_dedups_computes_medians_and_hourly_coverage(self):
        rows = [{"utc": iso(T0 + timedelta(seconds=10 * k)), "device": "nvme0n1", "cpu": 0.4, "steal": 0.0,
                 "disk_write_ms": 3.0, "disk_inflight": 0, "uplink_drops_total": 0,
                 "tunnels": {"B": {"rtt_ms": 30.0, "lost": False}}}
                 for k in range(360) if k % 50]  # a stall every 50th slot: those stay missing
        rows.append(dict(rows[0]))
        headers = [{"kind": "header", "started_utc": iso(T0), "device": "nvme0n1", "tunnels": ["B", "C"], "vcpus": 8}]
        rehearsal = [{"utc": iso(T0), "tunnels": {"B": {"rtt_ms": v, "lost": False}}} for v in (20, 30, 90)]
        doc = hs.assemble(rows, "nvme0n1", headers, rehearsal, end=T0 + timedelta(hours=2, minutes=5))
        self.assertEqual((doc["rehearsal_rtt_median_ms"], doc["vcpus"], doc["tunnels"]), ({"B": 30}, 8, ["B", "C"]))
        self.assertEqual(len(doc["samples"]), 360 - 8)
        cov = doc["coverage_by_hour"][0]["coverage"]
        self.assertEqual(cov["cpu"], round(352 / 360, 4))
        self.assertLess(cov["tunnel_B"], 0.98)
        self.assertEqual(cov["tunnel_C"], 0)  # a configured tunnel that never answered is listed, at 0
        # every hour to the shutdown is listed, the empty ones at 0
        self.assertEqual([h["hour_utc"] for h in doc["coverage_by_hour"]],
                         [iso(T0 + timedelta(hours=k)) for k in range(3)])
        self.assertEqual(set(doc["coverage_by_hour"][2]["coverage"].values()), {0})

    def test_partial_first_and_last_hours_expect_only_their_covered_slots(self):
        start, end = T0 + timedelta(minutes=30), T0 + timedelta(hours=2, minutes=29, seconds=50)
        rows, t = [], start
        while t <= end:
            rows.append({"utc": iso(t), "device": "sda", "cpu": 0.4, "steal": 0.0, "disk_inflight": 0,
                         "uplink_drops_total": 0, "tunnels": {"B": {"rtt_ms": 30.0, "lost": False}}})
            t += timedelta(seconds=10)
        headers = [{"kind": "header", "started_utc": iso(start), "device": "sda", "tunnels": ["B"], "vcpus": 8}]
        doc = hs.assemble(rows, "sda", headers, end=end)
        self.assertEqual([h["expected"] for h in doc["coverage_by_hour"]], [180, 360, 180])
        self.assertTrue(all(set(h["coverage"].values()) == {1.0} for h in doc["coverage_by_hour"]))
        rows = [r for r in rows if not r["utc"].startswith(iso(T0 + timedelta(hours=1))[:15])]  # a 10 min gap
        doc = hs.assemble(rows, "sda", headers, end=end)
        self.assertLess(doc["coverage_by_hour"][1]["coverage"]["cpu"], 0.98)
        self.assertEqual(doc["coverage_by_hour"][0]["coverage"]["cpu"], 1.0)

    def test_assemble_refuses_another_device_or_no_header(self):
        rows = [{"utc": iso(T0), "device": "sda"}]
        headers = [{"kind": "header", "started_utc": iso(T0), "device": "sda", "tunnels": [], "vcpus": 8}]
        with self.assertRaises(ValueError):
            hs.assemble(rows, "nvme0n1", headers)
        with self.assertRaises(ValueError):
            hs.assemble(rows, "sda", [])
        with self.assertRaises(ValueError):
            hs.assemble(rows, "sda", [dict(headers[0], vcpus=None)])

    def test_a_late_slot_is_skipped_not_backfilled(self):
        import threading
        out = Path(self.temp.name) / "samples.jsonl"
        times = iter([T0 + timedelta(seconds=s) for s in (0.2, 0.3, 12.5, 12.6, 20.1, 20.2, 30.0, 31.0)])

        class S:
            device, tunnels = "nvme0n1", {"B": ("gluetun-b", "10.0.0.1")}

            def sample(self, slot):
                return {"utc": iso(slot)}
        stop = threading.Event()
        hs.run_loop(S(), out, stop, now=lambda: next(times), sleep=lambda s: None, until=T0 + timedelta(seconds=30))
        self.assertEqual([r["utc"] for r in hs.load_lines(out)], [iso(T0), iso(T0 + timedelta(seconds=20))])
        header = hs.load_headers(out)[0]
        self.assertEqual((header["device"], header["tunnels"], header["vcpus"]), ("nvme0n1", ["B"], hs.os.cpu_count()))


if __name__ == "__main__":
    unittest.main()
