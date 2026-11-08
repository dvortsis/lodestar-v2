"""The filter gates (OP-1; plan §2, §3). Pure functions over the event env.

The workflow's job-level `if:` checks the same things before any job with an
environment starts; this module repeats them in code, then parses the command
and applies the soft stop, from which `down` is exempt.
"""

from __future__ import annotations

from typing import Mapping

from command import Command, Refused, parse


class Gated(Exception):
    """The event isn't one the control plane acts on. Not logged in detail."""


def stop_engaged(env: Mapping[str, str]) -> bool:
    # Fail closed: anything but an explicit "false" (unset included) is a stop.
    return env.get("OPERATOR_STOP", "") != "false"


def _numeric(value: str | None) -> str:
    if not value or not value.isdigit():
        raise Gated("unset or non-numeric id")
    return value


def check_event(env: Mapping[str, str]) -> None:
    """Channel PR, created events only, and the operator App's numeric bot ID."""
    if env.get("EVENT_NAME") != "issue_comment" or env.get("EVENT_ACTION") != "created":
        raise Gated("not a created comment")
    if env.get("IS_PR") != "true":
        raise Gated("not a pull request")
    if _numeric(env.get("ISSUE_NUMBER")) != _numeric(env.get("CHANNEL_PR")):
        raise Gated("not the channel PR")
    if _numeric(env.get("AUTHOR_ID")) != _numeric(env.get("OPERATOR_BOT_USER_ID")):
        raise Gated("author is not the operator")


def gate(env: Mapping[str, str], limits: Mapping) -> Command:
    """Raises Gated (ignore silently) or Refused (log and answer the reason)."""
    check_event(env)
    cmd = parse(env.get("COMMENT_BODY", ""), limits["presets"], limits["steps"])
    if cmd.verb != "down" and stop_engaged(env):
        raise Refused("OPERATOR_STOP is set: only down runs")
    return cmd

