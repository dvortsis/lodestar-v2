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


if __name__ == "__main__":
    unittest.main()
