"""X-cr-round1-collect: collect.py's round-1 arm (docs/plan/2026-11-27-revised-path-to-100.md
§5.2 items 1, 3, 4 and 11; the schema in docs/decisions/2026-11-27-X-cr-round1-gate.md).

Virtual clock, fake HTTP server, fake Docker API and fake APIs, as in test_collect_spec.py.
The crawler's metrics are test_gate_round's Round1Sim, which renders every §6.1 series, plus
the §5.1 switch gauge. The egress lookup is a fake: the real one joins the crawler's network
namespace (setns), which needs root, so the namespace step is tested on its own below with a
fake libc.
"""

import json
import os
import sys
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

import analyze_cr  # noqa: E402
import collect  # noqa: E402
import test_gate_round as gr  # noqa: E402
from test_collect import args_for, fixed_volumes, write_cgroup  # noqa: E402
from test_collect_spec import CHRONY, PASSWORD, PUBLIC_IP, FakeAPI, SpecBase, SpecDocker, write_net  # noqa: E402

ALL_ON = dict(gr.mk.ALL_ON, safe_restart=True)
ALL_OFF = dict(gr.mk.ALL_OFF, safe_restart=True)
EXIT_IP = {"B": "203.0.113.21", "A": "203.0.113.22", "C": "203.0.113.23"}
MACHINE_ID = "0123456789abcdef0123456789abcdef"
SALT = b"round-1 test salt, never real"
IMAGE = "sha256:" + "f" * 64  # test_collect's FakeDocker image digest
# Series Round1Sim renders without labels, which the collector's allowlist requires.
RELABEL = {"lodestar_fetch_admission_waits_total": '{kind="arrival"}',
           "lodestar_fetch_admission_wait_cancelled_total": '{reason="evicted"}',
           "lodestar_fetch_admission_refusals_total": '{reason="refused"}'}


def declaration(layout="L3"):
    """test_gate_round's declaration, on the fake Docker image's digest."""
    decl = json.loads(json.dumps(gr.mk.archive(layout)["declaration"]).replace(gr.mk.DIGEST, IMAGE))
    return analyze_cr.check_declaration(decl)


def env_for(switches):
    """A container environment that sets exactly these switches (name: bool)."""
    out = ["LODESTAR_DATA_DIR=/data", "LODESTAR_DHT_WITNESS_DIR=/witness"]
    return out + [f"{collect.SWITCH_ENV[k]}={'true' if v else 'false'}" for k, v in sorted(switches.items())]


def gauge_lines(gauge):
    return [f'lodestar_dht_switch{{switch="{k}"}} {int(v)}' for k, v in sorted(gauge.items())]


def round1_metrics(clock, sim, gauge=ALL_ON, start_offset=2):
    """A Lodestar /metrics body: Round1Sim's series, CR-0's required ones, and the gauge."""
    start = int(clock.base.timestamp()) - start_offset

    def render(t):
        lines = []
        for x in sim.snapshot(t, 1):
            name, _, value = x.partition(" ")
            if name in RELABEL:
                x = name + RELABEL[name] + " " + value
            if not x.startswith("lodestar_start_time_seconds"):
                lines.append(x)
        names = {x.split("{")[0].split(" ")[0] for x in lines}
        for name in collect.REQUIRED:
            if f"lodestar_{name}" not in names:
                value = start if name == "start_time_seconds" else int(t) if name.endswith("_total") else 0
                lines.append(f"lodestar_{name} {value}")
        if gauge is not None:
            lines += gauge_lines(gauge)
        lines.append('lodestar_peer_seen{addr="203.0.113.9:6881"} 1')
        return 200, ("\n".join(lines) + "\n").encode()
    return render


class Round1Base(SpecBase):
    arm, preset = "B", "balanced"

    def salt_file(self):
        path = self.tmp / "salt"
        path.write_bytes(SALT)
        path.chmod(0o600)
        return path

    def round1_args(self, *extra, arm=None, day=1, repeat=None, hours=2):
        arm = arm or self.arm
        argv = ["--round", "1", "--attempt", "r1-a1", "--day", str(day), "--arm", arm,
                "--identity-salt-file", str(self.salt_file()), "--run-id", f"r1-a1-d{day}-{arm}"]
        if repeat is not None:
            argv += ["--repeat", str(repeat)]
        return argv + list(extra)

    def round1_run(self, *extra, sim=None, gauge=ALL_ON, env=None, egress=None, api=None, hours=2,
                   hour_seconds=300, warmup=2, intensity=None, host_id=lambda: MACHINE_ID, **kw):
        write_cgroup(self.cgroup)
        write_net(self.tmp / "proc")
        self.sim = sim or gr.Round1Sim(arm=self.arm, preset=self.preset)
        self.lodestar.metrics = round1_metrics(self.clock, self.sim, gauge)
        docker = SpecDocker(env=env_for({"safe_restart": True}) if env is None else env)
        self.fake_api = api or FakeAPI(intensity=intensity or self.preset)
        instance = collect.DockerInstance(docker, "lodestar", self.cgroup, clock=self.clock.clock,
                                          sleep=self.clock.sleep, proc_root=self.tmp / "proc")
        argv = self.spec_args("comb", *self.round1_args(*extra, **kw))
        args = args_for(self.root, "--url", self.lodestar.url, *argv, intensity=intensity or self.preset)
        args.warmup_minutes, args.baseline_hours, args.hour_seconds = warmup, hours, hour_seconds
        lookups = []

        def lookup(pid):
            lookups.append(pid)
            return (egress or (lambda: EXIT_IP[self.arm]))()
        self.egress_lookups = lookups
        c = collect.Collector(args, instance, clock=self.clock.clock, sleep=self.clock.sleep, now=self.clock.now,
                              volumes=fixed_volumes(), ip_lookup=lambda url: PUBLIC_IP, api_call=self.fake_api,
                              run_command=lambda argv: CHRONY if argv[0] == "chronyc" else None,
                              egress_lookup=lookup, host_id=host_id)
        return c, c.run()

    def judge(self, decl=None):
        return analyze_cr.analyze_segment(self.root, None, decl or declaration())

    def evidence_text(self):
        return "\n".join(p.read_text(errors="replace") for p in self.root.rglob("*") if p.is_file()
                         and p.name != "collector.py")


class EffectiveSettingsTests(Round1Base):
    def test_all_on_by_compiled_default_is_recorded_on_and_valid(self):
        """§5.2's fixture: no switch in the environment but safe_restart; the gauge says on."""
        c, rc = self.round1_run()
        self.assertEqual(rc, 0, self.result()["reason"])
        config = self.run_doc()["config"]
        self.assertEqual(config["effective_switches"], ALL_ON)
        self.assertEqual(config["switches"], ALL_ON)
        self.assertEqual(config["switch_sources"]["utp"], "default")
        self.assertEqual(config["switch_sources"]["safe_restart"], "env")
        self.assertEqual(config["switch_env"]["utp"], "unset")
        self.assertEqual(config["switch_env"]["safe_restart"], "true")
        report = self.judge()
        self.assertEqual(report["verdict"], "valid", [x for x in report["checks"] if not x["ok"]])
        self.assertTrue([x for x in report["checks"] if x["check"] == "declared_switches"][0]["ok"])

    def test_explicit_all_off_is_recorded_off_and_valid(self):
        self.arm = "C"
        sim = gr.Round1Sim(arm="C", switches=gr.mk.ALL_OFF)
        c, rc = self.round1_run(sim=sim, gauge=ALL_OFF, env=env_for(ALL_OFF))
        self.assertEqual(rc, 0, self.result()["reason"])
        config = self.run_doc()["config"]
        self.assertEqual(config["switches"], ALL_OFF)
        self.assertEqual(set(config["switch_sources"].values()), {"env"})
        self.assertEqual(config["switch_env"]["utp"], "false")
        report = self.judge()
        self.assertEqual(report["verdict"], "valid", [x for x in report["checks"] if not x["ok"]])

    def test_an_environment_that_disagrees_with_the_gauge_records_the_gauge(self):
        """The gauge is the effective value; the environment's value sits beside it."""
        env = env_for({"safe_restart": True, "utp": False})
        gauge = dict(ALL_ON, utp=True)
        self.round1_run(env=env, gauge=gauge)
        config = self.run_doc()["config"]
        self.assertTrue(config["switches"]["utp"])
        self.assertEqual((config["switch_env"]["utp"], config["switch_sources"]["utp"]), ("false", "env"))

    def test_without_the_gauge_an_unset_switch_is_unknown_not_off(self):
        self.round1_run(gauge=None)
        config = self.run_doc()["config"]
        self.assertNotIn("effective_switches", config)
        self.assertIsNone(config["switches"]["utp"])
        self.assertEqual(config["switch_sources"]["utp"], "unset")
        self.assertTrue(config["switches"]["safe_restart"])
        self.assertIn("switch gauge", (self.root / "events.log").read_text())
        report = self.judge()
        self.assertEqual(report["verdict"], "invalid")
        failed = {x["check"] for x in report["checks"] if not x["ok"]}
        self.assertTrue(failed & {"configuration_identity_recorded", "effective_switches_recorded"}, failed)

    def test_a_partial_or_malformed_gauge_is_unknown(self):
        self.assertIsNone(collect.switch_gauge("\n".join(gauge_lines(ALL_ON)[:-1])))
        bad = gauge_lines(ALL_ON) + ['lodestar_dht_switch{switch="utp"} 1']
        self.assertIsNone(collect.switch_gauge("\n".join(bad)))
        two = [x.replace(" 1", " 2") for x in gauge_lines(ALL_ON)]
        self.assertIsNone(collect.switch_gauge("\n".join(two)))
        self.assertEqual(collect.switch_gauge("\n".join(gauge_lines(ALL_ON))), ALL_ON)

    def test_boundaries_carry_the_gauge_and_snapshots_keep_the_bundle_series(self):
        self.round1_run()
        marks = [r for r in self.rows() if r.get("boundary")]
        self.assertTrue(marks)
        self.assertTrue(all(r["api"]["switches"] == ALL_ON for r in marks))
        prom = (self.root / "metrics" / "00003.prom").read_text()
        for line in ('dht_switch{switch="utp"} 1', "dht_hints_captured_total", "fetch_stream_batches_total",
                     'dht_target_prefix_queries_total{kind="sample"}',
                     'dht_sample_cooling_reason{reason="first_failure"}'):
            self.assertIn("lodestar_" + line, prom)

    def test_old_runs_keep_config_identitys_meaning(self):
        """§8 item 8: without --round, an unset switch is still recorded false from `default`."""
        identity = collect.config_identity({"LODESTAR_DHT_UTP": "true"})
        self.assertFalse(identity["switches"]["lookup_end"])
        self.assertEqual(identity["switch_sources"]["lookup_end"], "default")
        self.assertNotIn("switch_env", identity)


class PlacementAndIdentityTests(Round1Base):
    def test_run_json_carries_the_binding_fields(self):
        self.round1_run(repeat=1)
        run = self.run_doc()
        self.assertEqual(run["run_id"], "r1-a1-d1-B")
        self.assertEqual(run["round1"], {"round": 1, "attempt": "r1-a1", "day": 1, "arm": "B", "repeat": 1})
        self.assertEqual(run["host_identity"], collect.salted_hash(SALT, "host", MACHINE_ID))
        self.assertEqual(run["network_identity"], {"uplink_hash": collect.salted_hash(SALT, "ip", PUBLIC_IP),
                                                   "provider": "pia", "protocol": "wireguard"})
        self.assertRegex(run["host_identity"], analyze_cr.HASH.pattern)
        self.assertTrue(run["settings"]["round1"]["coordinated_shutdown"])

    def test_a_derived_run_id_is_unique_and_well_formed(self):
        args = args_for(self.root, "--run-kind", "comb", "--round", "1", "--attempt", "r1-a1", "--day", "2",
                        "--arm", "A")
        collect.resolve_defaults(args)
        one, two = collect.parse_round1(args)[1], collect.parse_round1(args)[1]
        self.assertNotEqual(one, two)
        self.assertRegex(one, r"^r1-r1-a1-d2-A-[0-9a-f]{8}$")

    def test_evidence_holds_no_address_salt_or_machine_id(self):
        self.round1_run()
        text = self.evidence_text()
        for secret in (PUBLIC_IP, EXIT_IP["B"], SALT.decode(), MACHINE_ID, PASSWORD, "203.0.113.9"):
            self.assertNotIn(secret, text)

    def test_egress_is_hashed_at_bring_up_and_every_boundary(self):
        self.round1_run()
        rows = self.rows()
        want = collect.salted_hash(SALT, "ip", EXIT_IP["B"])
        self.assertEqual(rows[0]["egress"], {"read": "ok", "hash": want})
        marks = [r for r in rows if r.get("boundary")]
        self.assertEqual(sum(len(r["boundary"]) for r in marks), 3)
        self.assertTrue(all(r["egress"] == {"read": "ok", "hash": want} for r in marks))
        self.assertTrue(all(pid == 4242 for pid in self.egress_lookups))  # the crawler's own process

    def test_a_failed_egress_read_is_retried_within_the_boundary_tolerance(self):
        calls = []

        def flaky():
            calls.append(1)
            if len(calls) == 2:  # the first boundary's read
                raise OSError("tunnel busy")
            return EXIT_IP["B"]
        self.round1_run(egress=flaky)
        rows = self.rows()
        first = next(i for i, r in enumerate(rows) if r.get("boundary"))
        self.assertEqual(rows[first]["egress"], {"read": "failed"})
        self.assertEqual(rows[first + 1]["egress"]["read"], "ok")
        self.assertNotIn("boundary", rows[first + 1])

    def test_the_uplink_and_the_egress_hash_alike_when_the_tunnel_is_bypassed(self):
        """Both are kind `ip`: a crawler leaving by the uplink shows the uplink's hash."""
        self.round1_run(egress=lambda: PUBLIC_IP)
        run = self.run_doc()
        self.assertEqual(self.rows()[0]["egress"]["hash"], run["network_identity"]["uplink_hash"])


class StartAndBoundsTests(Round1Base):
    def test_cr0w_first_start_opens_within_minutes(self):
        """§5.2's fixture: a first start on a CR-0w build samples at once (reason first_start)."""
        sim = gr.Round1Sim(reason="first_start")
        self.round1_run(sim=sim)
        rows = self.rows()
        first = next(r for r in rows if r.get("boundary"))
        self.assertLessEqual(first["elapsed_seconds"], 2 * 60 + 70)  # T_open at the first scrape, then the warm-up

    def test_cr0w_with_safe_restart_off_opens_within_minutes_too(self):
        """The 6 h rule for a zero beside `off` was for pre-CR-0w builds; the gauge shows off."""
        sim = gr.Round1Sim(reason="off")
        gauge = dict(ALL_ON, safe_restart=False)
        self.round1_run(sim=sim, gauge=gauge, env=env_for({"safe_restart": False}))
        first = next(r for r in self.rows() if r.get("boundary"))
        self.assertLess(first["elapsed_seconds"], 10 * 60)

    def test_open_eligible_keeps_its_old_reading_without_the_gauge(self):
        zero_off = {"dht_sample_suppression_seconds": 0, "suppression_reason": "off"}
        self.assertFalse(collect.open_eligible(zero_off, 120))
        self.assertFalse(collect.open_eligible(zero_off, 120, True))
        self.assertTrue(collect.open_eligible(zero_off, 120, False))
        self.assertFalse(collect.open_eligible(zero_off, 30, False))
        self.assertTrue(collect.open_eligible(dict(zero_off, suppression_reason="first_start"), 5, True))

    def test_an_aggressive_arm_runs_above_balanceds_outstanding_bound(self):
        self.arm, self.preset = "A", "aggressive"
        sim = gr.Round1Sim(arm="A", preset="aggressive")
        sim.outstanding = lambda e: 3000
        c, rc = self.round1_run(sim=sim)
        self.assertEqual(rc, 0, self.result()["reason"])
        self.assertEqual(self.run_doc()["settings"]["round1"]["bounds"], {"peer_requests": 256, "outstanding": 4096})

    def test_a_balanced_arm_still_stops_over_its_own_bound(self):
        sim = gr.Round1Sim()
        sim.outstanding = lambda e: 1500
        c, rc = self.round1_run(sim=sim)
        self.assertEqual(self.result()["reason"], "three_consecutive_capacity_violations")

    def test_preset_slots_raise_the_peer_request_bound(self):
        """With dht.preset_slots on, Balanced's 80 slots: 60 running is in bounds."""
        sim = gr.Round1Sim()
        base = sim.snapshot
        sim.snapshot = lambda e, row: [x if not x.startswith("lodestar_dht_peer_active ") else "lodestar_dht_peer_active 60"
                                       for x in base(e, row)]
        c, rc = self.round1_run(sim=sim)
        self.assertEqual(rc, 0, self.result()["reason"])
        off = dict(ALL_ON, preset_slots=False)
        self.root = self.tmp / "run2"
        self.clock.t = 0.0
        c, rc = self.round1_run(sim=sim, gauge=off)
        self.assertEqual(self.result()["reason"], "three_consecutive_capacity_violations")

    def test_old_runs_keep_the_fixed_bounds(self):
        c = collect.Collector.__new__(collect.Collector)
        c.args = args_for(self.root, intensity="aggressive")
        self.assertEqual(c.round1_bounds(None), {"peer_requests": 256, "outstanding": 4096})


class LaunchTests(Round1Base):
    def refused(self, *extra, **kw):
        with self.assertRaises(SystemExit) as caught:
            self.round1_run(*extra, **kw)
        return str(caught.exception)

    def test_round1_refuses_what_it_cant_record(self):
        self.assertIn("without --stop", self.refused("--stop"))
        self.assertIn("machine ID", self.refused(host_id=lambda: None))

    def test_the_salt_must_be_private(self):
        path = self.salt_file()
        real = self.salt_file
        self.salt_file = lambda: (path.chmod(0o644), path)[1]
        self.assertIn("owner only", self.refused())
        self.salt_file = lambda: (path.write_bytes(b"short"), path.chmod(0o600), path)[2]
        self.assertIn("at least", self.refused())
        self.salt_file = real

    def test_round1_options_need_round(self):
        args = args_for(self.root, "--run-kind", "comb", "--arm", "B")
        collect.resolve_defaults(args)
        with self.assertRaises(SystemExit):
            collect.parse_round1(args)
        for bad in (["--day", "1", "--arm", "b"], ["--day", "1", "--arm", "B", "--attempt", "a b"]):
            args = args_for(self.root, "--run-kind", "comb", "--round", "1", "--attempt", "r1-a1", *bad)
            collect.resolve_defaults(args)
            if "--attempt" in bad[2:]:
                args.attempt = "a b"
            with self.assertRaises(SystemExit):
                collect.parse_round1(args)

    def test_an_unreadable_uplink_refuses_the_launch(self):
        write_cgroup(self.cgroup)
        write_net(self.tmp / "proc")
        self.lodestar.metrics = round1_metrics(self.clock, gr.Round1Sim())
        instance = collect.DockerInstance(SpecDocker(env=env_for({"safe_restart": True})), "lodestar", self.cgroup,
                                          clock=self.clock.clock, sleep=self.clock.sleep, proc_root=self.tmp / "proc")
        args = args_for(self.root, "--url", self.lodestar.url, *self.spec_args("comb", *self.round1_args()))

        def down(url):
            raise OSError("no route")
        c = collect.Collector(args, instance, clock=self.clock.clock, sleep=self.clock.sleep, now=self.clock.now,
                              volumes=fixed_volumes(), ip_lookup=down, api_call=FakeAPI(),
                              run_command=lambda argv: None, egress_lookup=lambda pid: EXIT_IP["B"],
                              host_id=lambda: MACHINE_ID)
        with self.assertRaises(SystemExit) as caught:
            c.run()
        self.assertIn("uplink", str(caught.exception))


class NamespaceTests(unittest.TestCase):
    """in_netns joins the namespace in its own thread only; the caller's thread never moves."""

    def test_setns_runs_in_a_throwaway_thread_and_errors_come_back(self):
        import threading
        seen = []

        class Libc:
            def setns(self, fd, kind):
                seen.append((threading.current_thread().name, kind))
                return 0
        with mock.patch.object(collect.ctypes, "CDLL", lambda *a, **k: Libc()), \
                mock.patch.object(collect.os, "open", lambda *a: 99), mock.patch.object(collect.os, "close", lambda fd: None):
            self.assertEqual(collect.in_netns("/proc/1/ns/net", lambda: "ok"), "ok")
            with self.assertRaises(ValueError):
                collect.in_netns("/proc/1/ns/net", lambda: (_ for _ in ()).throw(ValueError("x")))
        self.assertEqual(seen[0], ("netns-lookup", collect.CLONE_NEWNET))

    def test_a_refused_setns_is_an_error_never_a_reading(self):
        class Libc:
            def setns(self, fd, kind):
                return -1
        with mock.patch.object(collect.ctypes, "CDLL", lambda *a, **k: Libc()), \
                mock.patch.object(collect.os, "open", lambda *a: 99), mock.patch.object(collect.os, "close", lambda fd: None):
            with self.assertRaises(OSError):
                collect.in_netns("/proc/1/ns/net", lambda: "never")
        egress = collect.Egress(SALT, lambda pid: collect.in_netns("/nonexistent/ns/net", lambda: "x"))
        self.assertEqual(egress.check(1), {"read": "failed"})

    @unittest.skipUnless(os.geteuid() == 0 and Path("/proc/self/ns/net").exists(), "needs root")
    def test_joining_our_own_namespace_works_as_root(self):
        self.assertEqual(collect.in_netns("/proc/self/ns/net", lambda: 7), 7)


if __name__ == "__main__":
    unittest.main()
