"""The screen-rules file (OP-3b; docs/plan/2026-11-08-cloud-test-plan.md, the
cloud screen's rule 2: the rules are committed before the screen starts).

`run start-block <vm> <rules-commit>` reads RULES_PATH at that commit, never
from this checkout or a comment. The rules commit is an ancestor of main
(checked by act), so a change to the rules is a reviewed PR. The file is
JSON with exactly these keys:

    {
      "version": 1,
      "screen": "adm-1",                         # [a-z0-9-]{1,40}, the screen's name
      "lodestar_image": "ghcr.io/dvortsis/lodestar-v2@sha256:<64 hex>",
      "lodestar_commit": "<40 hex>",             # the source commit the image was built from
      "spec_commit": "<40 hex>",                 # the collector's --spec-commit (CR-SPEC in force)
      "treatment": "adm",                        # adm | utp | route
      "segment_hours": 2,                        # 2 or 3 (screen rule 3)
      "notice_version": 1                        # the Legal Notice version start-block accepts
    }

The image is pinned by digest, in the one repository the project publishes;
both commits must also be ancestors of main. Anything else refuses with a
fixed reason.
"""

from __future__ import annotations

import json
import re
import subprocess

from steps.remote import StepRefused

RULES_PATH = "infra/fleet/steps/screen-rules.json"
# The rules commit also gives the collector and the analyzer: what runs as root
# on the VM, and what judges it, are both fixed before the screen starts.
TOOLS = {"collect.py": "docs/soak/tools/linux/collect.py", "analyze_cr.py": "docs/soak/tools/analyze_cr.py"}
IMAGE = re.compile(r"ghcr\.io/dvortsis/lodestar-v2@sha256:([0-9a-f]{64})\Z")
HEX40 = re.compile(r"[0-9a-f]{40}\Z")
SCREEN = re.compile(r"[a-z0-9-]{1,40}\Z")
KEYS = {"version", "screen", "lodestar_image", "lodestar_commit", "spec_commit", "treatment", "segment_hours",
        "notice_version"}
TREATMENTS = ("adm", "utp", "route")
MAX_BYTES = 4096


def _int(v) -> bool:
    return isinstance(v, int) and not isinstance(v, bool)


def parse(text: bytes) -> dict:
    """The validated rules. Raises StepRefused."""
    bad = StepRefused("the screen rules at that commit are malformed")
    if len(text) > MAX_BYTES:
        raise bad
    try:
        doc = json.loads(text.decode("ascii"))
    except (UnicodeDecodeError, ValueError):
        raise bad from None
    if not isinstance(doc, dict) or set(doc) != KEYS or doc["version"] != 1 or not _int(doc["version"]):
        raise bad
    image = IMAGE.match(doc["lodestar_image"]) if isinstance(doc["lodestar_image"], str) else None
    ok = (image is not None
          and isinstance(doc["screen"], str) and SCREEN.match(doc["screen"])
          and all(isinstance(doc[k], str) and HEX40.match(doc[k]) for k in ("lodestar_commit", "spec_commit"))
          and doc["treatment"] in TREATMENTS
          and _int(doc["segment_hours"]) and doc["segment_hours"] in (2, 3)
          and _int(doc["notice_version"]) and 1 <= doc["notice_version"] <= 999999)
    if not ok:
        raise bad
    return dict(doc, image_digest=image.group(1))


def read(commit: str, repo: str, run=subprocess.run) -> tuple[dict, dict[str, bytes]]:
    """The rules and the tools at the rules commit."""
    def show(path: str) -> bytes:
        r = run(["git", "-C", repo, "show", f"{commit}:{path}"], capture_output=True, timeout=30)
        if r.returncode != 0:
            raise StepRefused("the rules commit has no screen rules" if path == RULES_PATH
                              else "the rules commit lacks the collector or the analyzer")
        return r.stdout
    rules = parse(show(RULES_PATH))
    return rules, {name: show(path) for name, path in TOOLS.items()}


def args(rules: dict, rules_commit: str, vm: str, block_id: str) -> dict[str, str]:
    """start-block's arguments (remote.payload checks their shape again)."""
    return {"IMAGE_DIGEST": rules["image_digest"], "LODESTAR_COMMIT": rules["lodestar_commit"],
            "SPEC_COMMIT": rules["spec_commit"], "RULES_COMMIT": rules_commit, "TREATMENT": rules["treatment"],
            "SEGMENT_HOURS": str(rules["segment_hours"]), "ROLE": vm, "NOTICE_VERSION": str(rules["notice_version"]),
            "BLOCK_ID": block_id}
