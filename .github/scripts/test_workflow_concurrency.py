#!/usr/bin/env python3
"""ci.yml, web.yml and e2e.yml cancel superseded runs for pull requests only.

A push to main must not cancel the main run already in progress, or the slow
Windows and macOS jobs never finish there (docs/decisions/2026-11-12-X-ci-main-signal.md).

Run: python3 .github/scripts/test_workflow_concurrency.py
"""

import re
import unittest
from pathlib import Path

WORKFLOWS = Path(__file__).resolve().parents[1] / "workflows"
PR_ONLY = "${{ github.event_name == 'pull_request' }}"


def top_level_concurrency(text):
    """Return the lines of the top-level `concurrency:` block (no indent)."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if line.rstrip() == "concurrency:":
            block = []
            for nxt in lines[i + 1 :]:
                if nxt.strip() and not nxt.startswith(" "):
                    break
                block.append(nxt)
            return block
    return None


class ConcurrencyTest(unittest.TestCase):
    def test_pull_request_only_cancellation(self):
        for name in ("ci.yml", "web.yml", "e2e.yml"):
            with self.subTest(workflow=name):
                block = top_level_concurrency((WORKFLOWS / name).read_text())
                self.assertIsNotNone(block, "no top-level concurrency block")
                values = [
                    m.group(1).strip()
                    for line in block
                    if (m := re.match(r"\s+cancel-in-progress:\s*(.+)$", line))
                ]
                self.assertEqual(values, [PR_ONLY])
                self.assertTrue(any(re.match(r"\s+group:", line) for line in block))


if __name__ == "__main__":
    unittest.main()
