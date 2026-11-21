"""CR-COL-6: stage 3's treatments, CR-HINT-FIRST (`--treatment hint`) and
X-sampler-cooldown-split (`--treatment split`), their mechanism checks and rule 6.4's
sampler report (docs/decisions/2026-11-21-CR-COL-6.md).

Segments are written as the Linux collector writes them, by test_analyze_cr's Sim, so the
new checks run through the same files a real run produces.
"""

import json
import math
import sys
import unittest
from datetime import timedelta
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE / "linux"))

import analyze_cr  # noqa: E402
import collect  # noqa: E402
import test_analyze_cr as cr  # noqa: E402
import test_analyze_cr_col5 as col5  # noqa: E402

H, S = collect.HINT, collect.SPLIT
ADM = "coordinated_admission"
# Stage 3's control, as the runner expects it: CR-ADM and the lookup-cost pair won before.
STAGE3_WINNERS = (ADM, "lookup_end", "preset_slots")


class Stage3Sim(cr.Sim):
    """A segment whose binary renders CR-HINT-FIRST's and X-sampler-cooldown-split's series.
    `on` is the treatment's own switch (the B arm); the winners are on in both arms."""

    def __init__(self, treatment="hint", on=True, winners=(ADM,), **kw):
        switches = {w: True for w in winners}
        switches[collect.TREATMENT_SWITCHES[treatment][0]] = on
        super().__init__(treatment=treatment, switches=switches, **kw)
        hint_on = bool(self.switches["hint_first"])
        # Per second: marks, keys taken, marks ignored, first lookups sent.
        self.marks, self.keys, self.ignored, self.lookups = (1.0, 1.0, 0.1, 0.8) if hint_on else (0, 0, 0, 0)
        split_on = bool(self.switches["sampler_cooldown_split"])
        self.table = lambda e: collect.SAMPLER_TABLE  # the table is full throughout
        self.spill = (lambda e: 4096) if split_on else (lambda e: 0)
        self.full_rate = 0.02 if split_on else 0.4  # sampler-full seconds per second
        self.render_hint, self.render_split = True, True
        self.drop_series = set()  # series names left out of every scrape

    def snapshot(self, e, row):
        lines = super().snapshot(e, row)
        out = []
        add = lambda name, value, labels="": out.append(f"lodestar_{name}{labels} {value}")
        if self.render_hint:
            add(H["marked"], math.floor(e * self.marks))
            for r in sorted(collect.HINT_SKIPPED_REASONS):
                add(H["skipped"], math.floor(e * self.marks / 4), f'{{reason="{r}"}}')
            add(H["keys"], math.floor(e * self.keys))
            add(H["ignored"], math.floor(e * self.ignored))
            add(H["waits"], math.floor(e * self.lookups / 2))
            for r in sorted(collect.HINT_LEFT_REASONS):
                add(H["left"], math.floor(e * self.lookups / 10), f'{{reason="{r}"}}')
            add(H["lookups"], math.floor(e * self.lookups))
            add(H["waiting"], 12 if self.lookups else 0)
            if self.switches[ADM]:  # T1's grants, each returned as any grant is
                hinted = math.floor(e * self.lookups / 2)
                add("fetch_admission_grants_total", hinted, f'{{kind="arrival",class="{collect.HINT_GRANT_CLASS}"}}')
                add("dht_peer_permit_returns_total", hinted, '{boundary="cancelled_queued"}')
        if self.render_split:
            add(S["full_seconds"], f"{e * self.full_rate:.3f}")
            add(S["spill"], self.spill(e))
            add("dht_sample_cooling", self.table(e) - 10)
            add("dht_sample_inflight", 10)
        out = [x for x in out if x.split("{")[0].split(" ")[0][len("lodestar_"):] not in self.drop_series]
        return lines + out


class Base(cr.Base):
    def run_sim(self, sim, winners=(ADM,)):
        self.n += 1
        return analyze_cr.analyze_segment(sim.write(self.tmp / f"seg{self.n}"),
                                          None if winners is None else list(winners))

    def check(self, report, name):
        return next((c for c in report["checks"] if c["check"] == name), None)


# ----------------------------------------------------------------- the treatments
class TreatmentTests(Base):
    def test_both_are_stacked_treatments_with_their_own_switch(self):
        self.assertEqual(collect.TREATMENT_SWITCHES["hint"], ("hint_first",))
        self.assertEqual(collect.TREATMENT_SWITCHES["split"], ("sampler_cooldown_split",))
        self.assertTrue({"hint", "split"} <= collect.STACKED)
        for switch in ("hint_first", "sampler_cooldown_split"):
            self.assertIn(switch, collect.SWITCH_ENV)  # recorded since CR-COL-5

    def test_winners_are_required_and_never_include_the_treatments_own_switch(self):
        for treatment, own in (("hint", "hint_first"), ("split", "sampler_cooldown_split")):
            with self.subTest(treatment=treatment):
                with self.assertRaisesRegex(ValueError, "give --winners"):
                    self.run_sim(Stage3Sim(treatment), winners=None)
                with self.assertRaisesRegex(ValueError, "treatment's own"):
                    self.run_sim(Stage3Sim(treatment), winners=[ADM, own])

    def test_the_cli_takes_both_treatments(self):
        winners = col5.write_winners(self.tmp / "w.json", [ADM])
        for treatment in ("hint", "split"):
            root = Stage3Sim(treatment).write(self.tmp / f"cli-{treatment}")
            out = self.tmp / f"{treatment}.json"
            self.assertEqual(analyze_cr.main(["segment", str(root), "--output", str(out), "--winners", str(winners)]), 0)
            self.assertValid(json.loads(out.read_text()))


# ----------------------------------------------------------------- CR-HINT-FIRST
class HintFirstTests(Base):
    def test_the_treatment_arm_marks_and_looks_up(self):
        report = self.run_sim(Stage3Sim("hint"))
        self.assertValid(report)
        hint = report["hint_first"]
        self.assertTrue(hint["on"])
        self.assertGreater(hint["marked"], 0)
        self.assertGreater(hint["lookups"], 0)
        self.assertLessEqual(hint["ignored"], hint["keys"])
        self.assertGreater(hint["hinted_grants"], 0)
        self.assertEqual(hint["absent"], [])
        self.assertEqual(set(hint["skipped"]), collect.HINT_SKIPPED_REASONS)
        for name in ("hint_first_marks_hashes", "hint_first_lookups_sent", "hint_ignored_within_keys"):
            self.assertTrue(self.check(report, name)["ok"], name)
            self.assertEqual(self.check(report, name)["rule"], "5")

    def test_no_marks_voids(self):
        sim = Stage3Sim("hint")
        sim.marks = 0
        report = self.run_sim(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertEqual(report["verdict_reasons"], ["hint_first_marks_hashes"])

    def test_marks_without_lookups_voids(self):
        sim = Stage3Sim("hint")
        sim.lookups = 0
        report = self.run_sim(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertEqual(report["verdict_reasons"], ["hint_first_lookups_sent"])

    def test_more_ignored_than_taken_voids_and_says_why(self):
        sim = Stage3Sim("hint")
        sim.ignored = 2.0
        report = self.run_sim(sim)
        self.assertEqual(report["verdict_reasons"], ["hint_ignored_within_keys"])
        detail = self.check(report, "hint_ignored_within_keys")["detail"]
        self.assertGreater(detail["ignored"], detail["keys"])
        self.assertIn("unexpected source", detail["note"])

    def test_as_many_ignored_as_taken_is_within(self):
        sim = Stage3Sim("hint")
        sim.ignored = sim.keys
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertEqual(report["hint_first"]["ignored"], report["hint_first"]["keys"])
        self.assertIn("## Hint-first", analyze_cr.markdown(report))

    def test_the_control_arm_is_not_judged(self):
        report = self.run_sim(Stage3Sim("hint", on=False))
        self.assertValid(report)
        self.assertFalse(report["hint_first"]["on"])
        self.assertEqual(report["hint_first"]["marked"], 0)
        self.assertIsNone(self.check(report, "hint_first_marks_hashes"))

    def test_missing_hint_series_in_either_arm_is_missing_evidence(self):
        for on in (True, False):
            for dropped in ({H["marked"]}, {H["lookups"], H["waiting"]}, {H["left"]}):
                with self.subTest(arm="B" if on else "A", dropped=sorted(dropped)):
                    sim = Stage3Sim("hint", on=on)
                    sim.drop_series = dropped
                    report = self.run_sim(sim)
                    self.assertEqual(report["verdict"], "invalid")
                    self.assertIn("required_series_present", self.failed(report))
                    self.assertTrue(set(report["hint_first"]["absent"]))

    def test_hinted_grants_are_required_only_under_coordinated_admission(self):
        sim = Stage3Sim("hint")
        sim.edits.append((lambda e, i: True, lambda row, lines: lines.__setitem__(
            slice(None), [x for x in lines if 'class="hinted"' not in x])))
        report = self.run_sim(sim)
        self.assertIn("required_series_present", self.failed(report))
        absent = self.check(report, "required_series_present")["detail"]["absent"]
        self.assertIn("fetch_admission_grants_total{'class': 'hinted'}", absent)
        report = self.run_sim(Stage3Sim("hint", winners=()), winners=[])
        self.assertValid(report)
        self.assertIn("hinted_grants", report["hint_first"]["absent"])


# ----------------------------------------------------------------- X-sampler-cooldown-split
class SamplerSplitTests(Base):
    def test_spill_slots_once_the_table_fills(self):
        report = self.run_sim(Stage3Sim("split"))
        self.assertValid(report)
        split = report["sampler_split"]
        self.assertEqual(split["spill_check"], "spilled")
        self.assertEqual(split["max_spill_slots"], 4096)
        self.assertEqual(split["max_table_entries"], collect.SAMPLER_TABLE)
        self.assertTrue(self.check(report, "spill_slots_used_once_table_fills")["ok"])

    def test_a_full_table_without_spill_voids(self):
        sim = Stage3Sim("split")
        sim.spill = lambda e: 0
        report = self.run_sim(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertEqual(report["verdict_reasons"], ["spill_slots_used_once_table_fills"])
        self.assertEqual(report["sampler_split"]["spill_check"], "table_full_without_spill")

    def test_a_table_that_fills_mid_window_without_spill_voids(self):
        sim = Stage3Sim("split")
        sim.table = lambda e: collect.SAMPLER_TABLE if e >= 1500 else 150_000  # mid-window
        sim.spill = lambda e: 0  # the table fills mid-window and the filter stays empty
        report = self.run_sim(sim)
        self.assertEqual(report["verdict_reasons"], ["spill_slots_used_once_table_fills"])
        self.assertGreater(report["sampler_split"]["table_filled_after_seconds"], 0)

    def test_a_table_that_never_fills_is_not_exercised_not_a_failure(self):
        sim = Stage3Sim("split")
        sim.table = lambda e: 120_000
        sim.spill = lambda e: 0
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertEqual(report["sampler_split"]["spill_check"], "not_exercised")
        self.assertIsNone(self.check(report, "spill_slots_used_once_table_fills"))

    def test_a_table_first_full_at_the_last_scrape_is_not_exercised(self):
        sim = Stage3Sim("split")
        last = sim.warm_min * 60 + sim.hours * sim.hour_s  # the window's end scrape, in elapsed seconds
        sim.table = lambda e: collect.SAMPLER_TABLE if e >= last else 150_000  # full at the last scrape only
        sim.spill = lambda e: 0
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertEqual(report["sampler_split"]["spill_check"], "not_exercised")
        self.assertIsNotNone(report["sampler_split"]["table_filled_after_seconds"])

    def test_spill_slots_alone_show_the_table_filled(self):
        sim = Stage3Sim("split")
        sim.table = lambda e: collect.SAMPLER_TABLE - 3  # read just below at every scrape
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertEqual(report["sampler_split"]["spill_check"], "spilled")

    def test_the_control_arm_is_not_judged(self):
        report = self.run_sim(Stage3Sim("split", on=False))
        self.assertValid(report)
        self.assertFalse(report["sampler_split"]["on"])
        self.assertNotIn("spill_check", report["sampler_split"])

    def test_rule_6_4_report_in_both_arms(self):
        for on in (True, False):
            with self.subTest(arm="B" if on else "A"):
                split = self.run_sim(Stage3Sim("split", on=on))["sampler_split"]
                self.assertGreater(split["sampler_full_seconds"], 0)
                worst = split["worst_hours_stored_per_min"]
                self.assertEqual(len(worst), 3)
                self.assertEqual(sorted(worst, key=lambda h: h["stored_per_min"]), worst)
                self.assertEqual({"hour", "stored_per_min"}, set(worst[0]))
        off, on = (self.run_sim(Stage3Sim("split", on=x))["sampler_split"]["sampler_full_seconds"] for x in (False, True))
        self.assertGreater(off, on)  # as written: the split spends less time full
        report = self.run_sim(Stage3Sim("split"))
        gate = report["sampler_split"]["restart_gate"]
        self.assertEqual(gate["suppression_reason_at_open"], report["startup"]["suppression_reason_at_open"])
        self.assertIn("suppressed_seconds", gate)
        self.assertIn("## Sampler split", analyze_cr.markdown(report))
        self.assertIn("spilled; sampler-full", analyze_cr.markdown(report))

    def test_missing_split_series_in_either_arm_is_missing_evidence(self):
        for on in (True, False):
            for dropped in ({S["full_seconds"]}, {S["spill"]}, {"dht_sample_cooling"}):
                with self.subTest(arm="B" if on else "A", dropped=sorted(dropped)):
                    sim = Stage3Sim("split", on=on)
                    sim.drop_series = dropped
                    report = self.run_sim(sim)
                    self.assertEqual(report["verdict"], "invalid")
                    self.assertIn("required_series_present", self.failed(report))


# ----------------------------------------------------------------- every other run
class ElsewhereTests(Base):
    def test_other_treatments_list_the_stage3_series_absent_and_never_crash(self):
        report = self.run_sim(cr.Sim(treatment="utp"), winners=None)
        self.assertNotIn("winners", report)
        self.assertIn("marked", report["hint_first"]["absent"])
        self.assertIn("hinted_grants", report["hint_first"]["absent"])
        self.assertEqual(report["sampler_split"]["absent"], ["sampler_full_seconds", "spill_slots", "table_entries"])
        self.assertIsNone(self.check(report, "hint_first_marks_hashes"))
        self.assertIsNone(self.check(report, "spill_slots_used_once_table_fills"))

    def test_an_lc_segment_reports_them_without_judging_them(self):
        sim = col5.LedgerSim()
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertTrue(report["hint_first"]["absent"])
        self.assertNotIn("spill_check", report["sampler_split"])

    def test_a_hint_segment_reports_the_split_series_and_the_reverse(self):
        hint = self.run_sim(Stage3Sim("hint"))
        self.assertEqual(hint["sampler_split"]["absent"], [])
        self.assertNotIn("spill_check", hint["sampler_split"])
        split = self.run_sim(Stage3Sim("split"))
        self.assertEqual(split["hint_first"]["absent"], [])
        self.assertIsNone(self.check(split, "hint_first_marks_hashes"))


# ----------------------------------------------------------------- the collector's allowlist
class AllowlistTests(unittest.TestCase):
    def test_the_sanitizer_keeps_stage3_series_and_drops_unknown_labels(self):
        text = "\n".join([
            f"lodestar_{H['marked']} 5", f'lodestar_{H["skipped"]}{{reason="responder"}} 2',
            f'lodestar_{H["skipped"]}{{reason="mystery"}} 2', f'lodestar_{H["left"]}{{reason="aged"}} 1',
            f'lodestar_{H["left"]}{{reason="full",peer="x"}} 1', f"lodestar_{H['waiting']} 3",
            f'lodestar_{H["keys"]}{{ip="192.0.2.1"}} 4',
            'lodestar_fetch_admission_grants_total{kind="arrival",class="hinted"} 7',
            'lodestar_fetch_admission_grants_total{kind="arrival",class="vip"} 7',
            f"lodestar_{S['full_seconds']} 12.500", f"lodestar_{S['spill']} 4096"])
        kept = collect.sanitize_snapshot(text).splitlines()
        self.assertEqual(kept, [
            f"lodestar_{H['marked']} 5", f'lodestar_{H["skipped"]}{{reason="responder"}} 2',
            f'lodestar_{H["left"]}{{reason="aged"}} 1', f"lodestar_{H['waiting']} 3",
            'lodestar_fetch_admission_grants_total{kind="arrival",class="hinted"} 7',
            f"lodestar_{S['full_seconds']} 12.500", f"lodestar_{S['spill']} 4096"])
        parsed = analyze_cr.parse_snapshot("\n".join(kept))
        self.assertEqual(analyze_cr.get(parsed, "fetch_admission_grants_total", **{"class": "hinted"}), 7)


# ----------------------------------------------------------------- blocks and the effect
def s3_seg(treatment, arm, net, start, winners=STAGE3_WINNERS, **report):
    s = cr.seg(arm, net, start, others={k: True for k in winners})
    s["identity"]["switches"]["utp"] = "utp" in winners
    s["identity"]["switches"][collect.TREATMENT_SWITCHES[treatment][0]] = arm == "B"
    s.update(treatment=treatment, winners=sorted(winners), **report)
    return s


def s3_block(treatment, order, means, start=0.0, **kw):
    return [s3_seg(treatment, arm, m, start + i * (7.0 + 5 / 60), **kw) for i, (arm, m) in enumerate(zip(order, means))]


class Stage3EffectTests(unittest.TestCase):
    WIN = ([100, 112, 111, 101], [110, 99, 100, 111])

    def effect(self, treatment, winners=STAGE3_WINNERS, **kw):
        blocks = [s3_block(treatment, "ABBA", self.WIN[0], winners=winners, **kw),
                  s3_block(treatment, "BAAB", self.WIN[1], start=12, winners=winners, **kw)]
        return analyze_cr.analyze_effect(treatment, blocks, sorted(winners))

    def test_a_hint_win_with_cr_adm_has_no_warning(self):
        out = self.effect("hint", hint_first={"marked": 9, "keys": 9, "ignored": 0, "lookups": 7})
        self.assertEqual(out["outcome"], "win")
        self.assertEqual(out["switch"], "hint_first")
        self.assertNotIn("warnings", out)
        self.assertEqual(out["control_switches"], sorted(STAGE3_WINNERS + ("safe_restart",)))
        self.assertEqual(len(out["hint_first_by_arm"]["B"]), 4)
        self.assertEqual(out["hint_first_by_arm"]["B"][0]["lookups"], 7)

    def test_a_hint_control_without_cr_adm_warns_and_keeps_its_verdict(self):
        with_adm = self.effect("hint")
        without = self.effect("hint", winners=("lookup_end", "preset_slots"))
        self.assertEqual(without["outcome"], with_adm["outcome"])
        self.assertEqual(without["fit"]["tau"], with_adm["fit"]["tau"])
        self.assertEqual(without["warnings"], [analyze_cr.HINT_WITHOUT_ADM])
        self.assertIn("coordinated admission", without["warnings"][0])

    def test_split_reports_rule_6_4_per_arm_and_decides_on_the_numerator(self):
        sampler = {"sampler_full_seconds": 120.0, "max_spill_slots": 4096,
                   "worst_hours_stored_per_min": [{"hour": 2, "stored_per_min": 90.0}]}
        out = self.effect("split", sampler_split=sampler)
        self.assertEqual(out["outcome"], "win")
        self.assertNotIn("warnings", out)
        self.assertEqual(out["sampler_split_by_arm"]["A"][0]["sampler_full_seconds"], 120.0)
        self.assertEqual(len(out["sampler_split_by_arm"]["B"]), 4)
        self.assertNotIn("hint_first_by_arm", out)
        self.assertEqual(out["split_not_exercised_b_segments"], 0)

    def test_split_never_exercised_in_any_treatment_segment_warns(self):
        idle = {"spill_check": "not_exercised", "sampler_full_seconds": 0.0, "max_spill_slots": 0,
                "worst_hours_stored_per_min": []}
        out = self.effect("split", sampler_split=idle)
        self.assertEqual(out["split_not_exercised_b_segments"], 4)
        self.assertEqual(out["warnings"], [analyze_cr.SPLIT_NOT_EXERCISED])
        self.assertEqual(out["outcome"], self.effect("split")["outcome"])  # the verdict stands
        self.assertEqual(out["sampler_split_by_arm"]["B"][0]["spill_check"], "not_exercised")


class RehearsalTests(Base):
    """The decisions file's rehearsal: a synthetic stage 3, two blocks (ABBA, then BAAB) for
    each treatment against the CR-ADM and lookup-cost pair winners, every segment written as
    the collector writes it and analyzed through the CLI."""

    def run_treatment(self, treatment):
        winners = col5.write_winners(self.tmp / f"winners-{treatment}.json", list(STAGE3_WINNERS))
        t0, start = cr.T0, cr.START
        self.addCleanup(setattr, cr, "T0", t0)
        self.addCleanup(setattr, cr, "START", start)
        segments, blocks, clock = [], [], t0
        for b, order in enumerate(("ABBA", "BAAB")):
            paths = []
            for i, arm in enumerate(order):
                cr.T0, cr.START = clock, clock.timestamp() - 2
                sim = Stage3Sim(treatment, on=arm == "B", winners=STAGE3_WINNERS,
                                rate=(128.0 if arm == "B" else 120.0) * (1 + 0.004 * ((b + i) % 3)))
                root = sim.write(self.tmp / f"{treatment}-b{b}s{i}")
                out = self.tmp / f"{treatment}-b{b}s{i}.json"
                self.assertEqual(analyze_cr.main(["segment", str(root), "--output", str(out), "--winners", str(winners)]), 0)
                last = json.loads(out.read_text())["timeline"]["last_scrape_utc"]
                clock = analyze_cr.parse_utc(last) + timedelta(minutes=5)
                paths.append(str(out))
                segments.append(json.loads(out.read_text()))
            clock += timedelta(hours=10)  # BAAB about 12 h out of phase
            blocks.append(paths)
        self.assertEqual([r["verdict"] for r in segments], ["valid"] * 8, [r["verdict_reasons"] for r in segments])
        effect = self.tmp / f"effect-{treatment}.json"
        argv = ["effect", "--treatment", treatment, "--winners", str(winners), "--output", str(effect)]
        for blk in blocks:
            argv += ["--block", *blk]
        self.assertEqual(analyze_cr.main(argv), 0)
        out = json.loads(effect.read_text())
        self.assertEqual(out["valid_blocks"], 2, [b["problems"] for b in out["blocks"]])
        self.assertEqual(out["control_switches"], sorted(STAGE3_WINNERS + ("safe_restart",)))
        return out

    def test_two_blocks_of_each_treatment_through_the_cli(self):
        hint, split = self.run_treatment("hint"), self.run_treatment("split")
        for name, out in (("hint", hint), ("split", split)):
            self.assertEqual(out["outcome"], "win")
            self.assertNotIn("warnings", out)
            print(f"\nrehearsal {name}: outcome {out['outcome']}, effect {out['fit']['effect']:+.3f} "
                  f"(CI {out['fit']['effect_ci'][0]:+.3f} to {out['fit']['effect_ci'][1]:+.3f}, df {out['fit']['df']}); "
                  f"control {', '.join(out['control_switches'])}", file=sys.stderr)
        self.assertGreater(min(s["lookups"] for s in hint["hint_first_by_arm"]["B"]), 0)
        self.assertEqual({s["lookups"] for s in hint["hint_first_by_arm"]["A"]}, {0})
        full = split["sampler_split_by_arm"]
        self.assertGreater(min(s["sampler_full_seconds"] for s in full["A"]),
                           max(s["sampler_full_seconds"] for s in full["B"]))
        print(f"rehearsal split: sampler-full seconds A {[round(s['sampler_full_seconds']) for s in full['A']]}, "
              f"B {[round(s['sampler_full_seconds']) for s in full['B']]}", file=sys.stderr)


if __name__ == "__main__":
    unittest.main()
