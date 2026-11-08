"""The thread reader the operator uses (OP-1; plan §2 "Reading the thread safely").

Returns only comments that are unedited (REST `updated_at` == `created_at`) and
whose numeric author ID is the runner's, the operator App bot's or the control
plane's. Everything else is dropped: anyone can comment on a public PR, the
author ID survives an edit, and the App can edit other authors' comments.

    curl ...issues/<n>/comments?per_page=100&page=k ... | python3 reader.py
Input: one JSON array of REST comments, or an array of such pages.
"""

from __future__ import annotations

import json
import os
import sys
from typing import Iterable, Mapping

# github-actions[bot], which posts fleet.yml's result and status comments.
CONTROL_PLANE_USER_ID = "41898282"


def allowed_ids(env: Mapping[str, str]) -> frozenset[str]:
    ids = {env.get("RUNNER_USER_ID", ""), env.get("OPERATOR_BOT_USER_ID", ""), CONTROL_PLANE_USER_ID}
    return frozenset(i for i in ids if i.isdigit())


def trusted(comments: Iterable, allowed: frozenset[str]) -> list[dict]:
    out = []
    for c in comments:
        if not isinstance(c, dict):
            continue
        user = c.get("user")
        uid = user.get("id") if isinstance(user, dict) else None
        created, updated, body = c.get("created_at"), c.get("updated_at"), c.get("body")
        if isinstance(uid, bool) or not isinstance(uid, int) or str(uid) not in allowed:
            continue
        if not isinstance(created, str) or not created or created != updated:
            continue
        if not isinstance(body, str):
            continue
        out.append({"id": c.get("id"), "author_id": uid, "created_at": created, "body": body})
    return out


def _flatten(data) -> list:
    if isinstance(data, list) and all(isinstance(p, list) for p in data):
        return [c for page in data for c in page]
    return data if isinstance(data, list) else []


def main() -> int:
    data = json.load(sys.stdin)
    json.dump(trusted(_flatten(data), allowed_ids(os.environ)), sys.stdout, indent=1)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
