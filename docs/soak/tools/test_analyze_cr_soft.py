"""X-soft-key: the `soft` treatment (CR-SOFT-TIMEOUT's switch, `soft_timeout`) and its report,
CR-SOFT-TIMEOUT's pair per hour, late ÷ soft and soft ÷ peer-lookup queries over sampling
minutes only (docs/decisions/2026-11-23-X-soft-key.md). Reported, never decided on.

Segments are written as the Linux collector writes them, by test_analyze_cr's Sim.
"""

import json
import math
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE / "linux"))

import analyze_cr  # noqa: E402
import collect  # noqa: E402
import test_analyze_cr as cr  # noqa: E402
import test_analyze_cr_col5 as col5  # noqa: E402
import test_analyze_cr_col6 as col6  # noqa: E402

SOFT, SPLIT, LEDGER = collect.SOFT, collect.SPLIT, collect.LEDGER
ADM = "coordinated_admission"
# Per second, in sampling time: soft queries, late replies, peer-lookup queries sent.
SOFT_RATE, LATE_RATE, QUERY_RATE = 2.0, 0.01, 20.0


class SoftSim(cr.Sim):
    """A segment whose binary renders CR-SOFT-TIMEOUT's pair, the peer-lookup query count and
    sampler-full time. `on` is soft_timeout (the B arm); the winners are on in both arms. From
    `stall` = (from_e, to_e) the sampler's table is full: sampler-full time grows, and the soft
    count runs ten times faster, so a report that counted stalled minutes would show it."""

    def __init__(self, treatment="soft", on=True, winners=(ADM,), stall=None, **kw):
        switches = {w: True for w in winners}
        switches["soft_timeout"] = on
        super().__init__(treatment=treatment, switches=switches, **kw)
        self.stall = stall
        self.drop_series = set()

    def stalled(self, e):
        """Seconds of stall up to elapsed e."""
        if not self.stall:
            return 0.0
        return max(0.0, min(e, self.stall[1]) - self.stall[0])

    def snapshot(self, e, row):
        lines = super().snapshot(e, row)
        out = []
        add = lambda name, value, labels="": out.append(f"lodestar_{name}{labels} {value}")
        stalled = self.stalled(e)
        on = self.switches["soft_timeout"]
        add(SOFT["soft"], math.floor((e - stalled) * SOFT_RATE + stalled * SOFT_RATE * 10) if on else 0)
        add(SOFT["late"], math.floor((e - stalled) * LATE_RATE) if on else 0)
        add(LEDGER["queries"], math.floor(e * QUERY_RATE), f'{{purpose="{collect.LEDGER_PEER_PURPOSE}"}}')
        add(SPLIT["full_seconds"], f"{stalled:.3f}")
        out = [x for x in out if x.split("{")[0].split(" ")[0][len("lodestar_"):] not in self.drop_series]
        return lines + out


class Base(col5.Base):
    pass


class TreatmentTests(Base):
    def test_soft_is_a_stacked_treatment_with_its_own_switch(self):
        self.assertEqual(collect.TREATMENT_SWITCHES["soft"], ("soft_timeout",))
        self.assertIn("soft", collect.STACKED)
        self.assertEqual(collect.SWITCH_ENV["soft_timeout"], "LODESTAR_DHT_SOFT_TIMEOUT")

    def test_winners_are_required_may_be_empty_and_never_soft_itself(self):
        with self.assertRaisesRegex(ValueError, "give --winners"):
            self.run_sim(SoftSim(), winners=None)
        with self.assertRaisesRegex(ValueError, "treatment's own"):
            self.run_sim(SoftSim(), winners=[ADM, "soft_timeout"])
        # Against the shipped defaults: no winners, so nothing else on.
        self.assertValid(self.run_sim(SoftSim(winners=()), winners=[]))

    def test_the_cli_takes_soft(self):
        winners = col5.write_winners(self.tmp / "w.json", [ADM])
        root = SoftSim().write(self.tmp / "cli")
        out = self.tmp / "soft.json"
        self.assertEqual(analyze_cr.main(["segment", str(root), "--output", str(out), "--winners", str(winners)]), 0)
        report = json.loads(out.read_text())
        self.assertValid(report)
        self.assertIn("## Soft timeout", analyze_cr.markdown(report))
        self.assertNotIn("## Soft timeout", analyze_cr.markdown({**report, "treatment": "hint"}))


class SoftReportTests(Base):
    def test_the_treatment_arm_reports_the_pair_over_sampling_minutes(self):
        report = self.run_sim(SoftSim())
        self.assertValid(report)
        soft = report["soft_timeout"]
        self.assertTrue(soft["on"])
        self.assertEqual(soft["absent"], [])
        self.assertEqual(soft["stalled_minutes"], 0)
        self.assertAlmostEqual(soft["sampling_minutes"], report["numerator"]["window"]["minutes"], places=2)
        self.assertAlmostEqual(soft["soft_per_hour"], SOFT_RATE * 3600, delta=SOFT_RATE * 3600 * 0.01)
        self.assertAlmostEqual(soft["late_per_hour"], LATE_RATE * 3600, delta=LATE_RATE * 3600 * 0.05)
        self.assertAlmostEqual(soft["late_per_soft"], LATE_RATE / SOFT_RATE, delta=0.0005)
        self.assertAlmostEqual(soft["soft_per_peer_query"], SOFT_RATE / QUERY_RATE, delta=0.002)

    def test_stalled_minutes_are_left_out(self):
        base = SoftSim()
        open_e = base.warm_min * 60  # the window opens after the warmup
        stall = (open_e + 600, open_e + 1200)  # ten minutes of the window's thirty
        report = self.run_sim(SoftSim(stall=stall))
        self.assertValid(report)
        soft = report["soft_timeout"]
        # Each stalled scrape interval goes whole: up to one interval more at each edge.
        self.assertGreaterEqual(soft["stalled_minutes"], 10)
        self.assertLessEqual(soft["stalled_minutes"], 12.1)
        self.assertAlmostEqual(soft["sampling_minutes"] + soft["stalled_minutes"],
                               report["numerator"]["window"]["minutes"], places=2)
        # The tenfold soft rate while stalled never reaches the report.
        self.assertAlmostEqual(soft["soft_per_hour"], SOFT_RATE * 3600, delta=SOFT_RATE * 3600 * 0.02)
        self.assertAlmostEqual(soft["late_per_soft"], LATE_RATE / SOFT_RATE, delta=0.0005)

    def test_the_control_arm_reports_zeros_and_no_ratio(self):
        soft = self.run_sim(SoftSim(on=False))["soft_timeout"]
        self.assertFalse(soft["on"])
        self.assertEqual((soft["soft"], soft["late"], soft["soft_per_hour"]), (0, 0, 0.0))
        self.assertIsNone(soft["late_per_soft"])  # no soft queries: no ratio, never a division by 0
        self.assertEqual(soft["soft_per_peer_query"], 0.0)

    def test_absent_series_read_absent_and_never_change_the_verdict(self):
        sim = SoftSim()
        sim.drop_series = {SOFT["soft"], SOFT["late"], SPLIT["full_seconds"]}
        report = self.run_sim(sim)
        self.assertValid(report)
        soft = report["soft_timeout"]
        self.assertEqual(soft["absent"], ["late", "sampler_full_seconds", "soft"])
        self.assertIsNone(soft["soft_per_hour"])
        self.assertIsNone(soft["late_per_soft"])
        self.assertIsNotNone(soft["peer_lookup_queries"])
        self.assertFalse(any("soft" in c["check"] for c in report["checks"]))  # nothing decided on it

    def test_other_treatments_have_no_soft_report(self):
        for treatment, winners in (("hint", [ADM]), ("split", [ADM]), ("utp", None)):
            with self.subTest(treatment=treatment):
                sim = SoftSim(treatment=treatment, on=True, winners=tuple(winners or ()))
                report = self.run_sim(sim, winners=winners)
                self.assertNotIn("soft_timeout", report)


class SoftEffectTests(unittest.TestCase):
    WIN = ([100, 112, 111, 101], [110, 99, 100, 111])

    def effect(self, treatment, **kw):
        winners = (ADM,)
        blocks = [col6.s3_block(treatment, "ABBA", self.WIN[0], winners=winners, **kw),
                  col6.s3_block(treatment, "BAAB", self.WIN[1], start=12, winners=winners, **kw)]
        return analyze_cr.analyze_effect(treatment, blocks, sorted(winners))

    def test_soft_reports_the_pair_per_arm_and_decides_on_the_numerator(self):
        soft = {"sampling_minutes": 300.0, "stalled_minutes": 60.0, "soft_per_hour": 7200.0,
                "late_per_hour": 30.0, "late_per_soft": 0.004, "soft_per_peer_query": 0.1, "absent": []}
        out = self.effect("soft", soft_timeout=soft)
        self.assertEqual(out["switch"], "soft_timeout")
        self.assertEqual(out["outcome"], self.effect("soft")["outcome"])  # the report never moves the verdict
        self.assertEqual(out["outcome"], "win")
        by_arm = out["soft_timeout_by_arm"]
        self.assertEqual((len(by_arm["A"]), len(by_arm["B"])), (4, 4))
        self.assertEqual(by_arm["B"][0]["late_per_soft"], 0.004)
        self.assertEqual(by_arm["A"][0]["stalled_minutes"], 60.0)
        self.assertNotIn("warnings", out)

    def test_other_treatments_have_no_soft_report(self):
        for treatment in ("hint", "split"):
            with self.subTest(treatment=treatment):
                self.assertNotIn("soft_timeout_by_arm", self.effect(treatment))


if __name__ == "__main__":
    unittest.main()
