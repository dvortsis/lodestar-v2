"""The channel's message formats (OP-4; plan §2 "The two-way channel").

Runner -> operator, a comment on the channel PR:

    operator-instruction v1
    id: <id>
    in-reply-to: <id>          optional
    kind: run | query
    mode: routine | triage     optional, default routine

    <body>

Operator (or the bridge) -> runner:

    operator-report v1
    id: <id>
    in-reply-to: <id>
    kind: ack | report | escalation | done

    <body>

Header lines come first, one field per line, each field at most once, then a
blank line and a non-empty body. Anything else raises Refused with a fixed
reason, so a refusal can be logged without echoing the text.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

INSTRUCTION = "operator-instruction v1"
REPORT = "operator-report v1"
MAX_LEN = 16000
ID = re.compile(r"[a-z0-9][a-z0-9-]{0,39}\Z")

REASONS = (
    "not an operator instruction",
    "malformed header",
    "unknown field",
    "duplicate field",
    "missing field",
    "malformed id",
    "unknown kind",
    "unknown mode",
    "empty body",
    "too long",
)

_KINDS = {INSTRUCTION: ("run", "query"), REPORT: ("ack", "report", "escalation", "done")}
_FIELDS = {INSTRUCTION: ("id", "in-reply-to", "kind", "mode"), REPORT: ("id", "in-reply-to", "kind")}
_REQUIRED = {INSTRUCTION: ("id", "kind"), REPORT: ("id", "in-reply-to", "kind")}


class Refused(ValueError):
    def __init__(self, reason: str):
        assert reason in REASONS, reason
        super().__init__(reason)
        self.reason = reason


@dataclass(frozen=True)
class Message:
    header: str
    id: str
    kind: str
    in_reply_to: str | None
    mode: str
    body: str


def parse(text: str, header: str = INSTRUCTION) -> Message:
    if not isinstance(text, str) or len(text) > MAX_LEN:
        raise Refused("too long")
    lines = text.replace("\r\n", "\n").split("\n")
    if lines[0] != header:
        raise Refused("not an operator instruction")
    fields: dict[str, str] = {}
    i = 1
    while i < len(lines) and lines[i] != "":
        m = re.fullmatch(r"([a-z-]+): (\S+)", lines[i])
        if not m:
            raise Refused("malformed header")
        name, value = m.groups()
        if name not in _FIELDS[header]:
            raise Refused("unknown field")
        if name in fields:
            raise Refused("duplicate field")
        fields[name] = value
        i += 1
    for name in _REQUIRED[header]:
        if name not in fields:
            raise Refused("missing field")
    for name in ("id", "in-reply-to"):
        if name in fields and not ID.match(fields[name]):
            raise Refused("malformed id")
    if fields["kind"] not in _KINDS[header]:
        raise Refused("unknown kind")
    mode = fields.get("mode", "routine")
    if mode not in ("routine", "triage"):
        raise Refused("unknown mode")
    body = "\n".join(lines[i + 1:]).strip()
    if not body:
        raise Refused("empty body")
    return Message(header, fields["id"], fields["kind"], fields.get("in-reply-to"), mode, body)


def report(id_: str, in_reply_to: str, kind: str, body: str) -> str:
    """A well-formed report; the bridge uses it for the escalations it posts."""
    text = f"{REPORT}\nid: {id_}\nin-reply-to: {in_reply_to}\nkind: {kind}\n\n{body}"
    parse(text, REPORT)  # never post a report the runner's parser would refuse
    return text
