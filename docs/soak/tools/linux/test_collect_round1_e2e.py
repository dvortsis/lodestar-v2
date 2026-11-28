"""X-cr-round1-collect, end to end: three arms' collectors, the coordinator and the host
sampler's format, through `analyze_cr.py segment --declaration` and `gate-round` on main.

Each arm's collector runs a real 60 min + 24 h window (no --rehearsal) on a virtual clock,
against test_gate_round's Round1Sim as its Lodestar (test_collect_round1.round1_metrics).
Starts are staggered as §6.3's rehearsal exercises them. The coordinator writes each day's
exit assignment, activity through the overhang, leak tests and archive with FakeHost (no
Docker, namespaces or VPN: those need the box, and its rehearsal runs them). The host
samples come from host_samples.py's real Sampler and run loop on a fake /proc and /sys, and
its own `assemble`. The manifest comes
from the coordinator and gate-round judges it.

Layout L3 (B, A and C on two days) is run once and shared by the tests, as it takes a while.
"""

import json
import sys
import tempfile
import unittest
from datetime import timedelta
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

import analyze_cr  # noqa: E402
import collect  # noqa: E402
import host_samples as hs  # noqa: E402
import test_collect_round1 as r1  # noqa: E402
import test_collect_round1_coordinator as tc  # noqa: E402
from test_collect import FakeClock, FakeLodestar, fixed_volumes, write_cgroup  # noqa: E402
from test_collect_spec import CHRONY, PASSWORD, PUBLIC_IP, FakeAPI, SpecDocker, write_net  # noqa: E402

mk = r1.gr.mk
LAYOUT = mk.LAYOUTS["L3"]
HOST = collect.salted_hash(r1.SALT, "host", r1.MACHINE_ID)
NETWORK = {"uplink_hash": collect.salted_hash(r1.SALT, "ip", PUBLIC_IP), "provider": "pia", "protocol": "wireguard"}
DAY_START = {1: tc.T0, 2: tc.T0 + timedelta(hours=26)}
PORTS = {"B": 8091, "A": 8092, "C": 8093}


def collector_run(root, arm, day, start, exit_ip, salt_file, password_file, run_id, fail_call=None):
    """One arm's real-window collector on fakes, started at `start` (its Lodestar 2 s before)."""
    clock = FakeClock()
    clock.base = start
    lodestar = FakeLodestar(clock)
    try:
        cgroup = root.parent / f".cgroup-{arm}"
        write_cgroup(cgroup)
        proc = root.parent / f".proc-{arm}"
        write_net(proc)
        preset = mk.PRESET[arm]
        switches = mk.ALL_OFF if arm == "C" else mk.ALL_ON
        sim = r1.gr.Round1Sim(arm=arm, preset=preset, switches=switches, hours=24, hour_s=3600)
        gauge = dict(switches, safe_restart=True)
        lodestar.metrics = r1.round1_metrics(clock, sim, gauge)
        instance = collect.DockerInstance(SpecDocker(env=r1.env_for(gauge)), "lodestar", cgroup, clock=clock.clock,
                                          sleep=clock.sleep, proc_root=proc)
        argv = ["--mode", "docker", "--root", str(root), "--commit", mk.COMMIT, "--url", lodestar.url,
                "--intensity", preset, "--run-kind", "comb", "--password-file", str(password_file),
                "--spec-commit", mk.SPEC_COMMIT, "--min-free-bytes", "3", "--warn-free-bytes", "2",
                "--stop-free-bytes", "1", "--checklist", "auto_update=excluded",
                "--next-run", "auto_update=2026-12-10T03:00:00Z", "--checklist", "backup=excluded",
                "--next-run", "backup=2026-12-09T03:00:00Z", "--round", "1", "--attempt", "r1-a1",
                "--day", str(day), "--arm", arm, "--run-id", run_id,
                "--identity-salt-file", str(salt_file)]
        args = collect.build_parser().parse_args(argv)
        calls = []

        def egress(pid):  # `fail_call`: that read fails (the final boundary's is the 26th)
            calls.append(pid)
            if len(calls) == fail_call:
                raise OSError("echo service hiccup")
            return exit_ip
        args.tick_seconds = 5.0
        c = collect.Collector(args, instance, clock=clock.clock, sleep=clock.sleep, now=clock.now,
                              volumes=fixed_volumes(), ip_lookup=lambda url: PUBLIC_IP,
                              api_call=FakeAPI(intensity=preset),
                              run_command=lambda a: CHRONY if a[0] == "chronyc" else None,
                              egress_lookup=egress, host_id=lambda: r1.MACHINE_ID)
        return c.run()
    finally:
        lodestar.close()


def sample_host(root, out, arms, start, end):
    """host_samples.py's real Sampler and run loop over [start, end], on a fake /proc and /sys
    whose counters advance like a quiet 8 vCPU host's, with every tunnel answering its ping."""
    proc, sysfs = root / "proc", root / "sys" / "class/net/eth0/statistics"
    proc.mkdir(parents=True, exist_ok=True)
    sysfs.mkdir(parents=True, exist_ok=True)
    for name, value in (("rx_dropped", 7), ("tx_dropped", 0)):
        (sysfs / name).write_text(f"{value}\n")
    pids = {f"gluetun-{a.lower()}": str(100 + i) for i, a in enumerate(arms)}

    def run(argv, timeout):
        if argv[0] == "docker":
            return 0, pids[argv[-1]] + "\n"
        return 0, "1 packets transmitted, 1 received\nrtt min/avg/max/mdev = 31.0/31.0/31.0/0.0 ms\n"

    class Driven(hs.Sampler):
        n = 0

        def sample(self, slot):
            Driven.n += 1
            k = Driven.n  # 10 s of 8 vCPUs is 8000 jiffies: 42 % busy, 1 % steal
            (proc / "stat").write_text(f"cpu  {3280 * k} 0 0 {4560 * k} 0 0 0 {80 * k} 0 0\n")
            (proc / "diskstats").write_text(f" 259 0 {mk.DEVICE} 1 0 8 1 {12 * k} 0 96 {42 * k} 0 9 9 0 0 0 0\n")
            return super().sample(slot)
    sampler = Driven(mk.DEVICE, "eth0", {a: (f"gluetun-{a.lower()}", "10.0.0.1") for a in arms},
                     proc_root=proc, sys_root=root / "sys", run=run)
    clock = [start]

    def sleep(seconds):
        clock[0] += timedelta(seconds=seconds)
    import threading
    from unittest import mock
    with mock.patch.object(hs.os, "cpu_count", lambda: 8):  # the box's 8 vCPUs (the declaration's)
        hs.run_loop(sampler, out, threading.Event(), now=lambda: clock[0], sleep=sleep, until=end)


class EndToEnd(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.tmp = Path(cls.temp.name)
        cls.round = cls.tmp / "round1"
        cls.round.mkdir()
        salt = cls.tmp / "salt"
        salt.write_bytes(r1.SALT)
        salt.chmod(0o600)
        pw = cls.tmp / "password"
        pw.write_text(PASSWORD + "\n")
        pw.chmod(0o600)
        # The declaration and booked comparator, on this host, network and image.
        a = json.loads(json.dumps(mk.archive("L3")).replace(mk.DIGEST, "lodestar@" + r1.IMAGE))
        decl = a["declaration"]
        decl.update(host_identity=HOST, network_identity=NETWORK)
        (cls.round / "declaration.json").write_text(json.dumps(decl))
        comp = a["comparator"]
        end = DAY_START[1] - timedelta(hours=12, minutes=30)
        comp["binding"].update(host_identity=HOST, network_identity=NETWORK, ended_utc=tc.iso(end + timedelta(seconds=40)))
        comp["timeline"].update(window_start_utc=tc.iso(end - timedelta(hours=24)), window_end_utc=tc.iso(end),
                                process_start_utc=tc.iso(end - timedelta(hours=25)))
        (cls.round / "comparator.json").write_text(json.dumps(comp))
        arms = {arm: {"stack": str(cls.tmp / f"stack-{arm.lower()}"), "suffix": "-" + arm.lower(), "port": PORTS[arm],
                      "tunnel_target": "10.0.0.1", "password_file": str(pw)} for arm in PORTS}
        for arm, a_ in arms.items():
            (Path(a_["stack"]) / "secrets").mkdir(parents=True)
            (Path(a_["stack"]) / "secrets" / "wg0.conf").write_text(f"Endpoint = 192.0.2.{PORTS[arm] - 8000}:1337\n")
        cls.cfg_path = cls.tmp / "coordinator.json"  # outside the round's folder, as the run sheet keeps it
        cls.data = cls.tmp / "data-volume"
        cls.data.mkdir()
        cls.secrets = [str(salt), str(pw), str(cls.data), str(cls.cfg_path)] + [a_["stack"] for a_ in arms.values()]
        cls.cfg_path.write_text(json.dumps({
            "schema": 1, "kind": "cr_round1_coordinator", "declaration": "declaration.json",
            "comparator_report": "comparator.json", "salt_file": str(salt), "data_volume": str(cls.data),
            "round_dir": str(cls.round), "image": "lodestar@" + r1.IMAGE, "commit": mk.COMMIT,
            "uplink_interface": "eth0", "ip_echo_url": "https://api.ipify.org", "arms": arms,
            "expected_growth_bytes": {"default": 20 << 30}}))
        cls.host = tc.FakeHost(DAY_START[1])
        (cls.round / "rehearsal").mkdir()
        sample_host(cls.tmp / "fake-host", cls.round / "rehearsal" / "host-samples.jsonl", sorted(PORTS),
                    DAY_START[1] - timedelta(hours=6), DAY_START[1] - timedelta(hours=5))
        cls.codes, cls.reports = {}, {}
        for day in (1, 2):
            cls.run_day(day, salt, pw)
        cls.manifest_out = cls.call("manifest")
        cls.verdict = analyze_cr.gate_round(analyze_cr.load_manifest(cls.round / "manifest.json"))

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    @classmethod
    def call(cls, command, *extra, day=None):
        argv = [command, str(cls.cfg_path)]
        if day is not None:
            argv += ["--day-dir", str(cls.round / "r1-a1" / f"day{day}"), "--attempt", "r1-a1", "--day", str(day)]
        import io
        from contextlib import redirect_stdout
        out = io.StringIO()
        with redirect_stdout(out):
            rc = tc.co.main(argv + list(extra), host=cls.host)
        assert rc == 0, out.getvalue()
        return out.getvalue().splitlines()

    @classmethod
    def run_day(cls, day, salt, pw):
        host = cls.host
        host.t = DAY_START[day] - timedelta(minutes=30)
        placed = LAYOUT[day]
        for arm, label in placed.items():
            host.ip_of_pid[host.pids["gluetun-" + arm.lower()]] = tc.EXIT_IP[label]
        host.leak_out = "ok tunnel_healthy\nok leak_test\n"  # what the stubbed leak-test.sh prints
        for arm in placed:
            cls.call("leak", "--arm", arm, "--phase", "before", day=day)
        cls.call("bring-up", day=day)
        cls.call("check-start", day=day)
        cls.call("collect", day=day)
        day_dir = cls.round / "r1-a1" / f"day{day}"
        state = json.loads((day_dir / "state.json").read_text())
        for arm, label in placed.items():  # as `collect` launches them, with its run IDs
            start = DAY_START[day] + timedelta(minutes=mk.STAGGER[arm])
            # Day 1's B: its final boundary's egress read fails once and is retried in place.
            cls.codes[(day, arm)] = collector_run(day_dir / arm, arm, day, start, tc.EXIT_IP[label], salt, pw,
                                                  state["arms"][arm]["run_id"],
                                                  fail_call=26 if (day, arm) == (1, "B") else None)
        host.t = DAY_START[day] - timedelta(minutes=1)
        cls.call("watch", day=day)
        for arm in placed:
            cls.call("leak", "--arm", arm, "--phase", "after", day=day)
        cls.call("archive", day=day)
        decl = analyze_cr.check_declaration(json.loads((cls.round / "declaration.json").read_text()))
        for arm in placed:
            report = analyze_cr.analyze_segment(day_dir / arm, None, decl)
            cls.reports[(day, arm)] = report
            (day_dir / f"{arm}.report.json").write_text(json.dumps(report))
        # The real sampler, on a fake /proc and /sys, from the bring-up to the shutdown; then
        # host_samples.py's own `assemble`, as the run sheet runs it.
        shutdown = analyze_cr.parse_utc(json.loads((day_dir / "archive.json").read_text())["shutdown_utc"])
        sample_host(cls.tmp / "fake-host", day_dir / "host-samples.jsonl", sorted(placed),
                    DAY_START[day] - timedelta(minutes=1), shutdown)
        assert hs.main(["assemble", "--samples", str(day_dir / "host-samples.jsonl"), "--device", mk.DEVICE,
                        "--rehearsal", str(cls.round / "rehearsal" / "host-samples.jsonl"),
                        "--archive", str(day_dir / "archive.json"), "--out", str(day_dir / "host-samples.json")]) == 0

    def test_every_collector_closed_its_own_window(self):
        self.assertEqual(set(self.codes.values()), {0})

    def test_every_segment_is_valid_against_its_declaration(self):
        for key, report in self.reports.items():
            self.assertEqual(report["verdict"], "valid", (key, [c for c in report["checks"] if not c["ok"]]))
            self.assertEqual(report["window_hours"], 24)
            self.assertFalse(report["rehearsal"])

    def test_the_archive_holds_the_exits_activity_and_leak_tests(self):
        for day in (1, 2):
            archive = json.loads((self.round / "r1-a1" / f"day{day}" / "archive.json").read_text())
            self.assertEqual(set(archive["exits"]), set(LAYOUT[day].values()))
            ends = [self.reports[(day, a)]["timeline"]["window_end_utc"] for a in LAYOUT[day]]
            for arm, info in archive["arms"].items():
                self.assertEqual(info["leak_test"], {"before": "pass", "after": "pass"})
                self.assertGreaterEqual(info["active_through_utc"], max(ends))

    def test_the_archive_holds_no_address_salt_or_machine_id(self):
        """Runner, 2026-11-27: every file the round's archive keeps, the collectors' evidence
        included, is free of raw addresses, the salt and the machine ID."""
        secrets = [PUBLIC_IP, r1.SALT.decode(), r1.MACHINE_ID, PASSWORD] + list(tc.EXIT_IP.values()) + self.secrets
        secrets += ["10.0.0.1", "192.0.2.91", tc.WG_PRIVATE]  # the gateway, and what gluetun's log names
        for path in self.round.rglob("*"):
            if path.is_file() and path.name != "collector.py":
                text = path.read_text(errors="replace")
                for secret in secrets:
                    self.assertNotIn(secret, text, path)

    def test_gate_round_on_main_passes_the_three_arm_layout_and_ships_b(self):
        """§5.2's fixtures: three-arm layout, both days valid and passing; staggered starts
        and finishes with every peer active through the overhang."""
        for arm in ("B", "A"):
            cand = self.verdict["candidates"][arm]
            self.assertEqual(cand["crawler"], "pass", (arm, [d["reasons"] for d in cand["days"]]))
        self.assertEqual(self.verdict["selection"]["ships"], "B")
        for day in (1, 2):  # A ran Aggressive, as declared (its preset reached the container)
            check = [c for c in self.reports[(day, "A")]["checks"] if c["check"] == "declared_preset"][0]
            self.assertTrue(check["ok"])
        self.assertIn("C", self.verdict["diagnostics"])

    def test_a_retried_final_egress_reading_still_binds_the_window(self):
        rows = [json.loads(x) for x in (self.round / "r1-a1" / "day1" / "B" / "samples.jsonl").read_text().splitlines()]
        final = [r for r in rows if 24 in (r.get("boundary") or [])][0]
        self.assertEqual(final["egress"], {"read": "failed"})
        self.assertEqual(final["egress_retries"][-1]["read"], "ok")
        day1 = [d for d in self.verdict["candidates"]["B"]["days"] if d["day"] == 1][0]
        self.assertEqual(day1["status"], "pass", day1["reasons"])

    def test_the_manifest_lists_each_arms_redacted_gluetun_log(self):
        """X-cr-round1-spec-fixes item 6: the handshake evidence, by hash, for every arm and day."""
        man = json.loads((self.round / "manifest.json").read_text())
        for d in man["days"]:
            self.assertEqual(sorted(d["gluetun_logs"]), sorted(LAYOUT[d["day"]]))
            archive = json.loads((self.round / d["archive"]["path"]).read_text())
            for arm, ref in d["gluetun_logs"].items():
                self.assertEqual(ref["sha256"], tc.co.sha256(self.round / ref["path"]))
                self.assertEqual(archive["arms"][arm]["gluetun_log_sha256"], ref["sha256"])
                self.assertTrue(archive["arms"][arm]["tunnel_target_is_gateway"])
                self.assertIn("[ipv4]", (self.round / ref["path"]).read_text())

    def test_the_verdict_reproduces(self):
        again = analyze_cr.gate_round(analyze_cr.load_manifest(self.round / "manifest.json"))
        self.assertEqual(json.dumps(again, sort_keys=True), json.dumps(self.verdict, sort_keys=True))

    def test_a_peer_that_exits_in_the_overhang_leaves_the_later_candidate_not_judged(self):
        """§5.2's fixture: B stops crawling after its own final boundary, before A's. A's day
        is not judged; B's own day still stands, since A and C ran through B's window."""
        man = json.loads((self.round / "manifest.json").read_text())
        day_dir = self.round / "r1-a1" / "day1"
        archive = json.loads((day_dir / "archive.json").read_text())
        b_end = analyze_cr.parse_utc(self.reports[(1, "B")]["timeline"]["window_end_utc"])
        archive["arms"]["B"]["active_through_utc"] = tc.iso(b_end + timedelta(seconds=30))
        alt = self.round / "alt"
        alt.mkdir(exist_ok=True)
        (alt / "archive.json").write_text(json.dumps(archive))
        for d in man["days"]:
            if d["day"] == 1:
                d["archive"] = {"path": "alt/archive.json", "sha256": tc.co.sha256(alt / "archive.json")}
        (self.round / "manifest-alt.json").write_text(json.dumps(man))
        verdict = analyze_cr.gate_round(analyze_cr.load_manifest(self.round / "manifest-alt.json"))
        a_day1 = [d for d in verdict["candidates"]["A"]["days"] if d["day"] == 1][0]
        self.assertEqual(a_day1["status"], "not_judged")
        self.assertIn("peer_not_active_through_candidate_window", a_day1["reasons"])
        self.assertEqual(verdict["candidates"]["A"]["crawler"], "not_judged")
        b_day1 = [d for d in verdict["candidates"]["B"]["days"] if d["day"] == 1][0]
        self.assertEqual(b_day1["status"], "pass")


if __name__ == "__main__":
    unittest.main()
