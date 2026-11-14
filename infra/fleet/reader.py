"""The thread reader the operator uses (OP-1; plan §2 "Reading the thread safely").

Returns only comments that are unedited (REST `updated_at` == `created_at`) and
whose numeric author ID is the runner's, the operator App bot's or the control
plane's. Everything else is dropped: anyone can comment on a public PR, the
author ID survives an edit, and the App can edit other authors' comments. A
comment by the operator App's bot counts only in a shape the bot may post
(channel_shape): any other is dropped too (X-operator-hardening).

    curl ...issues/<n>/comments?per_page=100&page=k ... | python3 reader.py
Input: one JSON array of REST comments, or an array of such pages.
"""

from __future__ import annotations

import json
import os
import re
import sys
from typing import Iterable, Mapping

# github-actions[bot], which posts fleet.yml's result and status comments.
CONTROL_PLANE_USER_ID = "41898282"
REPORT = "operator-report v1"
MAX_BODY = 16000
_FIELD = re.compile(r"(id|in-reply-to|kind): ([a-z0-9][a-z0-9-]{0,39})")
# A /fleet command in infra/fleet/command.py's grammar: `status`, `down`,
# `up <preset>`, `run <step> <vm> [<commit>]`. Presets and steps come from
# fleet.yml's own config, so here they're words of command.py's token shape;
# the verbs and VMs are command.py's own (test_gates_reader pins both). This
# file is fetched alone into the operator's sandbox, so it can't import it.
_WORD = re.compile(r"[a-z0-9-]{1,32}")
_COMMIT = re.compile(r"[0-9a-f]{40}")
FLEET_VERBS = ("status", "down", "up", "run")
FLEET_VMS = ("main", "worker")


def _fleet_shape(words: list[str]) -> bool:
    if not words or not all(_WORD.fullmatch(w) or _COMMIT.fullmatch(w) for w in words):
        return False
    verb, rest = words[0], words[1:]
    if verb in ("status", "down"):
        return not rest
    if verb == "up":
        return len(rest) == 1 and bool(_WORD.fullmatch(rest[0]))
    if verb == "run":
        return (len(rest) in (2, 3) and bool(_WORD.fullmatch(rest[0])) and rest[1] in FLEET_VMS
                and (len(rest) == 2 or bool(_COMMIT.fullmatch(rest[2]))))
    return False


def channel_shape(body) -> bool:
    """The only comments the operator's bot may post (check_post's rule): an
    operator-report v1 with its three header fields and a body, or a one-line
    /fleet command in the command parser's words (never free text). Shared with
    the channel helper's post check and the tripwire's sweep."""
    if not isinstance(body, str) or len(body) > MAX_BODY:
        return False
    text = body.strip()
    if text.startswith("/fleet "):
        return text.isascii() and _fleet_shape(text[len("/fleet "):].split(" "))
    lines = text.replace("\r\n", "\n").split("\n")
    if lines[0] != REPORT:
        return False
    fields, i = {}, 1
    while i < len(lines) and lines[i]:
        m = _FIELD.fullmatch(lines[i])
        if not m or m[1] in fields:
            return False
        fields[m[1]] = m[2]
        i += 1
    return (set(fields) == {"id", "in-reply-to", "kind"} and fields["kind"] in ("ack", "report", "escalation", "done")
            and bool("\n".join(lines[i + 1:]).strip()))


def allowed_ids(env: Mapping[str, str]) -> frozenset[str]:
    ids = {env.get("RUNNER_USER_ID", ""), env.get("OPERATOR_BOT_USER_ID", ""), CONTROL_PLANE_USER_ID}
    return frozenset(i for i in ids if i.isascii() and i.isdigit())


def trusted(comments: Iterable, allowed: frozenset[str], bot: str | None = None) -> list[dict]:
    """The trusted comments. With `bot` (the operator App bot's ID), a comment
    by the bot counts only in channel_shape."""
    bot = str(bot) if bot is not None and str(bot).isascii() and str(bot).isdigit() else None
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
        if bot is not None and str(uid) == bot and not channel_shape(body):
            continue
        out.append({"id": c.get("id"), "author_id": uid, "created_at": created, "body": body})
    return out


def _flatten(data) -> list:
    if isinstance(data, list) and all(isinstance(p, list) for p in data):
        return [c for page in data for c in page]
    return data if isinstance(data, list) else []


def main() -> int:
    data = json.load(sys.stdin)
    json.dump(trusted(_flatten(data), allowed_ids(os.environ), os.environ.get("OPERATOR_BOT_USER_ID")),
              sys.stdout, indent=1)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
