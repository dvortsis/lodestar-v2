"""The strict /fleet comment parser (OP-1; plan §2 "Workflow hygiene", §3).

Comment text reaches this module only through an environment variable, never a
shell. The grammar is an allow-list; anything else raises Refused with one of
the fixed reasons below, so a refusal can be logged without echoing the text.

    /fleet status
    /fleet up <preset>
    /fleet down
    /fleet run <step> <vm> [<commit>]   commit only, and always, for steps
                                        whose limits entry has takes_commit
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Mapping

VMS = ("main", "worker")
MAX_LEN = 200
HEX40 = re.compile(r"[0-9a-f]{40}\Z")
_TOKEN = re.compile(r"[a-z0-9-]{1,32}\Z")

# Every refusal reason. Logs and result comments carry only these strings.
REASONS = (
    "not a /fleet command",
    "malformed command",
    "unknown verb",
    "unknown preset",
    "unknown step",
    "unknown vm",
    "commit argument missing or malformed",
    "unexpected argument",
    "OPERATOR_STOP is set: only down runs",
    "fleet overdue or mismatched: only down runs",
)


class Refused(ValueError):
    def __init__(self, reason: str):
        assert reason in REASONS, reason
        super().__init__(reason)
        self.reason = reason


@dataclass(frozen=True)
class Command:
    verb: str
    preset: str | None = None
    step: str | None = None
    vm: str | None = None
    commit: str | None = None

    def canonical(self) -> str:
        parts = ["/fleet", self.verb]
        parts += [p for p in (self.preset, self.step, self.vm, self.commit) if p]
        return " ".join(parts)


def parse(text: str, presets: Mapping[str, object], steps: Mapping[str, Mapping]) -> Command:
    """Parse one comment body. Raises Refused for anything off the allow-list."""
    if not isinstance(text, str) or len(text) > MAX_LEN or not text.isascii():
        raise Refused("malformed command")
    # One trailing newline is tolerated (clients add one); nothing else.
    if text.endswith("\r\n"):
        text = text[:-2]
    elif text.endswith("\n"):
        text = text[:-1]
    tokens = text.split(" ")
    if not tokens or tokens[0] != "/fleet":
        raise Refused("not a /fleet command")
    args = tokens[1:]
    # Every token must be a plain lowercase word: this rejects control
    # characters, tabs, doubled or edge spaces and any other whitespace.
    for t in args:
        if not (_TOKEN.match(t) or HEX40.match(t)):
            raise Refused("malformed command")
    if not args:
        raise Refused("unknown verb")
    verb, rest = args[0], args[1:]
    if verb in ("status", "down"):
        if rest:
            raise Refused("unexpected argument")
        return Command(verb)
    if verb == "up":
        if len(rest) != 1:
            raise Refused("unexpected argument" if rest else "unknown preset")
        if rest[0] not in presets:
            raise Refused("unknown preset")
        return Command("up", preset=rest[0])
    if verb == "run":
        if len(rest) < 2:
            raise Refused("malformed command")
        step, vm, extra = rest[0], rest[1], rest[2:]
        if step not in steps:
            raise Refused("unknown step")
        if vm not in VMS:
            raise Refused("unknown vm")
        if steps[step].get("takes_commit"):
            if len(extra) != 1 or not HEX40.match(extra[0]):
                raise Refused("commit argument missing or malformed")
            return Command("run", step=step, vm=vm, commit=extra[0])
        if extra:
            raise Refused("unexpected argument")
        return Command("run", step=step, vm=vm)
    raise Refused("unknown verb")
