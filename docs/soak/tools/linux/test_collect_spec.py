"""CR-COL-3 tests for collect.py's CR-SPEC run kinds (ab, comb, comparator).

Virtual clock, fake HTTP server, fake Docker API, fake /proc and fake APIs, as in
test_collect.py. Each run is then judged by ../analyze_cr.py, so the collector and
the analyzer are tested against each other.
"""

import json
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

import analyze_cr  # noqa: E402
import collect  # noqa: E402
import test_analyze_cr  # noqa: E402
from test_collect import (COMMIT, CONTAINER_ID, SECRETS, Base, FakeDocker,  # noqa: E402
                          args_for, fixed_volumes, write_cgroup)

PUBLIC_IP = "198.51.100.7"
PASSWORD = "rehearsal-password-1"
COOKIE = "lodestar_session=1796187443.8907f1dc60c36216df36d03130d7806c"
CHRONY = "Reference ID    : C0A80001\nSystem time     : 0.000012345 seconds fast of NTP time\nLeap status     : Normal\n"


class SpecDocker(FakeDocker):
    """The fake Docker API with the Lodestar image's environment, mounts and exec."""

    def __init__(self, env=None, mounts=None, count=lambda: 0, **kw):
        super().__init__(**kw)
        self.container["Config"]["Env"] = env if env is not None else [
            "LODESTAR_DATA_DIR=/data", "LODESTAR_DHT_WITNESS_DIR=/witness", "LODESTAR_DHT_SAFE_RESTART=true",
            "LODESTAR_DHT_UTP=true", "LODESTAR_SETUP_TOKEN=s3cr3t-token"]
        self.container["Mounts"] = mounts if mounts is not None else [
            {"Destination": "/data", "Type": "volume", "Source": "/mnt/user/appdata/lodestar"},
            {"Destination": "/witness", "Type": "bind", "Source": "/mnt/user/appdata/witness"}]
        self.count, self.execs = count, []

    def get(self, path):
        if path.startswith("/exec/"):
            return 200, {"ExitCode": 0 if self.count() is not None else 1}
        return super().get(path)

    def post_json(self, path, body, timeout=None):
        self.calls.append(("POST", path))
        self.execs.append(body)
        return 201, {"Id": "e1"}

    def post_raw(self, path, body, timeout=None):
        self.calls.append(("POST", path))
        value = self.count()
        payload = f"{value if value is not None else 'ERROR'}\n".encode()
        return 200, bytes([1, 0, 0, 0]) + len(payload).to_bytes(4, "big") + payload


class FakeAPI:
    """The settings and Insights APIs behind a login."""

    def __init__(self, intensity="balanced", imports=0):
        self.intensity, self.imports, self.calls, self.expire = intensity, imports, [], False

    def __call__(self, method, url, body, cookie):
        self.calls.append((method, url.split("/api/v1", 1)[1], cookie))
        if url.endswith("/login"):
            ok = body == {"password": PASSWORD}
            return (200, {"set-cookie": COOKIE + "; HttpOnly; Path=/"}, b'{"ok":true}') if ok else (401, {}, b"")
        if cookie != COOKIE or self.expire:
            self.expire = False
            return 401, {}, b""
        if "/settings/crawler" in url:
            return 200, {}, json.dumps({"fields": {
                "intensity": {"value": self.intensity, "source": "default"},
                "paused": {"value": False, "source": "default"}}, "restart_required": []}).encode()
        if "/insights/events" in url:
            notes = [{"at": 1, "kind": "import_finished", "text": "", "import": {"job": 3, "source": "magnetico"}}] \
                * self.imports
            return 200, {}, json.dumps({"annotations": [{"kind": "restart"}] + notes, "paused": [],
                                        "truncated": False}).encode()
        return 404, {}, b""


def gate_metrics(clock, first_off=False, utp=True):
    """A Lodestar /metrics body with CR-0's required series and the gate series."""
    sim = test_analyze_cr.Sim(switches={"utp": utp})
    start = int(clock.base.timestamp()) - 2

    def render(t):
        sim.first_off = first_off and t < 10  # the DHT's pre-report default, at the first scrape only
        lines = [l for l in sim.snapshot(t, 0) if not l.startswith("lodestar_start_time_seconds")]
        names = {l.split("{")[0].split(" ")[0] for l in lines}
        for name in collect.REQUIRED:
            if f"lodestar_{name}" not in names:
                value = start if name == "start_time_seconds" else int(t) if name.endswith("_total") else 0
                lines.append(f"lodestar_{name} {value}")
        lines += ['lodestar_peer_seen{addr="203.0.113.9:6881"} 1',
                  'lodestar_torrent_info{name="Ubuntu 24.04 ISO"} 1']
        return 200, ("\n".join(lines) + "\n").encode()
    return render


def write_net(root, pid=4242):
    net = Path(root) / str(pid) / "net"
    net.mkdir(parents=True, exist_ok=True)
    (net / "dev").write_text(
        "Inter-|   Receive                            |  Transmit\n"
        " face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n"
        "    lo: 999 9 0 0 0 0 0 0 999 9 0 0 0 0 0 0\n"
        "  eth0: 5000 50 0 0 0 0 0 0 4000 40 0 0 0 0 0 0\n")
    (net / "sockstat").write_text("sockets: used 37\nTCP: inuse 12 orphan 0 tw 1 alloc 14 mem 2\nUDP: inuse 3 mem 1\n")


class SpecBase(Base):
    def spec_args(self, kind="ab", *extra, **kw):
        password = self.tmp / "password"
        password.write_text(PASSWORD + "\n")
        argv = ["--run-kind", kind, "--warmup-minutes", "2", "--baseline-hours", "2", "--hour-seconds", "300",
                "--password-file", str(password), "--spec-commit", COMMIT, "--ip-echo-url", "http://127.0.0.1:9/",
                "--checklist", "auto_update=excluded", "--next-run", "auto_update=2026-11-10T03:00:00Z",
                "--checklist", "backup=excluded", "--next-run", "backup=2026-11-09T03:00:00Z"]
        if kind == "ab":
            argv += ["--treatment", kw.pop("treatment", "utp")]
        return argv + list(extra)

    def spec_run(self, kind="ab", *extra, docker=None, api=None, ip=None, first_off=False, utp=True, **kw):
        write_cgroup(self.cgroup)
        write_net(self.tmp / "proc")
        self.api = docker or SpecDocker()
        self.fake_api = api or FakeAPI()
        self.lodestar.metrics = gate_metrics(self.clock, first_off=first_off, utp=utp)
        instance = collect.DockerInstance(self.api, "lodestar", self.cgroup, clock=self.clock.clock,
                                          sleep=self.clock.sleep, proc_root=self.tmp / "proc")
        args = args_for(self.root, "--url", self.lodestar.url, *self.spec_args(kind, *extra, **kw))
        c = collect.Collector(args, instance, clock=self.clock.clock, sleep=self.clock.sleep, now=self.clock.now,
                              volumes=fixed_volumes(), ip_lookup=ip or (lambda url: PUBLIC_IP),
                              api_call=self.fake_api, run_command=lambda argv: CHRONY if argv[0] == "chronyc" else None)
        return c, c.run()

    def run_doc(self):
        return json.loads((self.root / "run.json").read_text())


class SpecRunTests(SpecBase):
    def test_ab_run_records_section_8_and_the_analyzer_finds_it_valid(self):
        c, rc = self.spec_run()
        self.assertEqual(rc, 0, self.result()["reason"])
        run = self.run_doc()
        self.assertEqual(run["settings"]["run_kind"], "ab")
        self.assertEqual(run["settings"]["treatment"], "utp")
        config = run["config"]
        self.assertTrue(config["switches"]["utp"] and config["switches"]["safe_restart"])
        self.assertEqual(config["switch_sources"]["coordinated_admission"], "default")
        self.assertEqual(config["witness"], {"configured": True, "host_mounted": True, "outside_data_dir": True})
        self.assertEqual(config["settings_api"]["intensity"], "balanced")
        self.assertEqual(run["clock"]["ntp_start"], {"source": "chronyc", "synchronized": True,
                                                     "offset_seconds": 0.000012345})
        self.assertEqual(run["checklist"]["backup"]["next_run_utc"], "2026-11-09T03:00:00Z")
        rows = self.rows()
        self.assertEqual(rows[0]["public_ip"], "reference")
        marks = [r for r in rows if r.get("boundary")]
        self.assertEqual(sum(len(r["boundary"]) for r in marks), 3)  # t_s, one hour boundary, the end
        self.assertTrue(all(r["public_ip"] == "unchanged" and "imports" in r["api"] for r in marks))
        self.assertEqual(rows[0]["net"], {"rx_bytes": 5000, "tx_bytes": 4000, "rx_packets": 50, "tx_packets": 40,
                                          "sockets_used": 37, "tcp_inuse": 12, "udp_inuse": 3})
        self.assertEqual(rows[0]["suppression_reason"], "resumed")
        prom = (self.root / "metrics" / "00003.prom").read_text()
        for line in ('discovery_to_searchable_seconds_bucket{le="+Inf"}', 'searchable_new_total{origin="network"}',
                     "discovery_to_searchable_dropped_total", 'fetch_family_attempts_total{family="ipv6"}',
                     'fetch_attempts_total{transport="utp"}', "dht_utp_in_total", "utp_retransmits_total"):
            self.assertIn("lodestar_" + line, prom)
        report = analyze_cr.analyze_segment(self.root)
        self.assertEqual(report["verdict"], "valid", [c for c in report["checks"] if not c["ok"]])
        self.assertEqual(report["window_hours"], 2)

    def test_status_shows_no_counts_while_the_window_is_open(self):
        self.spec_run()
        status_rows = []
        original = collect.save_json

        def spy(value, path):
            if path.name == "status.json" and value.get("state") == "running":
                status_rows.append(value["latest"])
            return original(value, path)
        self.root = self.tmp / "run2"
        self.clock.t = 0
        with unittest.mock.patch.object(collect, "save_json", spy):
            self.spec_run()
        self.assertTrue(status_rows)
        for latest in status_rows:
            self.assertNotIn("values", latest)
            self.assertNotIn("net", latest)
            self.assertIn("window", latest)

    def test_evidence_keeps_no_address_password_cookie_or_path(self):
        self.spec_run()
        for path in self.root.rglob("*"):
            if not path.is_file() or path.name == "collector.py":
                continue
            text = path.read_text()
            for secret in SECRETS + (PUBLIC_IP, PASSWORD, COOKIE.split("=")[1], "/witness", "/data",
                                     "/mnt/user", "magnetico", "LODESTAR_SETUP_TOKEN"):
                self.assertNotIn(secret, text, f"{secret!r} leaked into {path.name}")

    def test_pre_report_default_does_not_open_the_window_early(self):
        c, rc = self.spec_run(first_off=True)
        self.assertEqual(rc, 0)
        rows = self.rows()
        self.assertEqual(rows[0]["suppression_reason"], "off")
        self.assertNotIn("boundary", rows[0])
        report = analyze_cr.analyze_segment(self.root)
        self.assertEqual(report["verdict"], "valid", [c for c in report["checks"] if not c["ok"]])
        self.assertEqual(report["timeline"]["t_open_elapsed"], rows[1]["elapsed_seconds"])

    def test_imports_and_a_changed_public_ip_reach_the_verdict(self):
        addresses = iter([PUBLIC_IP] * 2 + ["192.0.2.1"] * 100)
        self.spec_run(api=FakeAPI(imports=1), ip=lambda url: next(addresses))
        report = analyze_cr.analyze_segment(self.root)
        failed = {c["check"] for c in report["checks"] if not c["ok"]}
        self.assertIn("no_import_job", failed)
        self.assertIn("public_ip_unchanged", failed)
        self.assertNotIn("192.0.2.1", (self.root / "samples.jsonl").read_text())

    def test_a_failed_ip_check_is_retried_within_the_boundary_tolerance(self):
        answers = iter([PUBLIC_IP, PUBLIC_IP, OSError("down")] + [PUBLIC_IP] * 100)

        def lookup(url):
            answer = next(answers)
            if isinstance(answer, Exception):
                raise answer
            return answer
        self.spec_run(ip=lookup, **{})
        rows = self.rows()
        statuses = [r.get("public_ip") for r in rows if "public_ip" in r]
        self.assertIn("failed", statuses)
        failed_at = next(i for i, r in enumerate(rows) if r.get("public_ip") == "failed")
        self.assertEqual(rows[failed_at + 1]["public_ip"], "unchanged")

    def test_settings_session_is_renewed_after_a_401(self):
        api = FakeAPI()
        self.spec_run(api=api)
        logins = sum(1 for m, path, _ in api.calls if path == "/login")
        self.assertEqual(logins, 1)
        api.expire = True
        session = collect.LodestarAPI(self.lodestar.url, self.tmp / "password", api)
        session.cookie = COOKIE
        self.assertEqual(session.settings()["intensity"], "balanced")
        self.assertEqual([p for _, p, _ in api.calls[-3:]], ["/settings/crawler", "/login", "/settings/crawler"])

    def test_comparator_counts_by_a_read_only_query(self):
        counter = {"n": -1}

        def count():
            counter["n"] += 1
            return counter["n"] * 40
        docker = SpecDocker(env=[], mounts=[], count=count)
        c, rc = self.spec_run("comparator", "--postgres-container", "bitmagnet-postgres", docker=docker)
        self.assertEqual(rc, 0, self.result()["reason"])
        body = docker.execs[0]
        self.assertEqual(body["Cmd"][-1], collect.COMPARATOR_QUERY)
        self.assertIn("default_transaction_read_only=on", body["Env"][0])
        run = self.run_doc()
        self.assertEqual(run["comparator"]["query"], collect.COMPARATOR_QUERY)
        self.assertNotIn("config", run)
        report = analyze_cr.analyze_segment(self.root)
        self.assertEqual(report["verdict"], "valid", [c for c in report["checks"] if not c["ok"]])
        self.assertGreater(report["numerator"]["window"]["mean_per_min"], 0)

    def test_note_command_stamps_utc_and_elapsed(self):
        self.spec_run()
        origin = self.run_doc()["clock"]["monotonic_origin"]
        boot = self.tmp / "boot_id"
        boot.write_text((self.run_doc()["clock"]["boot_id"] or "") + "\n")
        rc = collect.note_command(["--root", str(self.root), "--category", "array_operation", "--text", "parity"],
                                  now=self.clock.now, clock=lambda: origin + 1234.56, boot_id_path=boot)
        self.assertEqual(rc, 0)
        entry = json.loads((self.root / "activity.jsonl").read_text())
        self.assertEqual(entry["category"], "array_operation")
        self.assertEqual(entry["elapsed_seconds"], 1234.6)
        self.assertTrue(entry["collector_finished"])
        with self.assertRaises(SystemExit):
            collect.note_command(["--root", str(self.root), "--category", "cosmic_rays"])
        self.assertEqual(collect.note_command(["--root", str(self.tmp), "--category", "import"]), 2)


class SpecLaunchTests(SpecBase):
    def refused(self, kind="ab", *extra, **kw):
        write_cgroup(self.cgroup)
        instance = collect.DockerInstance(SpecDocker(), "lodestar", self.cgroup, clock=self.clock.clock,
                                          sleep=self.clock.sleep)
        args = args_for(self.root, "--url", self.lodestar.url, *self.spec_args(kind, *extra, **kw))
        with self.assertRaises(SystemExit) as caught:
            collect.Collector(args, instance, clock=self.clock.clock, sleep=self.clock.sleep, now=self.clock.now,
                              volumes=fixed_volumes()).prepare()
        return str(caught.exception)

    def test_spec_kinds_refuse_unsafe_or_incomplete_launches(self):
        self.assertIn("--treatment", self.refused("comb", "--treatment", "utp"))
        self.assertIn("checklist", self.refused("ab", "--checklist", "backup=maybe"))
        self.assertIn("--next-run", self.refused("ab", "--next-run", "auto_update=tomorrow"))
        self.assertIn("comparator", self.refused("comparator"))
        self.assertIn("https", self.refused("ab", "--ip-echo-url", "ftp://example.test/"))

    def test_real_windows_are_fixed_outside_rehearsals(self):
        args = collect.resolve_defaults(collect.build_parser().parse_args(
            ["--mode", "docker", "--root", str(self.root), "--commit", COMMIT, "--run-kind", "ab"]))
        self.assertEqual((args.warmup_minutes, args.baseline_hours), (60, 6))
        args = collect.resolve_defaults(collect.build_parser().parse_args(
            ["--mode", "docker", "--root", str(self.root), "--commit", COMMIT, "--run-kind", "comb"]))
        self.assertEqual(args.baseline_hours, 24)
        argv = ["--mode", "docker", "--root", str(self.root), "--commit", COMMIT, "--run-kind", "ab",
                "--treatment", "utp", "--baseline-hours", "5", "--spec-commit", COMMIT,
                "--min-free-bytes", "3", "--warn-free-bytes", "2", "--stop-free-bytes", "1"]
        write_cgroup(self.cgroup)
        instance = collect.DockerInstance(SpecDocker(), "lodestar", self.cgroup)
        c = collect.Collector(collect.build_parser().parse_args(argv), instance, volumes=fixed_volumes())
        with self.assertRaises(SystemExit) as caught:
            c.prepare()
        self.assertIn("rehearsal", str(caught.exception))


class SpecUnitTests(unittest.TestCase):
    def test_sanitizer_validates_gate_labels_and_caps_series(self):
        text = "\n".join([
            'lodestar_discovery_to_searchable_seconds_bucket{le="+Inf"} 4',
            'lodestar_discovery_to_searchable_seconds_bucket{le="0.5"} 4',
            'lodestar_searchable_new_total{origin="203_0_113_9"} 1',
            'lodestar_searchable_new_total{origin="network",peer="x"} 1',
            'lodestar_fetch_admission_grants_total{class="interactive",kind="arrival"} 1',
            'lodestar_fetch_admission_grants_total{kind="arrival",class="interactive"} 2',
            'lodestar_dht_utp_in_total{family="ipv4"} 1',
            "lodestar_dht_utp_in_total 3"]) + "\n"
        kept = collect.sanitize_snapshot(text).splitlines()
        self.assertEqual(kept, ['lodestar_discovery_to_searchable_seconds_bucket{le="+Inf"} 4',
                                'lodestar_fetch_admission_grants_total{kind="arrival",class="interactive"} 2',
                                "lodestar_dht_utp_in_total 3"])
        flags = {}
        many = "\n".join(f'lodestar_fetch_failures_total{{kind="timed_out"}} {i}' for i in range(2)) + "\n" + "\n".join(
            f"lodestar_{name} 1" for name in sorted(collect.KEEP - set(collect.LABELLED)))
        with unittest.mock.patch.object(collect, "MAX_SERIES", 5):
            self.assertEqual(len(collect.sanitize_snapshot(many, flags).splitlines()), 5)
        self.assertEqual(flags, {"series_capped": True})

    def test_open_eligible(self):
        off0 = {"dht_sample_suppression_seconds": 0, "suppression_reason": "off"}
        self.assertFalse(collect.open_eligible(off0, 1))
        self.assertTrue(collect.open_eligible(off0, 6 * 3600))
        self.assertTrue(collect.open_eligible(dict(off0, suppression_reason="resumed"), 1))
        self.assertFalse(collect.open_eligible(dict(off0, dht_sample_suppression_seconds=5), 7 * 3600))

    def test_config_identity(self):
        env = {"LODESTAR_DHT_UTP": "true", "LODESTAR_DHT_ROUTE_BACKOFF": "yes", "LODESTAR_INTENSITY": "balanced",
               "LODESTAR_DHT_WITNESS_DIR": "/data/witness", "LODESTAR_DATA_DIR": "/data"}
        out = collect.config_identity(env, [{"destination": "/data", "type": "bind"}])
        self.assertTrue(out["switches"]["utp"])
        self.assertIsNone(out["switches"]["route_backoff"])  # unparsable: missing identity
        self.assertEqual(out["env_preset"], "balanced")
        self.assertEqual(out["witness"], {"configured": True, "host_mounted": True, "outside_data_dir": False})
        image_default = collect.config_identity({"LODESTAR_DHT_WITNESS_DIR": "/var/lib/lodestar-witness",
                                                 "LODESTAR_DATA_DIR": "/data"}, [{"destination": "/data", "type": "volume"}])
        self.assertFalse(image_default["witness"]["host_mounted"])
        native = collect.config_identity({"LODESTAR_DHT_WITNESS_DIR": "/srv/w", "LODESTAR_DATA_DIR": "/srv/data"})
        self.assertTrue(native["witness"]["outside_data_dir"])

    def test_ntp_sources(self):
        tds = {("timedatectl", "show", "-p", "NTPSynchronized", "--value"): "yes\n",
               ("timedatectl", "timesync-status"): "       Server: 192.0.2.1\n       Offset: -1.250ms\n"}
        out = collect.ntp_status(lambda argv: tds.get(tuple(argv)))
        self.assertEqual(out, {"source": "timedatectl", "synchronized": True, "offset_seconds": 0.00125})
        unsynced = collect.ntp_status(lambda argv: CHRONY.replace("Normal", "Not synchronised") if argv[0] == "chronyc" else None)
        self.assertFalse(unsynced["synchronized"])
        fallback = collect.ntp_status(lambda argv: None)
        self.assertIn(fallback["source"], ("kernel", "unavailable"))

    def test_net_counters_absent_when_unreadable(self):
        self.assertIsNone(collect.net_counters("/nonexistent", 1))
        self.assertIsNone(collect.net_counters("/proc", 0))

    def test_demux_keeps_stdout_only(self):
        frame = lambda stream, data: bytes([stream, 0, 0, 0]) + len(data).to_bytes(4, "big") + data
        self.assertEqual(collect.demux_docker_stream(frame(2, b"warn") + frame(1, b"42\n")), b"42\n")

    def test_comparator_count_rejects_non_numbers(self):
        self.assertIsNone(collect.comparator_count(SpecDocker(count=lambda: None), "pg", "postgres", "bitmagnet"))
        self.assertEqual(collect.comparator_count(SpecDocker(count=lambda: 12), "pg", "postgres", "bitmagnet"), 12)


import unittest.mock  # noqa: E402

if __name__ == "__main__":
    unittest.main()
