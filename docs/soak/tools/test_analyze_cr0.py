import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from analyze_cr0 import analyze, markdown, parse_prom


def sample(elapsed, count=0, phase="baseline", **changes):
    row = {
        "elapsed_seconds": elapsed, "phase": phase, "process_id": 123,
        "health": 200, "metrics_ok": True, "working_set_bytes": (32 + elapsed / 60) * 2**20,
        "values": {"start_time_seconds": 1000, "dht_peer_admitted_total": count,
                   "dht_peer_active": 3, "dht_peer_queued": 0,
                   "dht_sample_cooling": 200000, "dht_sample_inflight": 0},
    }
    row.update(changes)
    return row


class AnalyzerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "metrics").mkdir()
        (self.root / "run.json").write_text(json.dumps({
            "commit": "a" * 40, "settings": {"baseline_hours": 24},
            "secret": "never-export-this",
        }), encoding="utf-8")

    def rows(self, rows, tail=""):
        (self.root / "samples.jsonl").write_text(
            "".join(json.dumps(r) + "\n" for r in rows) + tail, encoding="utf-8-sig")

    def counter(self, report, phase="baseline", name="dht_peer_admitted_total"):
        return report["phases"][phase]["counters"][name]

    def test_rates_and_memory_are_separate_by_phase(self):
        self.rows([sample(0, 100, "startup"), sample(60, 102, "startup"),
                   sample(120, 150, "warmup"), sample(180, 170, "warmup"),
                   sample(240, 200), sample(300, 230)])
        report = analyze(self.root)
        self.assertEqual(self.counter(report, "startup")["per_hour"], 120)
        self.assertEqual(self.counter(report, "warmup")["delta"], 20)
        self.assertEqual(self.counter(report)["delta"], 30)
        self.assertEqual(report["phases"]["baseline"]["rss_change_mib_per_hour"], 60)
        self.assertEqual(report["baseline_status"], "in_progress")
        self.assertNotIn("never-export-this", json.dumps(report))

    def test_missing_baseline_and_metrics_are_unknown(self):
        self.rows([sample(0, phase="startup")])
        report = analyze(self.root)
        self.assertEqual(report["baseline_status"], "not_started")
        self.assertIsNone(self.counter(report)["delta"])
        self.assertIsNone(self.counter(report, "startup")["per_hour"])
        self.assertIn("unknown", markdown(report))

    def test_gaps_failed_metrics_and_restarts_are_not_bridged(self):
        self.rows([sample(0, 0), sample(60, 2), sample(240, 10),
                   sample(300, 12, metrics_ok=False), sample(360, 20),
                   sample(420, 1, process_id=456), sample(480, 3, process_id=456)])
        report = analyze(self.root)
        self.assertEqual(self.counter(report)["delta"], 4)
        self.assertEqual(self.counter(report)["covered_seconds"], 120)
        self.assertEqual(report["issues"]["sampling_gaps"], 1)
        self.assertEqual(report["issues"]["identity_changes"], 1)
        self.assertEqual(report["phases"]["baseline"]["metrics_failed_samples"], 1)
        self.assertIsNone(report["phases"]["baseline"]["rss_change_mib"])

    def test_health_failure_is_counted_without_success_only_rate_bias(self):
        self.rows([sample(0), sample(60, 2, health=0)])
        report = analyze(self.root)
        self.assertEqual(self.counter(report)["per_hour"], 120)
        self.assertEqual(report["phases"]["baseline"]["health_failed_samples"], 1)

    def test_counter_reset_is_excluded_and_recovery_is_usable(self):
        self.rows([sample(0, 100), sample(60, 1), sample(120, 4)])
        report = analyze(self.root)
        self.assertEqual(report["issues"]["counter_discontinuities"], 1)
        self.assertEqual(self.counter(report)["delta"], 3)
        self.assertEqual(self.counter(report)["covered_seconds"], 60)

    def test_missing_counter_has_its_own_coverage(self):
        rows = [sample(0, 0), sample(60, 2), sample(120, 4), sample(180, 7)]
        del rows[1]["values"]["dht_peer_admitted_total"]
        self.rows(rows)
        report = analyze(self.root)
        self.assertEqual(report["phases"]["baseline"]["valid_interval_seconds"], 180)
        self.assertEqual(self.counter(report)["covered_seconds"], 60)
        self.assertEqual(self.counter(report)["delta"], 3)

    def test_label_reset_is_not_hidden_by_other_series_growth(self):
        self.rows([sample(0), sample(60)])
        for i, (tcp, utp) in enumerate([(100, 0), (1, 200)]):
            (self.root / "metrics" / f"{i:05d}.prom").write_text(
                f'lodestar_fetch_success_total{{transport="tcp"}} {tcp}\n'
                f'lodestar_fetch_success_total{{transport="utp"}} {utp}\n', encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["issues"]["counter_discontinuities"], 1)
        self.assertIsNone(self.counter(report, name="fetch_success_total")["delta"])

    def test_prom_counters_include_failures_and_scientific_notation(self):
        self.rows([sample(0), sample(60)])
        for i in range(2):
            (self.root / "metrics" / f"{i:05d}.prom").write_text(
                f'lodestar_fetch_failures_total{{kind="refused"}} {i * 2}e0\n'
                f'lodestar_fetch_failures_total{{kind="timed_out"}} {i}\n'
                'lodestar_unknown_metric{secret="do-not-export"} 123\n', encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(self.counter(report, name="fetch_failures_total")["per_hour"], 180)
        self.assertNotIn("do-not-export", json.dumps(report))

    def test_capacity_equality_is_not_violation(self):
        self.rows([sample(0), sample(60)])
        report = analyze(self.root)
        bounds = report["phases"]["baseline"]["bounds"]
        self.assertEqual(bounds["sampler_states"]["at_capacity_samples"], 2)
        self.assertEqual(bounds["sampler_states"]["over_capacity_samples"], 0)
        self.assertIsNone(bounds["pool_lookups"]["max"])
        row = sample(120)
        row["values"]["dht_sample_inflight"] = 1
        self.rows([row])
        self.assertEqual(analyze(self.root)["phases"]["baseline"]["bounds"]
                         ["sampler_states"]["over_capacity_samples"], 1)

    def test_partial_tail_is_safe_but_corrupt_middle_is_error(self):
        self.rows([sample(0)], '{"elapsed_seconds":')
        self.assertEqual(analyze(self.root)["issues"]["incomplete_tail_ignored"], 1)
        self.rows([sample(0)], '{"elapsed_seconds":\n{}\n')
        with self.assertRaisesRegex(ValueError, "line 2"):
            analyze(self.root)

    def test_nonmonotonic_and_regressed_phases_are_flagged(self):
        self.rows([sample(60), sample(0), sample(120, phase="startup")])
        report = analyze(self.root)
        self.assertEqual(report["issues"]["nonmonotonic_intervals"], 1)
        self.assertEqual(report["issues"]["phase_regressions"], 1)
        self.assertIsNone(self.counter(report)["delta"])

    def test_terminated_short_run_is_not_accepted(self):
        self.rows([sample(0), sample(60)])
        result = {"reason": "baseline_window_complete", "samples": 2, "stop_mode": "forced_kill"}
        (self.root / "result.json").write_text(json.dumps(result), encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["baseline_status"], "collector_window_complete_review_required")
        self.assertEqual(report["issues"]["baseline_evidence_short"], 1)
        self.assertFalse(report["clean_shutdown"])
        result["reason"] = "operator_stop"
        (self.root / "result.json").write_text(json.dumps(result), encoding="utf-8")
        self.assertEqual(analyze(self.root)["baseline_status"], "stopped_early")

    def test_invalid_prom_and_snapshot_mismatch_do_not_poison_json_rates(self):
        self.rows([sample(0), sample(60, 2)])
        (self.root / "metrics/00001.prom").write_text(
            "lodestar_dht_peer_admitted_total 999\n", encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["issues"]["invalid_prom_snapshots"], 1)
        self.assertEqual(self.counter(report)["delta"], 2)
        for raw in ("NaN", "+Inf", "-1"):
            with self.assertRaises(ValueError):
                parse_prom("lodestar_fetch_success_total " + raw)

    def test_cli_cannot_write_inside_evidence_or_overwrite_report(self):
        self.rows([sample(0)])
        script = str(Path(__file__).with_name("analyze_cr0.py"))
        before = (self.root / "samples.jsonl").read_bytes()
        result = subprocess.run([sys.executable, script, str(self.root), "--output",
                                 str(self.root / "samples.jsonl")], capture_output=True)
        self.assertEqual(result.returncode, 2)
        self.assertEqual((self.root / "samples.jsonl").read_bytes(), before)
        with tempfile.TemporaryDirectory() as output_dir:
            output = Path(output_dir) / "report.json"
            output.write_text("keep this", encoding="utf-8")
            result = subprocess.run([sys.executable, script, str(self.root), "--output",
                                     str(output)], capture_output=True)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(output.read_text(encoding="utf-8"), "keep this")

    def test_full_baseline_is_review_required_not_automatic_pass(self):
        self.rows([sample(t, t // 60) for t in range(0, 24 * 3600, 60)])
        (self.root / "result.json").write_text(json.dumps({
            "reason": "baseline_window_complete", "samples": 1440, "stop_mode": "exited_0",
        }), encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["baseline_status"], "collector_window_complete_review_required")
        self.assertNotIn("baseline_evidence_short", report["issues"])
        self.assertEqual(self.counter(report)["per_hour"], 60)
        self.assertTrue(report["clean_shutdown"])

    def test_positive_suppression_in_baseline_requires_review(self):
        row = sample(0)
        row["values"]["dht_sample_suppression_seconds"] = 10
        self.rows([row])
        self.assertEqual(analyze(self.root)["issues"]["baseline_during_suppression"], 1)


if __name__ == "__main__":
    unittest.main()
