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
since the API can't say who edited it. A sweep that can't finish escalates
without disabling (X-operator-hardening): only a finding attributed to the bot
disables. Only numbers and fixed labels reach a log or a comment; never a
title, a body or a label name.
"""

from __future__ import annotations

import os
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "fleet"))

import message  # noqa: E402
import thread  # noqa: E402 - channel_shape: the one rule for what the bot may post

# Two hourly runs: a delayed or dropped scheduled run still leaves the next one
# covering its window. Overlaps repeat an escalation; they never hide one.
LOOKBACK_MINUTES = 130
WAKE_WORKFLOW = "operator-wake.yml"
MAX_PAGES = 10
# A PR's reviews are read whole (see _recent_reviews): from 5,000 a PR's own list
# counts as an incomplete sweep, so below 5,000 old reviews alone don't trip it.
REVIEW_MAX_PAGES = 50
# Review pages across the whole sweep: the job's token has a per-hour request
# budget, so many PRs full of old reviews can't use it up and turn a trip into
# an API error (which disables nothing). Past it, the sweep is incomplete: a trip.
REVIEW_PAGE_BUDGET = 60  # 7 sweeps x (60 + 42 other requests) = 714, leaving ~250 of ~1,000 (decisions file)
# The sweep's own deadline, under the jobs' 10-minute timeout: a job cancelled
# for time can neither escalate nor disable, so the sweep stops itself first and
# counts the unfinished run as a trip.
SWEEP_DEADLINE_SECONDS = 360


def _digits(v) -> str | None:
    v = str(v or "")
    return v if v.isascii() and v.isdigit() else None


def _uid(item) -> str | None:
    u = (item or {}).get("user") or (item or {}).get("actor") or {}
    return _digits(u.get("id")) if isinstance(u, dict) else None


def _when(s) -> datetime | None:
    """A zoned time, or None (callers count None as recent: fail closed)."""
    try:
        t = datetime.fromisoformat(str(s).replace("Z", "+00:00"))
    except ValueError:
        return None
    return t if t.tzinfo is not None else None


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
    """A list ran past its page limit (MAX_PAGES, or the reviews' own limits) before
    the lookback: a finding, never a silent pass. args: (list name, page limit)."""


def _list(transport, path: str, stop_before: datetime | None = None, key: str = "updated_at",
          max_pages: int = MAX_PAGES) -> list[dict]:
    out = []
    sep = "&" if "?" in path else "?"
    for page in range(1, max_pages + 1):
        items = transport("GET", f"{path}{sep}per_page=100&page={page}", None)
        if items is None:
            items = []
        if not isinstance(items, list):  # an unreadable page can't end a list as complete
            raise Incomplete("unreadable", f"a page of {path.split('?')[0].rsplit('/', 1)[-1]} wasn't a list")
        items = [i for i in items if isinstance(i, dict)]
        out += items
        if len(items) < 100 or (stop_before and items and (_when(items[-1].get(key)) or stop_before) < stop_before):
            return out
    raise Incomplete(path.split("?")[0].rsplit("/", 1)[-1], max_pages)


# Review summaries (X-operator-hardening): one GraphQL request covers up to
# SUMMARY_PRS PRs, each with its last SUMMARY_REVIEWS reviews and their total.
# A PR whose reviews all fit is decided from the summary and costs no REST
# review page; only a PR with more reviews than that falls back to the REST
# read below, under REVIEW_PAGE_BUDGET.
SUMMARY_PRS = 50
SUMMARY_REVIEWS = 50


def _summary_query(numbers: list[str]) -> str:
    fields = (f"p{n}:pullRequest(number:{n}){{reviews(last:{SUMMARY_REVIEWS}){{totalCount "
              "nodes{submittedAt author{__typename ... on Bot{databaseId} ... on User{databaseId}}}}}" for n in numbers)
    return "query($o:String!,$n:String!){repository(owner:$o,name:$n){" + "".join(fields) + "}}"


def _review_summaries(transport, repo: str, numbers: list[str]) -> dict[str, list[dict] | None]:
    """Each PR's reviews in the REST shape the sweep reads, or None where the
    summary doesn't hold them all (the REST read decides that PR). Anything the
    summary can't vouch for is Incomplete: an errors key, a missing PR, a total
    that isn't a count. `numbers` are digits only (checked by the caller)."""
    owner, name = repo.split("/", 1)
    out: dict[str, list[dict] | None] = {}
    for i in range(0, len(numbers), SUMMARY_PRS):
        chunk = numbers[i:i + SUMMARY_PRS]
        r = transport("POST", "/graphql", {"query": _summary_query(chunk), "variables": {"o": owner, "n": name}})
        data = (r or {}).get("data") if isinstance(r, dict) and not r.get("errors") else None
        found = (data or {}).get("repository") if isinstance(data, dict) else None
        if not isinstance(found, dict):
            raise Incomplete("unreadable", SUMMARY_UNREADABLE)
        for n in chunk:
            rv = ((found.get(f"p{n}") or {}).get("reviews")) if isinstance(found.get(f"p{n}"), dict) else None
            total, nodes = (rv or {}).get("totalCount"), (rv or {}).get("nodes")
            if not isinstance(total, int) or isinstance(total, bool) or not isinstance(nodes, list):
                raise Incomplete("unreadable", SUMMARY_UNREADABLE)
            if total > len(nodes):
                out[n] = None
                continue
            out[n] = [{"user": {"id": ((x.get("author") or {}).get("databaseId")) if isinstance(x, dict) else None},
                       "submitted_at": x.get("submittedAt") if isinstance(x, dict) else None} for x in nodes]
    return out


SUMMARY_UNREADABLE = "a reviews summary wasn't readable"


def _recent_reviews(transport, repo: str, number: str, since: datetime, budget: list[int]) -> list[dict]:
    """A PR's reviews submitted in the lookback.

    The reviews list runs in creation order and can't be sorted, and a review
    created pending is submitted later, so no page can be skipped by its
    position: every page is read and each review is kept by its own
    `submitted_at`. A missing or unzoned time counts as recent (fail closed).
    Trips (Incomplete): REVIEW_MAX_PAGES on one PR, more than MAX_PAGES * 100
    reviews in the window, or the sweep's REVIEW_PAGE_BUDGET spent."""
    pages = min(REVIEW_MAX_PAGES, budget[0])  # none left: _list reads nothing and raises, a budget trip
    try:
        items = _list(transport, f"/repos/{repo}/pulls/{number}/reviews", max_pages=pages)
    except Incomplete as e:
        # Only a page-limit stop on this list means the budget ran out; a deadline
        # or an unreadable page passes through as itself.
        if e.args == ("reviews", pages) and pages < REVIEW_MAX_PAGES:
            raise Incomplete("reviews", REVIEW_PAGE_BUDGET) from None
        raise
    budget[0] -= len(items) // 100 + 1
    out = []
    for r in items:
        when = _when(r.get("submitted_at"))
        if when is None or when >= since:
            out.append(r)
    if len(out) > MAX_PAGES * 100:  # as for every other list: more than 1,000 in the window is a trip
        raise Incomplete("reviews", MAX_PAGES)
    return out


def sweep(env, transport, now: datetime, found: dict | None = None) -> tuple[list[str], bool]:
    """The findings and whether to disable. `found` (optional) collects them as
    they're made, so a sweep that stops part way still reports, and still
    disables on, any bot finding it made before the stop."""
    repo, bot = env["GITHUB_REPOSITORY"], _digits(env.get("OPERATOR_BOT_USER_ID"))
    if not bot:  # defence in depth: cmd_sweep escalates on this first; keep it for direct callers
        raise SystemExit("tripwire: OPERATOR_BOT_USER_ID isn't set")
    channel = _digits(env.get("CHANNEL_PR"))
    since = now - timedelta(minutes=LOOKBACK_MINUTES)
    iso = since.strftime("%Y-%m-%dT%H:%M:%SZ")
    recent = lambda i, k="created_at": (_when(i.get(k)) or now) >= since  # noqa: E731 - unparseable counts as recent
    found = {"bot": [], "other": []} if found is None else found
    bot_hits, other = found["bot"], found["other"]

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

    review_budget = [REVIEW_PAGE_BUDGET]
    pulls = _list(transport, f"/repos/{repo}/pulls?state=all&sort=updated&direction=desc", since)
    active = []
    for p in pulls:
        if not recent(p, "updated_at"):
            continue
        n = _digits(p.get("number"))
        if n is None:  # its reviews can't be read: never skip them silently
            raise Incomplete("unreadable", "a PR in the pulls list had no readable number")
        if _uid(p) == bot:
            bot_hits.append(f"the operator's bot opened #{n} (active in the lookback)")
        active.append(n)
    summaries = _review_summaries(transport, repo, active) if active else {}
    for n in active:
        reviews = summaries.get(n)
        if reviews is None:  # more reviews than the summary holds: the REST read, under the budget
            reviews = _recent_reviews(transport, repo, n, since, review_budget)
        for r in reviews:
            if _uid(r) == bot and recent(r, "submitted_at"):
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
        elif not recent(c) or thread.channel_shape(c.get("body")):
            continue
        else:  # posted around the helper: no report or /fleet shape (its text is never echoed)
            bot_hits.append("the operator's bot posted a channel comment of no allowed shape")
    return sorted(set(bot_hits)) + sorted(set(other)), bool(bot_hits)


def with_deadline(transport, monotonic, seconds: int = SWEEP_DEADLINE_SECONDS):
    """The sweep's transport: past the deadline, the next request raises Incomplete."""
    end = monotonic() + seconds

    def call(method, path, body):
        if monotonic() >= end:
            raise Incomplete("deadline")
        return transport(method, path, body)
    return call


LISTS = ("events", "pulls", "reviews", "comments")
UNREADABLE = tuple(f"a page of {n} wasn't a list" for n in LISTS) + ("a PR in the pulls list had no readable number",
                                                                    SUMMARY_UNREADABLE)


def describe(e: Incomplete) -> str:
    name = e.args[0] if e.args else ""
    what = name if isinstance(name, str) and name.replace("_", "").isalpha() else "a list"
    if what == "unreadable" and len(e.args) > 1 and e.args[1] in UNREADABLE:
        return f"the sweep is incomplete: {e.args[1]}; check by hand"
    if what == "deadline":
        return f"the sweep is incomplete: it ran past its {SWEEP_DEADLINE_SECONDS // 60}-minute deadline; check by hand"
    if len(e.args) > 1 and isinstance(e.args[1], int):
        return f"the sweep is incomplete: {what} ran past {e.args[1]} pages; check by hand"
    return f"the sweep is incomplete: {what}; check by hand"


def cmd_sweep(env, transport, now, monotonic=time.monotonic) -> int:
    if not _digits(env.get("OPERATOR_BOT_USER_ID")):
        # Nothing can be attributed without the bot's ID: say so (best effort), then fail the job.
        act(env, transport, ["OPERATOR_BOT_USER_ID isn't set; nothing was checked this run"], False)
        raise SystemExit("tripwire: OPERATOR_BOT_USER_ID isn't set")
    found = {"bot": [], "other": []}
    try:
        findings, disable = sweep(env, with_deadline(transport, monotonic), now, found)
    except Incomplete as e:
        # A sweep that can't finish escalates, so it's checked by hand; it disables only
        # on a bot finding made before it stopped (X-operator-hardening, superseding the
        # runner's 2026-11-11 "an unfinished sweep is a trip": anyone could cause one).
        findings = sorted(set(found["bot"])) + sorted(set(found["other"])) + [describe(e)]
        disable = bool(found["bot"])
    except Exception as e:  # noqa: BLE001 - an API error: say so (best effort), then fail the job
        act(env, transport, [f"the sweep failed ({type(e).__name__}); nothing was checked this run"], False)
        raise SystemExit("tripwire: the sweep failed") from None
    if findings:
        act(env, transport, findings, disable)
    else:
        print("tripwire: sweep clean")
    return 0


def main(argv=None, env=None, transport=None, clock=None, monotonic=None) -> int:
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
        return cmd_sweep(env, transport, (clock or (lambda: datetime.now(timezone.utc)))(), monotonic or time.monotonic)
    raise SystemExit("usage: tripwire.py trip|sweep")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 - a traceback could carry values
        raise SystemExit(f"tripwire: internal error ({type(e).__name__})") from None
