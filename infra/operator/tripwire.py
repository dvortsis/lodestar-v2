"""The tripwire: escalate whenever the operator App's bot acts outside its lane
(OP-4; plan "Loosenings", the Pull requests write row).

    tripwire.py trip    operator-tripwire.yml, one event (pull_request_target,
                        issue_comment) whose sender is the bot
    tripwire.py sweep   on a schedule and after each bridge session: issue
                        events, PRs the bot opened or edited, reviews and
                        review comments, over the last LOOKBACK_MINUTES

The bot may do exactly one thing: post a new comment on the channel PR.
Anything else it does is a trip: an escalation on the channel PR, then
operator-wake.yml is disabled (never fleet.yml, which runs the reaper). A
base-branch change or an edited review comment by anyone is escalated too,
since the API can't say who edited it. Only numbers and fixed labels reach a
log or a comment; never a title, a body or a label name.
"""

from __future__ import annotations

import os
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "fleet"))

import message  # noqa: E402

# Two hourly runs: a delayed or dropped scheduled run still leaves the next one
# covering its window. Overlaps repeat an escalation; they never hide one.
LOOKBACK_MINUTES = 130
WAKE_WORKFLOW = "operator-wake.yml"
MAX_PAGES = 10


def _digits(v) -> str | None:
    v = str(v or "")
    return v if v.isascii() and v.isdigit() else None


def _uid(item) -> str | None:
    u = (item or {}).get("user") or (item or {}).get("actor") or {}
    return _digits(u.get("id")) if isinstance(u, dict) else None


def _when(s) -> datetime | None:
    try:
        return datetime.fromisoformat(str(s).replace("Z", "+00:00"))
    except ValueError:
        return None


def _channel(env, transport):
    from comments import Channel  # noqa: PLC0415 - infra/fleet/comments.py
    return Channel(transport, env["GITHUB_REPOSITORY"], env["CHANNEL_PR"])


def act(env, transport, findings: list[str], disable: bool) -> None:
    """Post one escalation; disable the wake workflow on a bot-attributed finding."""
    if not env.get("GITHUB_REPOSITORY"):
        raise SystemExit("tripwire: GITHUB_REPOSITORY isn't set; nothing posted or disabled")
    rid = "tripwire-" + "-".join(x for x in (env.get("GITHUB_RUN_ID", ""), env.get("GITHUB_RUN_ATTEMPT", "")) if _digits(x))
    lines = "\n".join(f"- {f}" for f in findings)
    tail = f"\n\n`{WAKE_WORKFLOW}` is disabled; re-enable it only after review." if disable else ""
    print(f"tripwire: {len(findings)} finding(s); disable={disable}")
    try:
        if _digits(env.get("CHANNEL_PR")):
            _channel(env, transport).post(message.report(rid, "none", "escalation", f"Tripwire:\n{lines}{tail}"))
    except Exception:  # noqa: BLE001 - the disable below still runs
        print("tripwire: couldn't post to the channel")
    if disable:
        try:
            transport("PUT", f"/repos/{env['GITHUB_REPOSITORY']}/actions/workflows/{WAKE_WORKFLOW}/disable", None)
            print("tripwire: operator-wake.yml disabled")
        except Exception:  # noqa: BLE001
            print("tripwire: couldn't disable operator-wake.yml; disable it by hand")
            raise SystemExit(1) from None


def cmd_trip(env, transport) -> int:
    bot, sender = _digits(env.get("OPERATOR_BOT_USER_ID")), _digits(env.get("SENDER_ID"))
    if not bot or sender != bot:
        print("tripwire: not the operator's bot")
        return 0
    event, action, number = env.get("EVENT_NAME", ""), env.get("EVENT_ACTION", ""), _digits(env.get("TARGET_NUMBER"))
    if event == "issue_comment" and action == "created" and number and number == _digits(env.get("CHANNEL_PR")):
        print("tripwire: a new comment on the channel PR")
        return 0
    label = f"{event}.{action}" if event in ("issue_comment", "pull_request_target") and action.replace("_", "").isalpha() else "event"
    act(env, transport, [f"the operator's bot did `{label}` on #{number or '?'}"], disable=True)
    return 0


class Incomplete(Exception):
    """A list ran past MAX_PAGES before the lookback: a finding, never a silent pass."""


def _list(transport, path: str, stop_before: datetime | None = None, key: str = "updated_at") -> list[dict]:
    out = []
    sep = "&" if "?" in path else "?"
    for page in range(1, MAX_PAGES + 1):
        items = transport("GET", f"{path}{sep}per_page=100&page={page}", None) or []
        items = [i for i in items if isinstance(i, dict)]
        out += items
        if len(items) < 100 or (stop_before and items and (_when(items[-1].get(key)) or stop_before) < stop_before):
            return out
    raise Incomplete(path.split("?")[0].rsplit("/", 1)[-1])


def sweep(env, transport, now: datetime) -> tuple[list[str], bool]:
    repo, bot = env["GITHUB_REPOSITORY"], _digits(env.get("OPERATOR_BOT_USER_ID"))
    if not bot:
        raise SystemExit("tripwire: OPERATOR_BOT_USER_ID isn't set")
    channel = _digits(env.get("CHANNEL_PR"))
    since = now - timedelta(minutes=LOOKBACK_MINUTES)
    iso = since.strftime("%Y-%m-%dT%H:%M:%SZ")
    recent = lambda i, k="created_at": (_when(i.get(k)) or now) >= since  # noqa: E731 - unparseable counts as recent
    bot_hits, other = [], []

    for e in _list(transport, f"/repos/{repo}/issues/events", since, "created_at"):
        if not recent(e):
            continue
        n = _digits((e.get("issue") or {}).get("number")) or "?"
        kind = str(e.get("event", ""))
        kind = kind if kind.replace("_", "").isalpha() else "event"
        if _uid(e) == bot:
            bot_hits.append(f"the operator's bot: issue event `{kind}` on #{n}")
        elif kind == "base_ref_changed":
            other.append(f"base branch changed on #{n}")

    pulls = _list(transport, f"/repos/{repo}/pulls?state=all&sort=updated&direction=desc", since)
    for p in pulls:
        if not recent(p, "updated_at"):
            continue
        n = _digits(p.get("number")) or "?"
        if _uid(p) == bot:
            bot_hits.append(f"the operator's bot opened #{n} (active in the lookback)")
        for r in _list(transport, f"/repos/{repo}/pulls/{n}/reviews") if n != "?" else []:
            if _uid(r) == bot:
                bot_hits.append(f"the operator's bot reviewed #{n}")

    for c in _list(transport, f"/repos/{repo}/pulls/comments?sort=updated&direction=desc&since={iso}", since):
        n = _digits(str(c.get("pull_request_url", "")).rsplit("/", 1)[-1]) or "?"
        if _uid(c) == bot:
            bot_hits.append(f"the operator's bot wrote a review comment on #{n}")
        elif c.get("updated_at") != c.get("created_at") and recent(c, "updated_at"):
            other.append(f"a review comment on #{n} was edited")

    for c in _list(transport, f"/repos/{repo}/issues/comments?sort=updated&direction=desc&since={iso}", since):
        n = _digits(str(c.get("issue_url", "")).rsplit("/", 1)[-1]) or "?"
        if _uid(c) != bot:
            continue
        if n != channel:
            bot_hits.append(f"the operator's bot commented on #{n}")
        elif c.get("updated_at") != c.get("created_at"):
            bot_hits.append("the operator's bot edited a comment on the channel PR")
    return sorted(set(bot_hits)) + sorted(set(other)), bool(bot_hits)


def cmd_sweep(env, transport, now) -> int:
    try:
        findings, disable = sweep(env, transport, now)
    except Incomplete as e:
        # Fail closed (the runner, 2026-11-11): a sweep we can't finish counts as a trip.
        what = str(e) if str(e).replace("_", "").isalpha() else "a list"
        findings, disable = [f"the sweep is incomplete: {what} ran past {MAX_PAGES} pages; check by hand"], True
    except Exception as e:  # noqa: BLE001 - an API error: say so (best effort), then fail the job
        act(env, transport, [f"the sweep failed ({type(e).__name__}); nothing was checked this run"], False)
        raise SystemExit("tripwire: the sweep failed") from None
    if findings:
        act(env, transport, findings, disable)
    else:
        print("tripwire: sweep clean")
    return 0


def main(argv=None, env=None, transport=None, clock=None) -> int:
    env = dict(os.environ if env is None else env)
    argv = sys.argv[1:] if argv is None else argv
    if transport is None:  # pragma: no cover - network
        from comments import urllib_transport  # noqa: PLC0415
        transport = urllib_transport(env.get("GITHUB_TOKEN", ""))
    if argv == ["trip"]:
        return cmd_trip(env, transport)
    if argv == ["sweep"]:
        if not env.get("GITHUB_REPOSITORY"):
            raise SystemExit("tripwire: GITHUB_REPOSITORY isn't set")
        return cmd_sweep(env, transport, (clock or (lambda: datetime.now(timezone.utc)))())
    raise SystemExit("usage: tripwire.py trip|sweep")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 - a traceback could carry values
        raise SystemExit(f"tripwire: internal error ({type(e).__name__})") from None
