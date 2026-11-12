"""X-soak-screen: the early-warning screen in the analyzer and the pre-soak sweep.

Synthetic runs are shaped by the published verdicts (the raw soak evidence is
private): r4's 128-512 KiB mappings, 1,843 -> 2,085 in 1.9 h, about 0.2 per stored
torrent, and its anonymous memory outside mimalloc, about 22.9 MiB per 1,000 stored;
r5's stored rate, segment and mmap-cache levels, flat mimalloc committed bytes, its
RssAnon slope of 0.86 MiB/h and its noisy early memory slope. No network, no wall
clock, no sleeps.
"""

import copy
import hashlib
import io
import json
import math
from pathlib import Path
import re
import tempfile
import unittest
from datetime import datetime, timezone

import analyze_cr0
from analyze_cr0 import analyze, markdown, screen_checkpoint, screen_config, screen_inputs
import screen_sweep

HERE = Path(__file__).resolve().parent
THRESHOLDS = HERE.parent / "screen-thresholds.json"
NOW = datetime(2026, 11, 7, tzinfo=timezone.utc)
MIB = 2**20


def config():
    return screen_config(json.loads(THRESHOLDS.read_text(encoding="utf-8")))


def sawtooth(h, low, high, period):
    return low + (high - low) * ((h / period) % 1.0)


def r5(h):
    """r5-shaped: about 681 stored an hour, merges bounding the segments (files
    peak 21), the mmap cache tracking them, mimalloc filling to 568 MiB then flat,
    RssAnon outside it rising 0.86 MiB/h with noise, the pool at its cap, and the
    noisy early memory slope (6.58 MiB/h to 14.6 h)."""
    stored = 900 + 681 * h
    files, main = round(sawtooth(h, 8, 21, 1.5)), round(sawtooth(h, 3, 6, 2.3))
    committed = 520 + 8 * min(h, 6.0)
    return {
        "stored": stored,
        "private_mib": 200 + 6.58 * min(h, 14.6) + 0.91 * max(0.0, h - 14.6) + 12 * math.sin(h * 7),
        "prom": {
            'search_mmap_cache_entries{index="files"}': 5 * files + 4,
            'search_mmap_cache_entries{index="main"}': 6 * main,
            'search_segments{index="files"}': files, 'search_segments{index="main"}': main,
            'mem_anon_mappings{size="lt_128k"}': 4, 'mem_anon_mappings{size="128k_512k"}': 2,
            'mem_anon_mappings{size="512k_2m"}': 0, 'mem_anon_mappings{size="ge_2m"}': 34,
            "mem_mimalloc_committed_bytes": committed * MIB,
            "mem_rss_anon_bytes": (committed + 40 + 0.86 * h + 3 * math.sin(h * 5)) * MIB,
            "fetch_held_bytes": (20 + 5 * math.sin(h)) * MIB,
        },
        "fetch_pending": 200000,
    }


def r4(h):
    """r5's shape with r4's growth: 128-512 KiB mappings at about 0.2 per stored torrent,
    about 120 kB dirty each, outside mimalloc (22.9 MiB per 1,000 stored)."""
    s = r5(h)
    s["prom"]['mem_anon_mappings{size="128k_512k"}'] = 300 + 0.2 * (s["stored"] - 900)
    s["prom"]["mem_rss_anon_bytes"] += 22.9 * (s["stored"] - 900) / 1000 * MIB
    return s


def failing_gate(h):
    """Screen-clean, but private memory climbs 29 MiB/h, as r4 did: the gate fails."""
    s = r5(h)
    s["private_mib"] = 300 + 29.35 * h
    return s


def no_new_gauges(h):
    """A binary from before X-soak-screen (r3's lacked even the mmap gauge): the
    mapping and held-bytes gauges are absent, the memory gauges are there."""
    s = r5(h)
    s["prom"] = {k: v for k, v in s["prom"].items() if not k.startswith(("mem_anon", "fetch_held"))}
    return s


def write_run(root, shape, hours=24.0, step=60, run=None, frozen=True, keep=lambda h: True):
    """A collector-shaped run directory: 5 warm-up rows, then a baseline every `step` s."""
    (root / "metrics").mkdir(parents=True)
    run = dict(run or {"commit": "a" * 40, "python": "3.12", "build_target": "x86_64-unknown-linux-musl",
                       "settings": {"mode": "docker", "baseline_hours": 24, "intensity": "balanced",
                                    "leg": "main", "rehearsal": False}})
    if frozen:
        data = THRESHOLDS.read_bytes()
        (root / "screen-thresholds.json").write_bytes(data)
        run["screen_thresholds_sha256"] = hashlib.sha256(data).hexdigest()
    (root / "run.json").write_text(json.dumps(run), encoding="utf-8")
    times = [(t, "warmup") for t in range(0, 300, 60)]
    times += [(300 + i * step, "baseline") for i in range(int(hours * 3600 / step) + 1)]
    lines, seq = [], 0
    for t, phase in times:
        h = (t - 300) / 3600
        if phase == "baseline" and not keep(h):
            continue
        s = shape(h)
        values = {"start_time_seconds": 1000, "torrents": round(s["stored"]), "fetch_pending": s["fetch_pending"],
                  "writer_stored_total": round(s["stored"]), "dht_sample_suppression_seconds": 0}
        row = {"sequence": seq, "elapsed_seconds": t, "phase": phase, "process_id": 123, "process_identity": "c1",
               "time": "2026-11-05T13:12:52Z", "health": 200, "metrics_ok": True,
               "private_bytes": s["private_mib"] * MIB, "working_set_bytes": (s["private_mib"] + 100) * MIB,
               "values": values}
        prom = [f"lodestar_{k} {v:g}" for k, v in values.items()]
        prom += [f"lodestar_{k} {v:.0f}" for k, v in s["prom"].items()]
        (root / "metrics" / f"{seq:05d}.prom").write_text("\n".join(prom) + "\n", encoding="utf-8")
        lines.append(json.dumps(row) + "\n")
        seq += 1
    (root / "samples.jsonl").write_text("".join(lines), encoding="utf-8")
    (root / "result.json").write_text(json.dumps({"reason": "baseline_window_complete", "samples": seq,
                                                  "state": "stopped"}), encoding="utf-8")
    return root


class Base(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.tmp = Path(temp.name)
        self.n = 0

    def run_dir(self, shape, **kw):
        self.n += 1
        return write_run(self.tmp / f"run{self.n}", shape, **kw)

    def screen(self, root, **kw):
        return analyze(root, now=NOW, **kw)["screen"]


class ThresholdsFileTests(unittest.TestCase):
    def test_every_threshold_has_its_value_reason_clean_value_and_margin(self):
        data = json.loads(THRESHOLDS.read_text(encoding="utf-8"))
        config()  # the analyzer accepts it
        for group in ("in_run", "sweep"):
            for name, c in data[group]["counters"].items():
                for field in ("reason", "clean", "margin", "unit"):
                    self.assertIn(field, c, f"{group}.{name}")
                if group == "in_run" and name in analyze_cr0.SCREEN_INFO:  # information only
                    self.assertEqual(c["trips"], "never", name)
                    self.assertNotIn("value", c, name)
                    continue
                self.assertIn("value", c, f"{group}.{name}")
                self.assertEqual(c["trips"], "above", name)
        self.assertEqual(screen_sweep.sweep_thresholds(THRESHOLDS),
                         {k: v["value"] for k, v in data["sweep"]["counters"].items()})

    def test_the_sweep_retention_threshold_is_the_pr_tests_bound(self):
        rust = (HERE.parents[2] / "bench/tests/mmap_retention.rs").read_text(encoding="utf-8")
        bound = float(re.search(r"const MAX_PER_COMMIT: f64 = ([0-9.]+);", rust)[1])
        self.assertEqual(screen_sweep.sweep_thresholds(THRESHOLDS)["retention_bytes_per_commit"], bound)

    def test_a_file_that_isnt_the_schema_is_refused(self):
        good = json.loads(THRESHOLDS.read_text(encoding="utf-8"))
        breaks = [
            lambda d: d.update(schema_version=2),
            lambda d: d["in_run"].update(checkpoints_hours=[3, 6]),
            lambda d: d["in_run"].update(files_per_segment=6),
            lambda d: d["in_run"].update(min_samples=0),
            lambda d: d["in_run"].update(min_samples=float("nan")),
            lambda d: d["in_run"].update(min_stored_spread=float("inf")),
            lambda d: d["in_run"].update(checkpoint_tolerance_seconds=float("nan")),
            lambda d: d["in_run"]["counters"].pop("segments"),
            lambda d: d["in_run"]["counters"]["segments"].update(value="64"),
            lambda d: d["in_run"]["counters"]["segments"].update(trips="below"),
            lambda d: d["in_run"]["counters"].update(extra={"value": 1, "trips": "above"}),
            # An information counter never gets a threshold, and a judged one is never "never".
            lambda d: d["in_run"]["counters"]["mimalloc_committed_mib_per_1000_stored"].update(value=20),
            lambda d: d["in_run"]["counters"]["mimalloc_committed_mib_per_1000_stored"].update(trips="above"),
            lambda d: d["in_run"]["counters"].update(mimalloc_committed_mib_per_1000_stored=None),
            lambda d: d["in_run"]["counters"]["segments"].update(trips="never"),
        ]
        for i, broken in enumerate(breaks):
            d = copy.deepcopy(good)
            broken(d)
            with self.assertRaises(ValueError, msg=i):
                screen_config(d)
            with self.assertRaises(ValueError, msg=i):  # through JSON text too
                screen_config(json.loads(json.dumps(d)))
        # json.loads reads NaN and Infinity in a file; the screen refuses them.
        nan = json.dumps(good).replace('"min_samples": 30', '"min_samples": NaN')
        self.assertIn("NaN", nan)
        with self.assertRaisesRegex(ValueError, "min_samples"):
            screen_config(json.loads(nan))


class CoreTests(unittest.TestCase):
    def test_the_collector_and_analyzer_share_one_screen_core(self):
        def core(path):
            text = path.read_text(encoding="utf-8")
            start, end = text.index("# --- screen core"), text.index("# --- end of screen core")
            return text[start:end]
        self.assertEqual(core(HERE / "analyze_cr0.py"), core(HERE / "linux" / "collect.py"))

    def test_inputs_read_from_a_snapshot(self):
        text = ('lodestar_search_mmap_cache_entries{index="files"} 72\n'
                'lodestar_search_segments{index="files"} 15\n'
                'lodestar_search_segments{index="other"} 99\n'
                'lodestar_mem_anon_mappings{size="128k_512k"} 2085\n'
                'lodestar_mem_anon_mappings{size="lt_128k"} 9\n'
                "lodestar_mem_mimalloc_committed_bytes 595591168\n"
                "lodestar_fetch_held_bytes 1048576\n")
        got = screen_inputs({"torrents": 17244, "fetch_pending": 200000, "x": True}, text)
        self.assertEqual(got, {"stored": 17244, "fetch_pending": 200000, "mmap_cache_entries[files]": 72,
                               "segments[files]": 15, "mmap_cache_excess[files]": 72 - 8 * 15,
                               "anon_mappings_128k_512k": 2085, "mimalloc_committed_mib": 568,
                               "fetch_held_mib": 1})
        # RssAnon with mimalloc's committed bytes gives the memory outside it.
        got = screen_inputs({}, text + "lodestar_mem_rss_anon_bytes 637534208\n")
        self.assertEqual((got["rss_anon_mib"], got["anon_outside_mimalloc_mib"]), (608, 40))
        self.assertEqual(screen_inputs({}, ""), {})
        # A value no float holds, or a non-finite JSON value, is absent, never ok.
        self.assertEqual(screen_inputs({"torrents": float("inf")},
                                       "lodestar_fetch_held_bytes " + "9" * 400 + "\n"), {})

    def linear(self, key, slope, at_value=None):
        """41 points, h = i/10, stored = 1,000 i, the input `slope` per 1,000."""
        pts = [(i / 10, {"stored": 1000.0 * i, key: slope * i}) for i in range(41)]
        if at_value is not None:
            pts[30][1][key] = at_value  # the sample at hour 3
        return pts

    def test_each_threshold_just_below_at_and_just_above(self):
        """Trips strictly above the threshold; at it, ok."""
        run = config()
        for name, kind, key in analyze_cr0.SCREEN_COUNTERS:
            if name in analyze_cr0.SCREEN_INFO:
                continue  # test_information_counters_never_trip
            limit = run["counters"][name]["value"]
            k = key.replace("[]", "[main]")
            label = name + ("[main]" if key.endswith("[]") else "")
            for delta, want in ((-0.01, "ok"), (0, "ok"), (0.01, "trip"), (-1e6, "ok")):
                if kind == "growth":
                    pts = self.linear(k, limit + delta)
                else:
                    pts = self.linear(k, 0, at_value=limit + delta)
                got = screen_checkpoint(pts, 4.0, 3, run)["counters"][label]
                self.assertEqual(got["status"], want, f"{label} at {limit}{delta:+g}: {got}")
                if want == "trip":
                    self.assertEqual(got["value"] > got["threshold"], True)

    def test_information_counters_never_trip(self):
        run = config()
        for name in analyze_cr0.SCREEN_INFO:
            key = dict((n, k) for n, _, k in analyze_cr0.SCREEN_COUNTERS)[name]
            for slope in (0, 37.79, 1e6):
                out = screen_checkpoint(self.linear(key, slope), 4.0, 3, run)
                r = out["counters"][name]
                self.assertEqual((r["status"], r["info"], r["threshold"]), ("info", True, None))
                self.assertAlmostEqual(r["value"], slope)
                self.assertNotEqual(out.get("counter"), name)
                self.assertNotIn(name, " ".join(out.get("without", [])))
        # It never decides a checkpoint alone: with only it computed, there's no ok.
        alone = [(h, {"stored": p["stored"]}) for h, p in self.linear("x", 0)]
        for h, p in alone:
            p["mimalloc_committed_mib"] = 0.0 if h < 3 else None
        alone = [(h, {k: v for k, v in p.items() if v is not None}) for h, p in alone]
        out = screen_checkpoint(alone, 4.0, 3, run)
        self.assertEqual(out["counters"]["mimalloc_committed_mib_per_1000_stored"]["status"], "info")
        self.assertEqual(out["line"], "not_available(absent:mmap_cache_entries_per_1000_stored[main])")

    def test_a_trip_names_the_counter_value_and_threshold(self):
        run = config()
        out = screen_checkpoint(self.linear("anon_mappings_128k_512k", 200), 4.0, 3, run)
        self.assertEqual(out["status"], "trip")
        self.assertEqual(out["line"], "trip(anon_mappings_128k_512k_per_1000_stored, 200.00, 20)")

    def test_growth_needs_samples_and_stored_spread(self):
        run = config()
        few = self.linear("segments[main]", 0)[:5]
        self.assertEqual(screen_checkpoint(few, 4.0, 0.4, run)["counters"]["segments_per_1000_stored[main]"]
                         ["status"], "too_few_samples")
        flat = [(h, dict(p, stored=900.0)) for h, p in self.linear("segments[main]", 0)]
        self.assertEqual(screen_checkpoint(flat, 4.0, 3, run)["counters"]["segments_per_1000_stored[main]"]
                         ["status"], "too_little_stored")


class RunTests(Base):
    def test_r4s_growth_trips_by_hour_3(self):
        s = self.screen(self.run_dir(r4))
        for c in s["checkpoints"]:
            self.assertEqual(c["status"], "trip", c["line"])
            self.assertEqual(c["counter"], "anon_mappings_128k_512k_per_1000_stored")
            self.assertAlmostEqual(c["counters"][c["counter"]]["value"], 200, delta=1)
        self.assertEqual(s["checkpoints"][0]["hour"], 3)
        self.assertEqual(s["overall"]["line"], s["checkpoints"][0]["line"])
        self.assertRegex(s["overall"]["line"], r"^trip\(anon_mappings_128k_512k_per_1000_stored, 19[89]\.[0-9]{2}, 20\)$")
        # The level bound trips too once the count passes 512.
        self.assertEqual(s["checkpoints"][0]["counters"]["anon_mappings_128k_512k"]["status"], "trip")

    def test_an_r5_shaped_run_doesnt_trip(self):
        s = self.screen(self.run_dir(r5))
        self.assertEqual([c["line"] for c in s["checkpoints"]], ["ok", "ok", "ok"])
        self.assertEqual(s["overall"]["line"], "ok")
        # Every input counted, none quietly skipped; mimalloc's growth shown, not judged.
        for c in s["checkpoints"]:
            self.assertEqual(len(c["counters"]), 15)
            self.assertEqual({n for n, r in c["counters"].items() if r["status"] != "ok"},
                             {"mimalloc_committed_mib_per_1000_stored"})
            self.assertEqual(c["counters"]["mimalloc_committed_mib_per_1000_stored"]["status"], "info")
            self.assertLess(c["counters"]["anon_outside_mimalloc_mib_per_1000_stored"]["value"], 2)

    def test_private_memory_is_not_a_screen_input(self):
        """r5 read 6.58 MiB/h at 14.6 h; any private-memory series gives the same screen."""
        quiet = self.screen(self.run_dir(r5))
        def flat(h):
            s = r5(h)
            s["private_mib"] = 250
            return s
        for shape in (failing_gate, flat):
            self.assertEqual(self.screen(self.run_dir(shape))["checkpoints"], quiet["checkpoints"])

    def test_a_live_mapping_leak_trips_the_mmap_cache(self):
        def leak(h):
            s = r5(h)
            s["prom"]['search_mmap_cache_entries{index="files"}'] += s["stored"] - 900  # one per commit
            return s
        c = self.screen(self.run_dir(leak))["checkpoints"][0]
        self.assertEqual(c["counter"], "mmap_cache_entries_per_1000_stored[files]")

    def test_merges_stopping_trip_the_segments(self):
        def stuck(h):
            s = r5(h)
            s["prom"]['search_segments{index="main"}'] = 3 + (s["stored"] - 900) / 100
            return s
        c = self.screen(self.run_dir(stuck))["checkpoints"][0]
        self.assertEqual(c["status"], "trip")
        self.assertIn("[main]", c["counter"])

    def test_the_pool_over_its_cap_trips(self):
        def over(h):
            s = r5(h)
            s["fetch_pending"] = 200001 if h > 5 else 200000
            return s
        s = self.screen(self.run_dir(over))
        self.assertEqual(s["checkpoints"][0]["line"], "ok")  # at the cap is backpressure
        self.assertEqual(s["checkpoints"][1]["line"], "trip(fetch_pending, 200001.00, 200000)")


class NeverAPassTests(Base):
    def gate(self, report):
        return {k: v for k, v in report.items() if k != "screen"}

    def test_the_gate_is_byte_identical_with_the_screen_on_or_off(self):
        for shape in (r5, r4, failing_gate, no_new_gauges):
            root = self.run_dir(shape)
            on, off = analyze(root, now=NOW), analyze(root, now=NOW, screen=False)
            self.assertNotIn("screen", off)
            self.assertEqual(json.dumps(self.gate(on), sort_keys=True), json.dumps(off, sort_keys=True))
            self.assertTrue(markdown(on).startswith(markdown(off)), shape.__name__)

    def test_every_analyzer_fixture_is_unchanged_by_the_screen(self):
        """Runs test_analyze_cr0's whole suite with each analysis done both ways."""
        import test_analyze_cr0 as suite
        real, calls = suite.analyze, []

        def both(root, *args, **kw):
            on = real(root, *args, **kw)  # each fixture's own arguments, unchanged
            off = real(root, *args, **dict(kw, screen=False))
            if "now" not in kw:  # freshness reads the wall clock without one
                on, off = dict(on, observation_state=None), dict(off, observation_state=None)
            self.assertEqual(json.dumps(self.gate(on), sort_keys=True), json.dumps(off, sort_keys=True))
            self.assertTrue(markdown(on).startswith(markdown(off)))
            calls.append(root)
            return on
        suite.analyze = both
        try:
            result = unittest.TextTestRunner(stream=io.StringIO(), verbosity=0).run(
                unittest.defaultTestLoader.loadTestsFromModule(suite))
        finally:
            suite.analyze = real
        self.assertTrue(result.wasSuccessful(), result.failures + result.errors)
        self.assertGreater(len(calls), 30)

    def edit_rows(self, root, change):
        rows = [json.loads(line) for line in (root / "samples.jsonl").read_text().splitlines()]
        for r in rows:
            change(r)
        (root / "samples.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))

    def test_evidence_the_screen_cant_read_never_touches_the_gate(self):
        """A JSON integer past a float's range, or a dict where the identity goes:
        the gate's output is what it is with the screen off, and the screen says
        so rather than failing the report."""
        def huge(r):
            if r["phase"] == "baseline" and r["elapsed_seconds"] == 300 + 7200:
                r["values"]["torrents"] = 10**400
                r["values"]["fetch_pending"] = 10**400
        def odd_identity(r):
            if r["phase"] == "baseline" and r["elapsed_seconds"] == 300 + 3600:
                r["process_identity"] = {"not": "a string"}
        for change, line in ((huge, "ok"), (odd_identity, "not_available(screen_error)")):
            root = self.run_dir(r5)
            plain = analyze(root, now=NOW)
            self.edit_rows(root, change)
            on, off = analyze(root, now=NOW), analyze(root, now=NOW, screen=False)
            self.assertEqual(json.dumps(self.gate(on), sort_keys=True), json.dumps(off, sort_keys=True))
            self.assertTrue(markdown(on).startswith(markdown(off)))
            self.assertIs(off["memory_fit"]["meets_bar"], plain["memory_fit"]["meets_bar"])
            self.assertEqual(on["screen"]["overall"]["line"], line, change.__name__)
            json.dumps(on, allow_nan=False)  # the CLI's own serialisation
        self.assertEqual({c["line"] for c in on["screen"]["checkpoints"]}, {"not_available(screen_error)"})

    def test_the_cli_reports_with_unreadable_screen_inputs(self):
        import subprocess
        import sys
        root = self.run_dir(r5)
        self.edit_rows(root, lambda r: r["values"].update(torrents=10**400))
        for extra in ([], ["--no-screen"]):
            done = subprocess.run([sys.executable, "-I", str(HERE / "analyze_cr0.py"), str(root), "--format", "json",
                                   *extra], capture_output=True, text=True, timeout=120)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual("screen" in json.loads(done.stdout), not extra)

    def test_overflowing_sums_are_unusable_not_ok_and_the_exit_code_holds(self):
        """The runner's repro: finite 308-digit counts whose slope sums overflow."""
        import subprocess
        import sys
        def huge(h):
            s = r5(h)
            s["prom"]['mem_anon_mappings{size="128k_512k"}'] = 1e307 * (1 + h)
            return s
        root = self.run_dir(huge)
        c = self.screen(root)["checkpoints"][0]["counters"]["anon_mappings_128k_512k_per_1000_stored"]
        self.assertEqual((c["status"], c["value"]), ("unusable", None))
        outs = {}
        for extra in ([], ["--no-screen"]):
            done = subprocess.run([sys.executable, "-I", str(HERE / "analyze_cr0.py"), str(root), "--format", "json",
                                   *extra], capture_output=True, text=True, timeout=120)
            self.assertEqual(done.returncode, 0, done.stderr)
            outs[bool(extra)] = json.loads(done.stdout)
        on, off = outs[False], outs[True]
        for report in (on, off):
            report.pop("observation_state")  # the CLI reads the wall clock
        self.assertEqual(json.dumps(self.gate(on), sort_keys=True), json.dumps(off, sort_keys=True))
        # The level bound is a real reading (about 4e307 > 512) and trips, shown short.
        self.assertEqual(on["screen"]["overall"]["line"], "trip(anon_mappings_128k_512k, 4.000e+307, 512)")
        # Without the bound tripping, an unusable growth is never ok.
        out = screen_checkpoint([(i / 10, {"stored": 1000.0 * i, "anon_mappings_128k_512k": 1e307 * (1 + i)})
                                 for i in range(41)], 4.0, 3,
                                dict(config(), counters={**config()["counters"], "anon_mappings_128k_512k":
                                                         {"value": float("inf"), "trips": "above"}}))  # bound off
        self.assertEqual(out["counters"]["anon_mappings_128k_512k_per_1000_stored"]["status"], "unusable")
        self.assertNotEqual(out["status"], "ok")

    def test_a_stored_spread_too_wide_to_square_is_unusable_not_an_error(self):
        # (x - mx) ** 2 raised OverflowError past about 1e154; the slope is no reading.
        bound_off = dict(config(), counters={**config()["counters"], "anon_mappings_128k_512k":
                                             {"value": float("inf"), "trips": "above"}})
        out = screen_checkpoint([(i / 10, {"stored": 1e158 * (1 + i), "anon_mappings_128k_512k": 1.0 * i})
                                 for i in range(41)], 4.0, 3, bound_off)
        self.assertEqual(out["counters"]["anon_mappings_128k_512k_per_1000_stored"]["status"], "unusable")
        self.assertIsNone(out["counters"]["anon_mappings_128k_512k_per_1000_stored"]["value"])
        self.assertNotEqual(out["status"], "ok")
        self.assertTrue(math.isnan(analyze_cr0.screen_slope([(1e155, 0.0), (3e155, 1.0)])))

    def test_a_result_the_cli_cant_write_is_a_screen_error(self):
        root = self.run_dir(r5)
        real = analyze_cr0.screen_evaluate
        def nan(*args):
            out = real(*args)
            out["checkpoints"][0]["counters"]["segments[main]"]["value"] = float("nan")
            return out
        analyze_cr0.screen_evaluate = nan
        try:
            report = analyze(root, now=NOW)
        finally:
            analyze_cr0.screen_evaluate = real
        self.assertEqual(report["screen"]["overall"]["line"], "not_available(screen_error)")
        json.dumps(report, allow_nan=False)

    def test_with_the_screen_off_no_snapshot_text_is_kept(self):
        root = self.run_dir(r5, hours=1.0)
        issues = analyze_cr0.Counter()
        self.assertFalse(any("screen_text" in r for r in analyze_cr0.load_rows(root, issues)))
        kept = [r["screen_text"] for r in analyze_cr0.load_rows(root, issues, screen=True)]
        self.assertTrue(all(line.startswith(analyze_cr0.SCREEN_PREFIXES)
                            for text in kept for line in text.splitlines()))
        self.assertTrue(any("mem_anon_mappings" in text for text in kept))
        self.assertFalse(any("lodestar_torrents" in text for text in kept))

    def test_ok_never_implies_a_gate_pass(self):
        report = analyze(self.run_dir(failing_gate), now=NOW)
        self.assertEqual(report["screen"]["overall"]["line"], "ok")
        self.assertIs(report["memory_fit"]["meets_bar"], False)
        self.assertTrue(report["screen"]["never_a_pass"])
        text = markdown(report)
        self.assertIn("misses the bar", text)
        self.assertIn("ok never means the gate passed", text)
        flat = json.dumps(report["screen"])
        for word in ("meets_bar", '"pass"', "verdict", "gate_evidence"):
            self.assertNotIn(word, flat)

    def test_the_screen_is_printed_apart_from_the_gate(self):
        text = markdown(analyze(self.run_dir(r4), now=NOW))
        gate, screen = text.split("## Screen (early warning, never a pass)")
        self.assertIn("## Memory gate", gate)
        self.assertNotIn("screen:", gate)
        self.assertIn("screen: trip(anon_mappings_128k_512k_per_1000_stored", screen)
        self.assertIn("- Baseline hour 6: trip(", screen)


class MissingDataTests(Base):
    def test_snapshots_without_the_new_metrics_decide_on_the_rest_and_name_them(self):
        s = self.screen(self.run_dir(no_new_gauges))
        absent = ["absent:anon_mappings_128k_512k_per_1000_stored", "absent:anon_mappings_128k_512k",
                  "absent:fetch_held_mib"]
        self.assertEqual(s["overall"]["line"], f"ok(without {', '.join(absent)})")
        for c in s["checkpoints"]:
            self.assertEqual(c["status"], "ok")
            self.assertEqual(c["without"], absent)
            self.assertEqual(c["line"], f"ok(without {', '.join(absent)})")
            for name, r in c["counters"].items():
                if name.startswith(("anon_mappings", "fetch_held")):
                    self.assertEqual(r["status"], "absent", name)
                    self.assertIsNone(r["value"])

    def test_older_run_directories_still_analyze(self):
        """r3's binary: no mmap, segment, memory or mapping gauges, and no frozen thresholds.
        The pool's key count is still read, so the screen decides on that and names the rest."""
        def r3(h):
            s = r5(h)
            s["prom"] = {}
            return s
        report = analyze(self.run_dir(r3, frozen=False), now=NOW)
        self.assertIsNotNone(report["memory_fit"]["slope_mib_per_hour"])
        self.assertEqual(report["screen"]["thresholds"]["source"], "analyzer_not_frozen")
        for c in report["screen"]["checkpoints"]:
            self.assertEqual(c["status"], "ok")
            self.assertEqual({n for n, r in c["counters"].items() if r["status"] == "ok"}, {"fetch_pending"})
            self.assertEqual(len(c["without"]), 13)
        self.assertIn("aren't frozen", markdown(report))
        # With the pool's count gone too there's nothing usable: not_available.
        def bare(h):
            s = r5(h)
            s["prom"], s["fetch_pending"] = {}, -1  # negative: no reading
            return s
        report = analyze(self.run_dir(bare, frozen=False), now=NOW)
        self.assertEqual(report["screen"]["overall"]["line"],
                         "not_available(absent:mmap_cache_entries_per_1000_stored[main])")

    def test_a_missing_checkpoint_doesnt_shift_the_others(self):
        s = self.screen(self.run_dir(r5, keep=lambda h: not 5.5 < h <= 6.0))
        self.assertEqual([c["hour"] for c in s["checkpoints"]], [3, 6, 12])
        self.assertEqual([c["line"] for c in s["checkpoints"]],
                         ["ok", "not_available(no_sample_at_checkpoint)", "ok"])
        # Hour 12's window still holds every sample up to hour 12, not hour 6's gap's neighbours.
        full = self.screen(self.run_dir(r5))["checkpoints"][2]["counters"]["segments[files]"]
        self.assertEqual(s["checkpoints"][2]["counters"]["segments[files]"], full)
        self.assertEqual(s["overall"]["line"], "not_available(no_sample_at_checkpoint)")

    def test_checkpoints_not_reached_are_not_available_and_dont_hold_back_ok(self):
        s = self.screen(self.run_dir(r5, hours=4.0))
        self.assertEqual([c["line"] for c in s["checkpoints"]],
                         ["ok", "not_available(not_reached)", "not_available(not_reached)"])
        self.assertEqual(s["overall"]["line"], "ok")
        s = self.screen(self.run_dir(r5, hours=2.0))
        self.assertEqual(s["overall"]["line"], "not_available(not_reached)")

    def test_failed_metrics_reads_count_as_missing_not_zero(self):
        root = self.run_dir(r5)
        rows = [json.loads(line) for line in (root / "samples.jsonl").read_text().splitlines()]
        for r in rows:
            if r["phase"] == "baseline" and r["elapsed_seconds"] % 600 == 0:
                r["metrics_ok"] = False
        (root / "samples.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
        self.assertEqual(self.screen(root)["overall"]["line"], "ok")

    def test_a_restart_before_a_checkpoint_refuses_it(self):
        root = self.run_dir(r5)
        rows = [json.loads(line) for line in (root / "samples.jsonl").read_text().splitlines()]
        for r in rows:
            if r["elapsed_seconds"] > 300 + 4 * 3600:
                r["process_id"] = 456
        (root / "samples.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
        self.assertEqual([c["line"] for c in self.screen(root)["checkpoints"]],
                         ["ok", "not_available(identity_change)", "not_available(identity_change)"])

    def test_the_windows_collector_names_linux_only_gauges_and_decides_on_the_rest(self):
        """cr0-soak.ps1 evidence: no /proc gauges, so no mapping counts or RssAnon; nothing trips."""
        def windows(h):
            s = r5(h)
            s["prom"] = {k: v for k, v in s["prom"].items() if not k.startswith(("mem_anon", "mem_rss"))}
            return s
        run = {"commit": "a" * 40, "common_sha256": "b" * 64, "os": "Microsoft Windows 11",
               "settings": {"baseline_hours": 24, "intensity": "balanced", "leg": "main", "rehearsal": False}}
        s = self.screen(self.run_dir(windows, run=run, frozen=False))
        for c in s["checkpoints"]:
            self.assertEqual(c["line"], "ok(without absent:anon_mappings_128k_512k_per_1000_stored, "
                                        "absent:anon_mappings_128k_512k, "
                                        "absent:anon_outside_mimalloc_mib_per_1000_stored)")
            rest = {n: r["status"] for n, r in c["counters"].items() if not n.startswith("anon")}
            self.assertEqual(set(rest.values()), {"ok", "info"}, rest)


class FreezingTests(Base):
    def test_the_frozen_copy_is_used_and_checked(self):
        root = self.run_dir(r5)
        s = self.screen(root)
        self.assertEqual(s["thresholds"]["source"], "frozen_with_run")
        self.assertIs(s["thresholds"]["changed_since_start"], False)
        self.assertEqual(s["thresholds"]["sha256"], hashlib.sha256(THRESHOLDS.read_bytes()).hexdigest())

    def test_a_thresholds_file_changed_after_start_is_flagged(self):
        root = self.run_dir(r4)
        # The checked-in file changed: flagged, and the frozen copy still judges.
        current = self.tmp / "current.json"
        loose = json.loads(THRESHOLDS.read_text())
        loose["in_run"]["counters"]["anon_mappings_128k_512k_per_1000_stored"]["value"] = 1e9
        current.write_text(json.dumps(loose))
        s = self.screen(root, screen_current=current)
        self.assertIs(s["thresholds"]["changed_since_start"], True)
        self.assertEqual(s["checkpoints"][0]["status"], "trip")
        self.assertIn("has changed since this run started", markdown(analyze(root, now=NOW, screen_current=current)))
        # The run's own copy edited: refused, never judged by the edit.
        (root / "screen-thresholds.json").write_text(json.dumps(loose))
        s = self.screen(root)
        self.assertEqual({c["line"] for c in s["checkpoints"]}, {"not_available(thresholds_modified)"})
        (root / "screen-thresholds.json").unlink()
        self.assertEqual(self.screen(root)["overall"]["line"], "not_available(thresholds_missing)")

    def test_a_malformed_recorded_hash_is_not_available(self):
        root = self.run_dir(r5)
        run = json.loads((root / "run.json").read_text())
        run["screen_thresholds_sha256"] = "not-a-hash"
        (root / "run.json").write_text(json.dumps(run))
        self.assertEqual(self.screen(root)["overall"]["line"], "not_available(thresholds_hash_invalid)")

    def test_an_invalid_or_missing_file_is_not_available(self):
        root = self.run_dir(r5, frozen=False)
        self.assertEqual(self.screen(root, screen_current=self.tmp / "none.json")["overall"]["line"],
                         "not_available(no_thresholds)")
        bad = self.tmp / "bad.json"
        bad.write_text("{")
        self.assertEqual(self.screen(root, screen_current=bad)["overall"]["line"],
                         "not_available(invalid_thresholds)")


class DeterminismTests(Base):
    def test_the_same_input_gives_the_same_output(self):
        root = self.run_dir(r4)
        a, b = analyze(root, now=NOW), analyze(root, now=NOW)
        self.assertEqual(json.dumps(a, sort_keys=True), json.dumps(b, sort_keys=True))
        self.assertEqual(markdown(a), markdown(b))
        self.assertEqual(json.dumps(self.screen(self.run_dir(r4))), json.dumps(a["screen"]))


class SweepTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.tmp = Path(temp.name)

    def sweep(self, *argv):
        out = io.StringIO()
        import contextlib
        with contextlib.redirect_stdout(out):
            code = screen_sweep.main(list(argv))
        return code, out.getvalue()

    def cheap(self, per_1000, name="cheap.csv"):
        header = "torrents,rss_anon_mib,<128K,128K-512K,512K-2M"
        rows = [f"{n},1.0,3,{round(6 + per_1000 * n / 1000)},2" for n in range(0, 10001, 1000)]  # <128K fixed
        path = self.tmp / name
        path.write_text("\n".join([header] + rows + ["# 10000 torrents in 271 s"]) + "\n")
        return path

    def log(self, bytes_per_commit, pruning="on"):
        path = self.tmp / f"r{bytes_per_commit}.log"
        # As cargo test --nocapture prints it: stderr on the test-name line.
        path.write_text("running 1 test\ntest one_document_commits_retain_no_memory_per_commit ... "
                        f"retained {bytes_per_commit} B per commit (pruning {pruning})\nok\n")
        return path

    def test_the_r3_leak_fails_loudly_naming_the_counter(self):
        """r3's arm (pruning off) retains about 2,300 B a commit."""
        code, out = self.sweep("--retention-log", str(self.log(2305, "off")))
        self.assertEqual(code, 1)
        self.assertIn("trip(retention_bytes_per_commit, 2305.00, 300)", out)
        code, out = self.sweep("--retention-log", str(self.log(2305, "off")), "--expect", "trip")
        self.assertEqual(code, 0, out)

    def test_the_r4_growth_trips_and_override_passes(self):
        code, out = self.sweep("--mem-cheap-csv", str(self.cheap(5.7)))
        self.assertEqual(code, 1)
        self.assertIn("trip(musl_anon_under_512k_per_1000_torrents", out)
        code, out = self.sweep("--mem-cheap-csv", str(self.cheap(0)))
        self.assertEqual(code, 0, out)
        # A control arm that stopped tripping fails: the screen lost its sight.
        code, _ = self.sweep("--mem-cheap-csv", str(self.cheap(0)), "--expect", "trip")
        self.assertEqual(code, 1)

    def test_boundaries(self):
        for value, want in ((299, 0), (300, 0), (301, 1)):
            self.assertEqual(self.sweep("--retention-log", str(self.log(value)))[0], want, value)
        # Through the CSV parser: integer counts whose OLS slope is exactly 2, or
        # 2 with the last row one higher or lower (2 +- about 0.05).
        for last, want in ((-1, 0), (0, 0), (1, 1)):
            path = self.tmp / f"exact{last}.csv"
            rows = [f"{n},0,{2 * n // 1000 + (last if n == 10000 else 0)}" for n in range(1000, 10001, 1000)]
            path.write_text("torrents,<128K,128K-512K\n" + "\n".join(rows) + "\n")
            self.assertEqual(self.sweep("--mem-cheap-csv", str(path))[0], want, last)
        self.assertEqual(screen_sweep.mem_cheap((self.tmp / "exact0.csv").read_text()), 2.0)

    def test_unreadable_input_is_not_available(self):
        bad = self.tmp / "bad.log"
        bad.write_text("no measurement here\n")
        code, out = self.sweep("--retention-log", str(bad))
        self.assertEqual(code, 2)
        self.assertIn("not_available", out)
        bad.write_text("torrents,<128K,128K-512K\n1000,0,x\n")
        self.assertEqual(self.sweep("--mem-cheap-csv", str(bad))[0], 2)


if __name__ == "__main__":
    unittest.main()
