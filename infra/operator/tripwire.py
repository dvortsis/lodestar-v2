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
since the API can't say who edited it. A sweep that can't finish escalates;
it disables only when a wake session ran inside its lookback, or that can't be
read (the owner's decision, 2026-11-14). A finding attributed to the bot always
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
# an API error. Past it, the sweep is incomplete: escalated for a check by hand.
REVIEW_PAGE_BUDGET = 60  # 7 sweeps x (60 + 42 other requests) = 714, leaving ~250 of ~1,000 (decisions file)
# The sweep's own deadline, under the jobs' 10-minute timeout: a job cancelled
# for time can neither escalate nor disable, so the sweep stops itself first and
# escalates the unfinished run.
SWEEP_DEADLINE_SECONDS = 360
# An incomplete sweep disables only when a wake session ran inside its lookback
# (the owner's decision, 2026-11-14; _wake_session). A job can wait up to 30
# days for an environment's approval, so runs created that far back count.
# The escalation is posted before the check, which has its own deadline, and
# the disable follows it: a timeout can miss the disable, never the post. Past
# a list's SESSION_PAGES, past the deadline, or on any unreadable answer, a
# session counts as having run.
APPROVAL_WAIT_DAYS = 30
SESSION_PAGES = 10  # GitHub lists at most 1,000 runs for a created= filter
SESSION_DEADLINE_SECONDS = 60
REQUEST_TIMEOUT_SECONDS = 30  # infra/fleet/comments.py's urlopen timeout, per socket operation
# The runs the check reads, by the API's status filter: in flight, and every
# completed conclusion but `skipped` (every job skipped: what a comment alone
# makes). `waiting` (a wake job waiting for approval: no session yet) and
# `skipped` are only counted: all runs minus those two must equal the runs
# read, or the check can't vouch for the window (a status GitHub adds later).
RUN_STATES = ("queued", "in_progress", "pending", "requested")
RUN_CONCLUSIONS = ("success", "failure", "cancelled", "timed_out", "action_required", "startup_failure",
                   "neutral", "stale")
RUN_UNREAD = ("skipped", "waiting")
SESSION_RAN = "a wake session ran inside the sweep's lookback"
SESSION_UNKNOWN = "whether a wake session ran inside the sweep's lookback couldn't be read"
END_OF_WAKE = "this is the end-of-wake sweep"


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
        _disable(env, transport)


def _disable(env, transport) -> None:
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
    the lookback, or couldn't be read: a finding, never a silent pass.
    args: (list name, page limit or reason). `partial`: the items read before the
    stop, which the sweep still checks."""

    def __init__(self, *args, partial=None):
        super().__init__(*args)
        self.partial = [i for i in (partial or []) if isinstance(i, dict)]


def _list(transport, path: str, stop_before: datetime | None = None, key: str = "updated_at",
          max_pages: int = MAX_PAGES) -> list[dict]:
    out = []
    sep = "&" if "?" in path else "?"
    name = path.split("?")[0].rsplit("/", 1)[-1]
    for page in range(1, max_pages + 1):
        try:
            items = transport("GET", f"{path}{sep}per_page=100&page={page}", None)
        except Incomplete as e:  # the deadline: keep what was read
            raise Incomplete(*e.args, partial=out) from None
        except Exception:  # noqa: BLE001 - a failed request stops this list, never the sweep
            raise Incomplete("unreadable", f"a request for {name} failed", partial=out) from None
        if items is None:
            items = []
        if not isinstance(items, list):  # an unreadable page can't end a list as complete
            raise Incomplete("unreadable", f"a page of {name} wasn't a list", partial=out)
        items = [i for i in items if isinstance(i, dict)]
        out += items
        if len(items) < 100 or (stop_before and items and (_when(items[-1].get(key)) or stop_before) < stop_before):
            return out
    raise Incomplete(name, max_pages, partial=out)


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
        try:
            r = transport("POST", "/graphql", {"query": _summary_query(chunk), "variables": {"o": owner, "n": name}})
        except Incomplete:
            raise
        except Exception:  # noqa: BLE001 - a failed request: every PR takes the REST read
            raise Incomplete("unreadable", SUMMARY_FAILED) from None
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
SUMMARY_FAILED = "a reviews summary request failed"
# Any other error in a stage's read: recorded as a stop like the rest, so it
# can't discard findings already made or skip the stages after it.
STAGE_FAILED = "a list couldn't be read"
ITEM_FAILED = "an item in a list couldn't be checked"


def _recent_reviews(transport, repo: str, number: str, since: datetime, budget: list[int]) -> list[dict]:
    """A PR's reviews submitted in the lookback.

    The reviews list runs in creation order and can't be sorted, and a review
    created pending is submitted later, so no page can be skipped by its
    position: every page is read and each review is kept by its own
    `submitted_at`. A missing or unzoned time counts as recent (fail closed).
    Incomplete (with the reviews read so far): REVIEW_MAX_PAGES on one PR, more
    than MAX_PAGES * 100 reviews in the window, or the sweep's REVIEW_PAGE_BUDGET
    spent."""
    pages = min(REVIEW_MAX_PAGES, budget[0])  # none left: _list reads nothing and raises
    try:
        items = _list(transport, f"/repos/{repo}/pulls/{number}/reviews", max_pages=pages)
    except Incomplete as e:
        budget[0] = max(0, budget[0] - (len(e.partial) // 100 + 1))
        # Only a page-limit stop on this list means the budget ran out; a deadline
        # or an unreadable page passes through as itself.
        if e.args == ("reviews", pages) and pages < REVIEW_MAX_PAGES:
            raise Incomplete("reviews", REVIEW_PAGE_BUDGET, partial=e.partial) from None
        raise
    budget[0] -= len(items) // 100 + 1
    out = []
    for r in items:
        when = _when(r.get("submitted_at"))
        if when is None or when >= since:
            out.append(r)
    if len(out) > MAX_PAGES * 100:  # as for every other list: more than 1,000 in the window is incomplete
        raise Incomplete("reviews", MAX_PAGES, partial=out)
    return out


def sweep(env, transport, now: datetime, found: dict | None = None) -> tuple[list[str], bool]:
    """The findings and whether to disable. Each stage runs on its own: a list
    that stops part way is recorded as incomplete, the items it read are still
    checked, and the stages after it still run. So a stop anywhere can't hide a
    bot finding elsewhere. `found` (optional) collects the findings as they're
    made, for a caller that sees the sweep raise."""
    repo, bot = env["GITHUB_REPOSITORY"], _digits(env.get("OPERATOR_BOT_USER_ID"))
    if not bot:  # defence in depth: cmd_sweep escalates on this first; keep it for direct callers
        raise SystemExit("tripwire: OPERATOR_BOT_USER_ID isn't set")
    channel = _digits(env.get("CHANNEL_PR"))
    since = now - timedelta(minutes=LOOKBACK_MINUTES)
    iso = since.strftime("%Y-%m-%dT%H:%M:%SZ")
    recent = lambda i, k="created_at": (_when(i.get(k)) or now) >= since  # noqa: E731 - unparseable counts as recent
    found = {"bot": [], "other": []} if found is None else found
    bot_hits, other = found["bot"], found["other"]
    stops = found.setdefault("incomplete", [])

    def listed(path, key="updated_at"):
        try:
            return _list(transport, path, since, key)
        except Incomplete as e:
            stops.append(e)
            return e.partial
        except Exception:  # noqa: BLE001 - see STAGE_FAILED
            stops.append(Incomplete("unreadable", STAGE_FAILED))
            return []

    def each(items, check):
        """Checks every item on its own: one that can't be checked is a stop
        (ITEM_FAILED), and the items and stages after it still run."""
        for item in items:
            try:
                check(item)
            except Exception:  # noqa: BLE001
                stops.append(Incomplete("unreadable", ITEM_FAILED))

    def event(e):
        if not recent(e):
            return
        n = _digits((e.get("issue") or {}).get("number")) or "?"
        kind = str(e.get("event", ""))
        kind = kind if kind.replace("_", "").isalpha() else "event"
        if _uid(e) == bot:
            bot_hits.append(f"the operator's bot: issue event `{kind}` on #{n}")
        elif kind == "base_ref_changed":
            other.append(f"base branch changed on #{n}")
    each(listed(f"/repos/{repo}/issues/events", "created_at"), event)

    review_budget = [REVIEW_PAGE_BUDGET]
    active = []

    def pull(p):
        if not recent(p, "updated_at"):
            return
        n = _digits(p.get("number"))
        if n is None:  # its reviews can't be read: never skip them silently
            stops.append(Incomplete("unreadable", "a PR in the pulls list had no readable number"))
            return
        if _uid(p) == bot:
            bot_hits.append(f"the operator's bot opened #{n} (active in the lookback)")
        active.append(n)
    each(listed(f"/repos/{repo}/pulls?state=all&sort=updated&direction=desc"), pull)
    # No summary: every PR takes the REST read, under the budget. That read
    # records its own stops, so a summary that can't be used isn't one itself:
    # it's logged for the job's log (first-run check). The deadline still is.
    try:
        summaries = _review_summaries(transport, repo, active) if active else {}
    except Incomplete as e:
        if e.args[:1] == ("deadline",):
            stops.append(e)
        else:
            print(f"tripwire: {SUMMARY_UNREADABLE}; every PR took the REST read")
        summaries = {}
    except Exception:  # noqa: BLE001
        print(f"tripwire: {SUMMARY_FAILED}; every PR took the REST read")
        summaries = {}

    def reviews_of(n):
        reviews = summaries.get(n)
        if reviews is None:  # more reviews than the summary holds: the REST read, under the budget
            try:
                reviews = _recent_reviews(transport, repo, n, since, review_budget)
            except Incomplete as e:
                stops.append(e)
                reviews = e.partial
            except Exception:  # noqa: BLE001 - see STAGE_FAILED
                stops.append(Incomplete("unreadable", STAGE_FAILED))
                reviews = []

        def review(r):
            if _uid(r) == bot and recent(r, "submitted_at"):
                bot_hits.append(f"the operator's bot reviewed #{n}")
        each(reviews, review)
    each(active, reviews_of)

    def review_comment(c):
        n = _digits(str(c.get("pull_request_url", "")).rsplit("/", 1)[-1]) or "?"
        if _uid(c) == bot:
            bot_hits.append(f"the operator's bot wrote a review comment on #{n}")
        elif c.get("updated_at") != c.get("created_at") and recent(c, "updated_at"):
            other.append(f"a review comment on #{n} was edited")
    each(listed(f"/repos/{repo}/pulls/comments?sort=updated&direction=desc&since={iso}"), review_comment)

    def issue_comment(c):
        n = _digits(str(c.get("issue_url", "")).rsplit("/", 1)[-1]) or "?"
        if _uid(c) != bot:
            return
        if n != channel:
            bot_hits.append(f"the operator's bot commented on #{n}")
        elif c.get("updated_at") != c.get("created_at"):
            bot_hits.append("the operator's bot edited a comment on the channel PR")
        elif recent(c) and not thread.channel_shape(c.get("body")):  # its text is never echoed
            bot_hits.append("the operator's bot posted a channel comment of no allowed shape")
    each(listed(f"/repos/{repo}/issues/comments?sort=updated&direction=desc&since={iso}"), issue_comment)
    notes = list(dict.fromkeys(describe(e) for e in stops))  # one line per kind of stop, in order
    return sorted(set(bot_hits)) + sorted(set(other)) + notes, bool(bot_hits)


class _Unknown(Exception):
    """An answer the session check can't vouch for."""


def _wake_session(env, transport, now: datetime, monotonic=time.monotonic) -> str | None:
    """Why an incomplete sweep counts a wake session as having run, or None.

    Read from the Actions API with the job's own token: the runs of
    operator-wake.yml created in the last APPROVAL_WAIT_DAYS, listed by status
    on the server (RUN_STATES, RUN_CONCLUSIONS), and in each the `wake` job.
    A session counts when a `wake` job wasn't skipped, isn't waiting for
    approval, and hadn't completed before the lookback: so a queued or running
    one counts, and so does one refused or cancelled after it started (stricter
    than the rule's wording). A completed run updated before the lookback is
    passed over without reading its jobs. The first session found answers.
    Fails closed: any error, an unexpected answer, a list past SESSION_PAGES,
    counts that don't add up, or SESSION_DEADLINE_SECONDS passing counts as a
    session. The end-of-wake sweep is decided by the caller (END_OF_WAKE)."""
    call = with_deadline(transport, monotonic, SESSION_DEADLINE_SECONDS)
    since = now - timedelta(minutes=LOOKBACK_MINUTES)
    try:
        repo = env["GITHUB_REPOSITORY"]
        oldest = (now - timedelta(days=APPROVAL_WAIT_DAYS)).strftime("%Y-%m-%dT%H:%M:%SZ")
        base = f"/repos/{repo}/actions/workflows/{WAKE_WORKFLOW}/runs?created=%3E%3D{oldest}"

        def runs_page(status: str, page: int, per: int) -> tuple[list, int]:
            r = call("GET", f"{base}{'&status=' + status if status else ''}&per_page={per}&page={page}", None)
            items, total = (r or {}).get("workflow_runs"), (r or {}).get("total_count")
            if not isinstance(items, list) or not isinstance(total, int) or isinstance(total, bool) or total < 0:
                raise _Unknown
            return items, total

        def counts() -> int:  # all runs in the window, minus those the check never reads
            total = runs_page("", 1, 1)[1]
            return total - sum(runs_page(st, 1, 1)[1] for st in RUN_UNREAD)

        expected = counts()
        read = 0
        for status in RUN_STATES + RUN_CONCLUSIONS:
            key = "status" if status in RUN_STATES else "conclusion"
            seen = 0
            for page in range(1, SESSION_PAGES + 1):
                items, total = runs_page(status, page, 100)
                for run in items:
                    if not isinstance(run, dict) or _digits(run.get("id")) is None or run.get(key) != status:
                        raise _Unknown  # includes a status filter the API didn't apply
                    updated = _when(run.get("updated_at"))
                    if run.get("status") == "completed" and updated is not None and updated < since:
                        continue  # a run's update time is at or after its jobs' completion
                    if _wake_ran(call, repo, _digits(run.get("id")), since):
                        return SESSION_RAN
                seen += len(items)
                if len(items) < 100 or seen >= total:
                    break
            else:
                raise _Unknown  # more than SESSION_PAGES pages
            if seen < total:
                raise _Unknown
            read += total
        if read != expected or counts() != expected:  # a status the lists don't cover, or the window moved
            raise _Unknown
        return None
    except Exception:  # noqa: BLE001 - fail closed (the deadline raises Incomplete, an Exception)
        return SESSION_UNKNOWN


def _wake_ran(call, repo: str, rid: str, since: datetime) -> bool:
    """Whether run `rid`'s `wake` job counts as a session in the lookback."""
    seen = 0
    for page in range(1, 4):
        j = call("GET", f"/repos/{repo}/actions/runs/{rid}/jobs?filter=all&per_page=100&page={page}", None)
        jobs, count = (j or {}).get("jobs"), (j or {}).get("total_count")
        if not isinstance(jobs, list) or not isinstance(count, int) or isinstance(count, bool):
            raise _Unknown
        for job in jobs:
            if not isinstance(job, dict):
                raise _Unknown
            if job.get("name") != "wake" or job.get("conclusion") == "skipped" or job.get("status") == "waiting":
                continue
            done = _when(job.get("completed_at"))
            if job.get("completed_at") is None or done is None or done >= since:
                return True
        seen += len(jobs)
        if len(jobs) < 100 or seen >= count:
            return False
    raise _Unknown


PENDING = (f"if a wake session ran inside the sweep's lookback, or that can't be read, `{WAKE_WORKFLOW}` "
           "is disabled after this post; check whether it is")


def _incomplete(env, transport, now, findings: list[str], monotonic) -> None:
    """The owner's rule (2026-11-14) for an incomplete sweep without a bot
    finding: the escalation first, then the session check, then the disable
    if a session ran. A timeout can then miss the disable, never the post."""
    if f"/.github/workflows/{WAKE_WORKFLOW}@" in str(env.get("GITHUB_WORKFLOW_REF", "")):
        act(env, transport, findings + [f"{END_OF_WAKE}, so an incomplete sweep disables"], True)
        return
    act(env, transport, findings + [PENDING], False)
    why = _wake_session(env, transport, now, monotonic)
    if why is None:
        print("tripwire: no wake session in the lookback; not disabled")
        return
    _disable(env, transport)
    try:  # best effort: the disable is done
        if _digits(env.get("CHANNEL_PR")):
            rid = "tripwire-" + "-".join(x for x in (env.get("GITHUB_RUN_ID", ""), env.get("GITHUB_RUN_ATTEMPT", ""))
                                         if _digits(x))
            _channel(env, transport).post(message.report(
                rid + "-2", "none", "escalation",
                f"Tripwire: {why}, so an incomplete sweep disables.\n\n`{WAKE_WORKFLOW}` is disabled; "
                "re-enable it only after review."))
    except Exception:  # noqa: BLE001
        print("tripwire: couldn't post to the channel")


def with_deadline(transport, monotonic, seconds: int = SWEEP_DEADLINE_SECONDS):
    """The sweep's transport: past the deadline, the next request raises Incomplete."""
    end = monotonic() + seconds

    def call(method, path, body):
        if monotonic() >= end:
            raise Incomplete("deadline")
        return transport(method, path, body)
    return call


LISTS = ("events", "pulls", "reviews", "comments")
UNREADABLE = (tuple(f"a page of {n} wasn't a list" for n in LISTS)
              + tuple(f"a request for {n} failed" for n in LISTS)
              + ("a PR in the pulls list had no readable number", SUMMARY_UNREADABLE, SUMMARY_FAILED, STAGE_FAILED,
                 ITEM_FAILED))


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
    # A sweep that can't finish escalates, so it's checked by hand. A bot finding
    # disables; otherwise it disables only when a wake session ran inside the
    # lookback (the owner's decision, 2026-11-14; _incomplete).
    failed = None
    try:
        findings, disable = sweep(env, with_deadline(transport, monotonic), now, found)
        incomplete = bool(found.get("incomplete"))
    except Incomplete as e:  # the sweep records its own stops; this is the backstop for any it doesn't
        findings, disable, incomplete = (sorted(set(found["bot"])) + sorted(set(found["other"])) + [describe(e)],
                                         bool(found["bot"]), True)
    except Exception as e:  # noqa: BLE001 - say so (best effort), keep what was found, then fail the job
        # Findings made before the failure still count: a bot finding still disables.
        failed = f"the sweep failed ({type(e).__name__}) before it finished; check by hand"
        findings, disable, incomplete = (sorted(set(found["bot"])) + sorted(set(found["other"])) + [failed],
                                         bool(found["bot"]), True)
    if not findings:
        print("tripwire: sweep clean")
    elif incomplete and not disable:
        _incomplete(env, transport, now, findings, monotonic)
    else:
        act(env, transport, findings, disable)
    if failed:
        raise SystemExit("tripwire: the sweep failed")
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
