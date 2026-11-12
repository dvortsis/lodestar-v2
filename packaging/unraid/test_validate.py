#!/usr/bin/env python3
"""Tests for validate.py. Run: cd packaging/unraid && python3 -m unittest test_validate

The shipped templates must pass, and a template that still carries a field described as
"not used yet" (the old Hot path and Cold path fields) must be refused.
"""
import re
import tempfile
import unittest
from pathlib import Path

import validate

HERE = Path(__file__).parent
HOT = (
    '  <Config Name="Hot path (optional)" Target="/data/hot" Default="" Mode="rw" '
    'Description="Planned, not used yet. Optional." Type="Path" Display="advanced" Required="false"/>\n'
)


class ValidateTemplates(unittest.TestCase):
    def check_text(self, name: str, text: str) -> list[str]:
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / name
            path.write_text(text, encoding="utf-8")
            return validate.check(path, validate.TEMPLATES[name])

    def test_the_shipped_templates_pass(self):
        for name, rules in validate.TEMPLATES.items():
            self.assertEqual(validate.check(HERE / name, rules), [], name)

    def test_no_shipped_template_maps_the_split_paths(self):
        for name in validate.TEMPLATES:
            text = (HERE / name).read_text(encoding="utf-8")
            self.assertNotIn("/data/hot", text, name)
            self.assertNotIn("/data/cold", text, name)

    def test_a_not_used_yet_field_is_refused(self):
        for name in ("lodestar.xml", "lodestar-gluetun.xml"):
            text = (HERE / name).read_text(encoding="utf-8")
            # Put the old field back, just before the PUID field.
            put_back = re.sub(r"(\s*<Config Name=\"PUID\")", "\n" + HOT.rstrip("\n") + r"\1", text, count=1)
            self.assertNotEqual(put_back, text, name)
            errors = self.check_text(name, put_back)
            self.assertTrue(any("not used yet" in e for e in errors), (name, errors))

    def test_the_refusal_ignores_letter_case(self):
        text = (HERE / "lodestar.xml").read_text(encoding="utf-8")
        shouted = text.replace('Description="Web UI and API."', 'Description="Not Used Yet."', 1)
        self.assertNotEqual(shouted, text)
        errors = self.check_text("lodestar.xml", shouted)
        self.assertTrue(any("not used yet" in e for e in errors), errors)

    def test_the_planned_peers_tcp_port_is_still_allowed(self):
        text = (HERE / "lodestar.xml").read_text(encoding="utf-8")
        self.assertIn("(planned)", text)
        self.assertEqual(self.check_text("lodestar.xml", text), [])


class RequiresAndOverview(unittest.TestCase):
    def broken(self, name: str, old: str, new: str) -> list[str]:
        text = (HERE / name).read_text(encoding="utf-8")
        self.assertIn(old, text, name)
        return ValidateTemplates.check_text(self, name, text.replace(old, new, 1))

    def test_a_missing_requires_element_is_refused(self):
        for name in validate.TEMPLATES:
            text = (HERE / name).read_text(encoding="utf-8")
            removed = re.sub(r"<Requires>.*?</Requires>\s*", "", text, flags=re.S)
            self.assertNotEqual(removed, text, name)
            errors = ValidateTemplates.check_text(self, name, removed)
            self.assertTrue(any("<Requires> element missing" in e for e in errors), (name, errors))

    def test_an_empty_gluetun_requires_is_refused(self):
        text = (HERE / "lodestar-gluetun.xml").read_text(encoding="utf-8")
        emptied = re.sub(r"<Requires>.*?</Requires>", "<Requires></Requires>", text)
        errors = ValidateTemplates.check_text(self, "lodestar-gluetun.xml", emptied)
        self.assertTrue(any("'gluetun'" in e for e in errors), errors)

    def test_a_requires_naming_another_container_is_refused(self):
        errors = self.broken("lodestar-gluetun.xml", "--network=container:gluetun", "--network=container:vpn")
        self.assertTrue(any("'vpn'" in e for e in errors), errors)

    def test_a_worker_requires_without_the_version_is_refused(self):
        errors = self.broken("lodestar-worker.xml", "Lodestar v0.6 or newer running as the main instance", "A Lodestar")
        self.assertTrue(any("v0.6" in e and "<Requires>" in e for e in errors), errors)

    def test_lodestar_may_leave_requires_empty(self):
        text = (HERE / "lodestar.xml").read_text(encoding="utf-8")
        self.assertRegex(text, r"<Requires>\s*</Requires>")

    def test_the_worker_overview_must_still_say_v0_6(self):
        errors = self.broken("lodestar-worker.xml", "Requires Lodestar v0.6+. ", "")
        self.assertTrue(any("Overview must say it requires" in e for e in errors), errors)

    def test_an_overview_whose_first_sentence_does_not_say_what_it_is_is_refused(self):
        errors = self.broken("lodestar.xml", "Lodestar crawls the BitTorrent DHT,", "Open the web UI first,")
        self.assertTrue(any("first sentence" in e for e in errors), errors)

    def test_an_empty_overview_is_refused(self):
        text = (HERE / "lodestar.xml").read_text(encoding="utf-8")
        emptied = re.sub(r"<Overview>.*?</Overview>", "<Overview></Overview>", text, flags=re.S)
        errors = ValidateTemplates.check_text(self, "lodestar.xml", emptied)
        self.assertTrue(any("<Overview> missing or empty" in e for e in errors), errors)


class EveryTemplateIsChecked(unittest.TestCase):
    def test_the_real_folder_passes_as_a_whole(self):
        self.assertEqual(validate.unlisted(HERE), [])
        self.assertEqual(validate.main(), 0)

    def test_an_unlisted_template_is_refused_by_name(self):
        with tempfile.TemporaryDirectory() as d:
            for name in validate.TEMPLATES:
                (Path(d) / name).write_text("<Container/>", encoding="utf-8")
            (Path(d) / "lodestar-new.xml").write_text("<Container/>", encoding="utf-8")
            errors = validate.unlisted(Path(d))
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("lodestar-new.xml", errors[0])


if __name__ == "__main__":
    unittest.main()
