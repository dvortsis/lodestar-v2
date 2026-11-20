"""CR-COL-5: r5.1's stacked controls, the lookup-cost pair's mechanism check (A1) and
CR-LEDGER's W, q and y (docs/decisions/2026-11-20-CR-COL-5.md).

Segments are written as the Linux collector writes them, by test_analyze_cr's Sim, so the
new checks run through the same files a real run produces. The stage-1 regression compares
today's verdicts with fixtures/cr_col5_stage1_golden.json, captured from the analyzer
before this change.
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

L = collect.LEDGER
GOLDEN = HERE / "fixtures" / "cr_col5_stage1_golden.json"
NEW_SWITCHES = [s for s in collect.SWITCH_ENV if s not in collect.STAGE1_SWITCHES]


class LedgerSim(cr.Sim):
    """A segment whose binary renders CR-LEDGER's series. `raw` renders full exposition
    text (HELP and TYPE lines, a label that must not survive) and sanitizes it as the
    collector does before writing the snapshot."""

    def __init__(self, treatment="lc", pair=True, winners=("coordinated_admission",), raw=False, **kw):
        switches = {w: True for w in winners}
        switches.update(lookup_end=pair, preset_slots=pair)
        super().__init__(treatment=treatment, switches=switches, **kw)
        self.lookups_per_s, self.residence, self.queries, self.value_share = 2.0, 6.0, 12.0, 0.3
        self.occupied, self.qps, self.ledger, self.raw = 40, 150.0, True, raw
        if raw:
            self.edits.append((lambda e, i: True, lambda row, lines: lines.__setitem__(
                slice(None), collect.sanitize_snapshot("\n".join(lines)).splitlines())))

    def snapshot(self, e, row):
        lines = super().snapshot(e, row)
        if not self.ledger:
            return lines
        ended = math.floor(e * self.lookups_per_s)
        values = math.floor(ended * self.value_share)
        out = []
        if self.raw:  # as the binary renders them: Sim writes these three without their label
            labels = {"fetch_admission_waits_total": 'kind="arrival"',
                      "fetch_admission_wait_cancelled_total": 'reason="evicted"',
                      "fetch_admission_refusals_total": 'reason="refused"'}
            lines = [next((f"lodestar_{n}{{{v}}} {x.split()[1]}" for n, v in labels.items()
                           if x.startswith(f"lodestar_{n} ")), x) for x in lines]
            out += [f"# HELP lodestar_{L['ended']} Lookups ended, by reason.", f"# TYPE lodestar_{L['ended']} counter",
                    f'lodestar_{L["ended"]}{{reason="mystery"}} 4', 'lodestar_dht_peer_bytes{ip="192.0.2.1"} 5']
        for reason, n in (("values", values), ("closest", ended - values)):
            out.append(f'lodestar_{L["ended"]}{{reason="{reason}"}} {n}')
            out.append(f'lodestar_{L["residence"]}{{reason="{reason}"}} {n * self.residence:.3f}')
        out.append(f'lodestar_{L["queries"]}{{purpose="peer_lookup"}} {math.floor(ended * self.queries)}')
        out.append(f'lodestar_{L["queries"]}{{purpose="sample"}} {math.floor(e * 5)}')
        out.append(f"lodestar_dht_queries_out_total {math.floor(e * self.qps)}")
        occupied = self.occupied(e) if callable(self.occupied) else self.occupied
        out += [f"lodestar_{L['occupied']} {occupied}", f"lodestar_{L['cap']} 64",
                f"lodestar_dht_peer_lookup_seconds_total {ended * self.residence:.3f}",
                f"lodestar_dht_peer_lookups_answered_total {values}"]
        return lines + out


def write_winners(path, winners):
    path.write_text(json.dumps({"winners": list(winners)}))
    return path


class Base(cr.Base):
    def run_sim(self, sim, winners=("coordinated_admission",)):
        self.n += 1
        return analyze_cr.analyze_segment(sim.write(self.tmp / f"seg{self.n}"),
                                          None if winners is None else list(winners))


# ----------------------------------------------------------------- stage 1 doesn't change
def stage1_scenarios():
    S = cr.Sim
    out = {"utp_off": S(treatment="utp"), "utp_on": S(treatment="utp", switches={"utp": True}),
           "adm_on": S(treatment="adm", switches={"coordinated_admission": True}),
           "route_on": S(treatment="route", switches={"route_backoff": True}),
           "comb": S(kind="comb", treatment=None), "comparator": S(kind="comparator", treatment=None)}
    s = S(treatment="utp")
    s.switches["responder_hints"] = True
    out["other_switch_on"] = s
    s = S(treatment="utp", switches={"utp": True})
    s.utp_in = 0.0
    out["utp_void"] = s
    s = S(treatment="utp")
    s.api = dict(s.api, intensity="aggressive")
    out["preset_void"] = s
    return out


def verdict_lines(report):
    window = (report.get("numerator") or {}).get("window") or {}
    mean = window.get("mean_net_per_min", window.get("mean_per_min"))
    return {"verdict": report.get("verdict"), "reasons": report.get("verdict_reasons"),
            "checks": [[c["rule"], c["check"], c["ok"], c["effect"]] for c in report.get("checks", [])],
            "mean": round(mean, 6) if report.get("numerator") else None,
            "markdown_verdict": [x for x in analyze_cr.markdown(report).splitlines() if x.startswith("- Verdict")]}


def stage1_effects(legacy):
    b = cr.block
    cases = {"win": [b("ABBA", [100, 112, 111, 101]), b("BAAB", [110, 99, 100, 111], start=12)],
             "loss": [b("ABBA", [100, 80, 81, 100]), b("BAAB", [79, 101, 100, 80], start=12)],
             "inconclusive": [b("ABBA", [100, 130, 80, 100]), b("BAAB", [70, 101, 140, 120], start=12)],
             "one_block": [b("ABBA", [100, 110, 110, 100])]}
    out = {}
    for name, blocks in cases.items():
        if legacy:
            for seg in (s for blk in blocks for s in blk):
                for k in NEW_SWITCHES:
                    del seg["identity"]["switches"][k]
        e = analyze_cr.analyze_effect("utp", blocks)
        out[name] = {"outcome": e["outcome"], "valid_blocks": e["valid_blocks"],
                     "tau": round(e["fit"]["tau"], 9) if e.get("fit") else None,
                     "problems": [blk["problems"] for blk in e["blocks"]]}
    return out


class Stage1RegressionTests(Base):
    """Rule-for-rule, stage 1's verdicts are what they were before CR-COL-5, whether the
    collector records the new switches (off) or predates them."""

    def test_verdicts_match_the_golden_fixture(self):
        golden = json.loads(GOLDEN.read_text())
        for legacy in (False, True):
            with self.subTest(legacy_collector=legacy):
                got = {}
                for name, sim in stage1_scenarios().items():
                    if legacy:
                        for k in NEW_SWITCHES:
                            del sim.switches[k]
                    self.n += 1
                    got["segments/" + name] = verdict_lines(analyze_cr.analyze_segment(sim.write(self.tmp / f"g{self.n}")))
                got.update({"effects/" + k: v for k, v in stage1_effects(legacy).items()})
                self.assertEqual(got, golden)

    def test_stage1_reports_the_ledger_absent_and_takes_no_winners(self):
        report = self.run_sim(cr.Sim(treatment="utp"), winners=None)
        self.assertNotIn("winners", report)
        led = report["ledger"]["window"]
        self.assertIsNone(led["W_seconds"])
        self.assertIn("lookups_ended", led["absent"])
        self.assertIsNotNone(led["stored"])
        self.assertIn("absent: ", analyze_cr.markdown(report))
        with self.assertRaisesRegex(ValueError, "only from stage 2"):
            self.run_sim(cr.Sim(treatment="utp"), winners=[])

    def test_stage1_controls_must_match_across_blocks(self):
        blocks = [cr.block("ABBA", [100, 112, 111, 101]), cr.block("BAAB", [110, 99, 100, 111], start=12)]
        for s in blocks[1]:
            s["identity"]["switches"]["responder_hints"] = True
        out = analyze_cr.analyze_effect("utp", blocks)
        self.assertEqual(out["outcome"], "invalid")
        self.assertIn("differs between blocks", out["reason"])
        self.assertEqual(out["control_switches"], [["responder_hints", "safe_restart"], ["safe_restart"]])


# ----------------------------------------------------------------- the stacked control (A2)
class StackTests(Base):
    def test_a_stacked_treatment_arm_is_valid_and_names_its_control(self):
        report = self.run_sim(LedgerSim())
        self.assertValid(report)
        self.assertEqual(report["winners"], ["coordinated_admission"])
        self.assertTrue(report["mechanism"]["lookup_cost_pair"]["on"])
        text = analyze_cr.markdown(report)
        self.assertIn("On: coordinated_admission, lookup_end, preset_slots, safe_restart.", text)
        self.assertIn("Stacked control (r5.1 A2), earlier winners: coordinated_admission.", text)

    def test_the_control_arm_is_valid_without_the_pair_acting(self):
        sim = LedgerSim(pair=False)
        sim.occupied, sim.value_share = 20, 0.0
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertFalse(report["mechanism"]["lookup_cost_pair"]["on"])
        self.assertNotIn("lookups_end_on_values", {c["check"] for c in report["checks"]})

    def test_empty_winners_mean_the_shipped_defaults(self):
        self.assertValid(self.run_sim(LedgerSim(winners=()), winners=[]))
        report = self.run_sim(LedgerSim(winners=()), winners=["utp"])
        self.assertIn("control_is_the_stacked_winners", self.failed(report))

    def test_the_wrong_stacked_control_is_invalid(self):
        report = self.run_sim(LedgerSim(winners=("coordinated_admission", "utp")))
        self.assertEqual(report["verdict"], "invalid")
        self.assertIn("control_is_the_stacked_winners", self.failed(report))
        detail = next(c["detail"] for c in report["checks"] if c["check"] == "control_is_the_stacked_winners")
        self.assertEqual(detail, {"on": ["coordinated_admission", "utp"], "winners": ["coordinated_admission"]})

    def test_half_a_pair_is_invalid(self):
        sim = LedgerSim()
        sim.switches["preset_slots"] = False
        report = self.run_sim(sim)
        self.assertIn("treatment_switches_together", self.failed(report))
        self.assertEqual(report["verdict"], "invalid")

    def test_a_stacked_arm_needs_every_switch_recorded(self):
        sim = LedgerSim(pair=False)
        for k in NEW_SWITCHES:
            del sim.switches[k]
        self.assertIn("configuration_identity_recorded", self.failed(self.run_sim(sim)))

    def test_winners_are_required_and_checked(self):
        with self.assertRaisesRegex(ValueError, "give --winners"):
            self.run_sim(LedgerSim(), winners=None)
        with self.assertRaisesRegex(ValueError, "treatment's own"):
            self.run_sim(LedgerSim(), winners=["lookup_end"])
        for bad in ({"winners": ["utp", "utp"]}, {"winners": ["safe_restart"]}, {"winners": "utp"}, {}):
            path = self.tmp / "w.json"
            path.write_text(json.dumps(bad))
            with self.assertRaises(ValueError):
                analyze_cr.load_winners(path)
        self.assertEqual(analyze_cr.load_winners(write_winners(self.tmp / "ok.json", ["utp", "coordinated_admission"])),
                         ["coordinated_admission", "utp"])


# ----------------------------------------------------------------- A1's joint mechanism check
class LookupCostPairTests(Base):
    def test_no_lookup_ending_on_values_voids(self):
        sim = LedgerSim()
        sim.value_share = 0.0
        report = self.run_sim(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertEqual(report["verdict_reasons"], ["lookups_end_on_values"])

    def test_slots_never_above_32_voids(self):
        sim = LedgerSim()
        sim.occupied = 32
        report = self.run_sim(sim)
        self.assertEqual(report["verdict_reasons"], ["slots_exceed_32_once"])
        sim = LedgerSim()
        sim.occupied = lambda e: 33 if 1500 <= e < 1560 else 30  # once in the window is enough
        self.assertValid(self.run_sim(sim))

    def test_query_rate_over_the_preset_voids(self):
        sim = LedgerSim()
        sim.qps = 200 * 1.05 * 62 / 60 + 1
        report = self.run_sim(sim)
        self.assertEqual(report["verdict_reasons"], ["query_rate_within_preset"])
        pair = report["mechanism"]["lookup_cost_pair"]
        self.assertGreater(pair["intervals_over_rate"], 0)
        self.assertEqual(pair["preset_queries_per_second"], 200)
        sim = LedgerSim()
        sim.qps = 205.0  # within the 5 % share pings to queriers spend
        self.assertValid(self.run_sim(sim))

    def test_absent_ledger_series_are_missing_evidence_not_a_crash(self):
        sim = LedgerSim()
        sim.ledger = False
        report = self.run_sim(sim)
        self.assertEqual(report["verdict"], "invalid")
        self.assertIn("required_series_present", self.failed(report))
        self.assertIsNone(report["ledger"]["window"]["W_seconds"])


# ----------------------------------------------------------------- W, q and y
class LedgerTests(Base):
    def test_w_q_y_from_the_ledger(self):
        report = self.run_sim(LedgerSim())
        led = report["ledger"]["window"]
        self.assertEqual(led["absent"], [])
        self.assertAlmostEqual(led["W_seconds"], 6.0, places=2)
        self.assertAlmostEqual(led["q"], 12.0, places=1)
        self.assertAlmostEqual(led["y"], 1.0, delta=0.01)  # 120 stored/min over 120 lookups/min
        self.assertAlmostEqual(led["seconds_per_answered"], 20.0, delta=0.1)  # 6 s / 0.3 answered
        self.assertEqual(len(report["ledger"]["hourly"]), 3)
        self.assertIn("W 6.00 s, q 12.00, y 1.0", analyze_cr.markdown(report))

    def test_end_to_end_from_emitted_text_through_the_sanitized_snapshot(self):
        sim = LedgerSim(raw=True)
        root = sim.write(self.tmp / "raw")
        kept = (root / "metrics" / "00010.prom").read_text()
        self.assertNotIn("mystery", kept)
        self.assertNotIn("192.0.2.1", kept)
        self.assertNotIn("# ", kept)
        self.assertIn(f'lodestar_{L["ended"]}{{reason="values"}}', kept)
        self.assertIn("lodestar_dht_peer_lookups_answered_total", kept)
        out = self.tmp / "report.json"
        self.assertEqual(analyze_cr.main(["segment", str(root), "--output", str(out),
                                          "--winners", str(write_winners(self.tmp / "w.json",
                                                                         ["coordinated_admission"]))]), 0)
        report = json.loads(out.read_text())
        self.assertValid(report)
        self.assertAlmostEqual(report["ledger"]["window"]["W_seconds"], 6.0, places=2)
        self.assertEqual(analyze_cr.publication_problems(report), [])

    def test_sanitizer_keeps_ledger_series_and_histogram_edges(self):
        text = "\n".join([
            f'lodestar_{L["first_values"]}_bucket{{le="0.5"}} 3', f'lodestar_{L["first_values"]}_bucket{{le="+Inf"}} 9',
            f'lodestar_{L["first_values"]}_bucket{{le="1.2.3.4"}} 1', f'lodestar_{L["values"]}_count 4',
            f'lodestar_{L["hints"]}{{outcome="guarded"}} 2', f'lodestar_{L["queries"]}{{purpose="ping"}} 7',
            f'lodestar_{L["queries"]}{{purpose="ping",peer="x"}} 7', "lodestar_dht_peer_lookup_seconds_total 2.500"])
        kept = collect.sanitize_snapshot(text).splitlines()
        self.assertEqual(len(kept), 6)
        self.assertNotIn("1.2.3.4", "\n".join(kept))
        parsed = analyze_cr.parse_snapshot("\n".join(kept))
        self.assertEqual(analyze_cr.get(parsed, L["first_values"] + "_bucket", le="0.5"), 3)


# ----------------------------------------------------------------- blocks and the effect
def lc_seg(arm, net, start, winners=("coordinated_admission",), given=None, w=6.0):
    s = cr.seg(arm, net, start, others={k: True for k in winners})
    s["identity"]["switches"].update(utp=False, lookup_end=arm == "B", preset_slots=arm == "B")
    s.update(treatment="lc", winners=sorted(winners if given is None else given))
    s["ledger"] = {"window": {"W_seconds": w if arm == "A" else w / 3, "q": 40.0 if arm == "A" else 12.0, "y": 0.16}}
    return s


def lc_block(order, means, start=0.0, **kw):
    return [lc_seg(arm, m, start + i * (7.0 + 5 / 60), **kw) for i, (arm, m) in enumerate(zip(order, means))]


class StackedEffectTests(unittest.TestCase):
    WIN = ([100, 112, 111, 101], [110, 99, 100, 111])

    def test_a_stacked_win_prints_its_control(self):
        out = analyze_cr.analyze_effect("lc", [lc_block("ABBA", self.WIN[0]), lc_block("BAAB", self.WIN[1], start=12)],
                                        ["coordinated_admission"])
        self.assertEqual(out["outcome"], "win")
        self.assertEqual(out["switch"], "lookup_end+preset_slots")
        self.assertEqual(out["control_switches"], ["coordinated_admission", "safe_restart"])
        self.assertEqual(out["winners"], ["coordinated_admission"])
        self.assertAlmostEqual(out["ledger_by_arm"]["A"]["mean"]["W_seconds"], 6.0)
        self.assertAlmostEqual(out["ledger_by_arm"]["B"]["mean"]["q"], 12.0)
        self.assertEqual(len(out["ledger_by_arm"]["B"]["segments"]), 4)

    def test_wrong_winners_make_every_block_invalid(self):
        blocks = [lc_block("ABBA", self.WIN[0], given=["utp"]), lc_block("BAAB", self.WIN[1], start=12, given=["utp"])]
        out = analyze_cr.analyze_effect("lc", blocks, ["utp"])
        self.assertEqual(out["outcome"], "not_decidable")
        self.assertIn("control_not_the_stacked_winners", out["blocks"][0]["problems"])
        blocks = [lc_block("ABBA", self.WIN[0]), lc_block("BAAB", self.WIN[1], start=12)]
        out = analyze_cr.analyze_effect("lc", blocks, ["coordinated_admission", "utp"])
        self.assertIn("segment_judged_with_other_winners", out["blocks"][1]["problems"])

    def test_control_set_differing_between_blocks_is_invalid(self):
        blocks = [lc_block("ABBA", self.WIN[0]), lc_block("BAAB", self.WIN[1], start=12, winners=(), given=[])]
        with self.assertRaisesRegex(ValueError, "give --winners"):
            analyze_cr.analyze_effect("lc", blocks)
        for s in blocks[1]:  # recorded as judged against the same winners, but run without them
            s["winners"] = ["coordinated_admission"]
        out = analyze_cr.analyze_effect("lc", blocks, ["coordinated_admission"])
        self.assertIn("control_not_the_stacked_winners", out["blocks"][1]["problems"])
        self.assertEqual(out["outcome"], "not_decidable")

    def test_half_a_pair_splits_the_block(self):
        block = lc_block("ABBA", self.WIN[0])
        block[1]["identity"]["switches"]["preset_slots"] = False
        problems = analyze_cr.judge_block(block, "lc", ["coordinated_admission"])["problems"]
        self.assertIn("treatment_switches_split", problems)


class RehearsalTests(Base):
    """The decisions file's rehearsal: a synthetic stage-2 A/B, two blocks (ABBA, then BAAB),
    each segment written as the collector writes it and analyzed through the CLI."""

    def test_two_blocks_through_the_cli(self):
        winners = write_winners(self.tmp / "winners.json", ["coordinated_admission"])
        t0, start = cr.T0, cr.START
        self.addCleanup(setattr, cr, "T0", t0)
        self.addCleanup(setattr, cr, "START", start)
        segments, blocks, clock = [], [], t0
        for b, order in enumerate(("ABBA", "BAAB")):
            paths = []
            for i, arm in enumerate(order):
                cr.T0, cr.START = clock, clock.timestamp() - 2
                sim = LedgerSim(pair=arm == "B", rate=(133.0 if arm == "B" else 120.0) * (1 + 0.004 * ((b + i) % 3)))
                if arm == "A":
                    sim.occupied, sim.residence, sim.queries = 31, 23.0, 45.0
                root = sim.write(self.tmp / f"b{b}s{i}")
                out = self.tmp / f"b{b}s{i}.json"
                self.assertEqual(analyze_cr.main(["segment", str(root), "--output", str(out), "--winners", str(winners)]), 0)
                last = json.loads(out.read_text())["timeline"]["last_scrape_utc"]
                clock = analyze_cr.parse_utc(last) + timedelta(minutes=5)
                paths.append(str(out))
                segments.append(json.loads(out.read_text()))
            clock += timedelta(hours=10)  # BAAB about 12 h out of phase
            blocks.append(paths)
        self.assertEqual([r["verdict"] for r in segments], ["valid"] * 8, [r["verdict_reasons"] for r in segments])
        effect = self.tmp / "effect.json"
        argv = ["effect", "--treatment", "lc", "--winners", str(winners), "--output", str(effect)]
        for blk in blocks:
            argv += ["--block", *blk]
        self.assertEqual(analyze_cr.main(argv), 0)
        out = json.loads(effect.read_text())
        self.assertEqual(out["valid_blocks"], 2, [b["problems"] for b in out["blocks"]])
        self.assertEqual(out["outcome"], "win")
        self.assertEqual(out["control_switches"], ["coordinated_admission", "safe_restart"])
        self.assertAlmostEqual(out["ledger_by_arm"]["A"]["mean"]["W_seconds"], 23.0, places=1)
        self.assertAlmostEqual(out["ledger_by_arm"]["B"]["mean"]["W_seconds"], 6.0, places=1)
        self.assertGreater(out["fit"]["effect"], 0.05)
        print(f"\nrehearsal: outcome {out['outcome']}, effect {out['fit']['effect']:+.3f} "
              f"(CI {out['fit']['effect_ci'][0]:+.3f} to {out['fit']['effect_ci'][1]:+.3f}, df {out['fit']['df']}); "
              f"W A {out['ledger_by_arm']['A']['mean']['W_seconds']:.2f} s, B {out['ledger_by_arm']['B']['mean']['W_seconds']:.2f} s; "
              f"q A {out['ledger_by_arm']['A']['mean']['q']:.2f}, B {out['ledger_by_arm']['B']['mean']['q']:.2f}; "
              f"control {', '.join(out['control_switches'])}", file=sys.stderr)


if __name__ == "__main__":
    unittest.main()
