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
SWEEP_DEADLINE_SECONDS = 300  # with the session check's, leaves 60 s of the job's timeout (budget test)
# An incomplete sweep disables only when a wake session ran inside its lookback
# (the owner's decision, 2026-11-14; _wake_session). A job can wait up to 30
# days for an environment's approval, so runs in flight created that far back
# are read; completed runs are read from COMPLETED_EXTRA_MINUTES before the
# lookback. A wake whose run is older than every later sweep's completed
# window could see refuses to start a session (wake_run_too_old, in wake.py),
# so no completed session falls outside it.
# The escalation is posted before the check, which has its own deadline, and
# the disable follows it: a timeout can miss the disable, never the post. Past
# a list's SESSION_PAGES, past the deadline, or on any unreadable answer, a
# session counts as having run.
APPROVAL_WAIT_DAYS = 30
COMPLETED_EXTRA_MINUTES = 72 * 60
WAKE_JOB_MINUTES = 45  # operator-wake.yml's wake job timeout (test_workflows pins it)
SESSION_PAGES = 10  # GitHub lists at most 1,000 runs for a created= filter
SESSION_DEADLINE_SECONDS = 45
REQUEST_TIMEOUT_SECONDS = 30  # infra/fleet/comments.py's urlopen timeout, per socket operation
# The runs in flight, by the API's status filter (`waiting` included: a re-run
# waiting for approval still holds its earlier attempts' jobs). Their number is
# all runs minus `completed` ones; the lists must account for every one, or the
# check can't vouch for the window (a status GitHub adds later). Completed runs
# are listed by conclusion, never `skipped` (every job skipped: what a comment
# alone makes, so any number of comments costs nothing): the common ones first,
# the rest only while `completed` minus `skipped` runs are still unread.
# Every list and count is of issue_comment runs, the event the sessions run on.
RUN_STATES = ("in_progress", "waiting", "queued", "pending", "requested")
DONE_FIRST = ("success", "failure", "cancelled")
DONE_REST = ("timed_out", "action_required", "startup_failure", "neutral", "stale")
# A comment's run moves from in flight to skipped within seconds: counts that
# don't add up are read again, up to SESSION_ATTEMPTS times SETTLE_SECONDS
# apart, all inside the check's deadline.
SESSION_ATTEMPTS = 3
SETTLE_SECONDS = 4
# GitHub may cap a created= list's total_count at 1,000. A capped total can't
# exceed it, so only a total of exactly COUNT_CAP is split in two and counted
# again, up to COUNT_SPLITS deep; any other total is real and used as it is.
# The first run verifies whether the cap exists.
COUNT_CAP = 1000
COUNT_SPLITS = 6
# Requests the check may make in all, its retries included: past it, it counts
# as a session (test_the_budget_leaves_room_for_the_other_workflows).
SESSION_REQUESTS = 40
# A run in flight whose gate job hasn't finished is read again, GATE_READS
# times GATE_WAIT_SECONDS apart, all inside the check's deadline.
GATE_READS = 5
GATE_WAIT_SECONDS = 2
SESSION_RAN = "a wake session ran inside the sweep's lookback"
SESSION_UNKNOWN = "whether a wake session ran inside the sweep's lookback couldn't be read"
END_OF_WAKE = "this is the end-of-wake sweep"


def wake_run_too_old(created: datetime | None, now: datetime) -> bool:
    """Whether a wake whose run was created at `created` must refuse to start
    a session at `now`. Its session ends within WAKE_JOB_MINUTES; every sweep
    up to LOOKBACK_MINUTES after that must still find the run in its completed
    window, which reaches LOOKBACK_MINUTES + COMPLETED_EXTRA_MINUTES back. So
    the run may be at most COMPLETED_EXTRA_MINUTES - WAKE_JOB_MINUTES old. An
    unknown creation time refuses (fail closed)."""
    if created is None:
        return True
    return created < now - timedelta(minutes=COMPLETED_EXTRA_MINUTES - WAKE_JOB_MINUTES)


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


def act(env, transport, findings: list[str], disable: bool) -> bool:
    """Post one escalation; disable the wake workflow on a bot-attributed
    finding. Returns whether the escalation was posted."""
    if not env.get("GITHUB_REPOSITORY"):
        raise SystemExit("tripwire: GITHUB_REPOSITORY isn't set; nothing posted or disabled")
    rid = "tripwire-" + "-".join(x for x in (env.get("GITHUB_RUN_ID", ""), env.get("GITHUB_RUN_ATTEMPT", "")) if _digits(x))
    lines = "\n".join(f"- {f}" for f in findings)
    tail = f"\n\n`{WAKE_WORKFLOW}` is disabled; re-enable it only after review." if disable else ""
    print(f"tripwire: {len(findings)} finding(s); disable={disable}")
    posted = False
    try:
        if _digits(env.get("CHANNEL_PR")):
            _channel(env, transport).post(message.report(rid, "none", "escalation", f"Tripwire:\n{lines}{tail}"))
            posted = True
    except Exception:  # noqa: BLE001 - the disable below still runs
        print("tripwire: couldn't post to the channel")
    if disable:
        _disable(env, transport)
    return posted


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


class _Mismatch(Exception):
    """Counts that don't add up: read again (SESSION_ATTEMPTS). `settling`:
    only runs started by others' comments, still moving, explain it."""

    def __init__(self, settling: bool):
        super().__init__()
        self.settling = settling


def _stamp(t: datetime) -> str:
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def _wake_session(env, transport, now: datetime, monotonic=time.monotonic, sleep=time.sleep) -> str | None:
    """Why an incomplete sweep counts a wake session as having run, or None.

    Read from the Actions API with the job's own token: operator-wake.yml's
    issue_comment runs in flight created in the last APPROVAL_WAIT_DAYS, and
    its completed runs created from COMPLETED_EXTRA_MINUTES before the
    lookback; in each, its jobs (_wake_ran: only a run whose gate succeeded
    can hold a session). A completed run that is skipped, or updated before
    the lookback, is passed over without reading its jobs. The first session
    found answers.
    Counts that don't add up are read again after SETTLE_SECONDS. On the last
    attempt they still count as no session when every run listed in flight
    had a gate that finished without success and the latest count leaves no
    run in flight unlisted (comments' runs settling). Otherwise it fails
    closed: any error, an unexpected answer, a list past SESSION_PAGES, counts
    that still don't add up, SESSION_REQUESTS requests, or
    SESSION_DEADLINE_SECONDS passing counts as a session. The end-of-wake
    sweep is decided by the caller (END_OF_WAKE)."""
    timed = with_deadline(transport, monotonic, SESSION_DEADLINE_SECONDS)
    made = [0]

    def call(method, path, body):
        made[0] += 1
        if made[0] > SESSION_REQUESTS:
            raise _Unknown
        return timed(method, path, body)
    since = now - timedelta(minutes=LOOKBACK_MINUTES)
    try:
        repo = env["GITHUB_REPOSITORY"]
        # The window ends at the sweep's `now`, fixed: a run created during the check isn't in it.
        lo, hi = now - timedelta(days=APPROVAL_WAIT_DAYS), now
        done_from = since - timedelta(minutes=COMPLETED_EXTRA_MINUTES)
        cache: dict[str, list[dict]] = {}  # a completed run's jobs don't change: read once
        for attempt in range(1, SESSION_ATTEMPTS + 1):
            if attempt > 1:
                sleep(SETTLE_SECONDS)
            try:
                return _session_pass(call, repo, lo, hi, done_from, since, sleep, cache)
            except _Mismatch as m:
                if attempt == SESSION_ATTEMPTS and m.settling:
                    return None
        return SESSION_UNKNOWN
    except Exception:  # noqa: BLE001 - fail closed (the deadline raises Incomplete, an Exception)
        return SESSION_UNKNOWN


def _runs_page(call, repo: str, lo: datetime, hi: datetime, status: str, page: int, per: int) -> tuple[list, int]:
    r = call("GET", f"/repos/{repo}/actions/workflows/{WAKE_WORKFLOW}/runs?created={_stamp(lo)}..{_stamp(hi)}"
                    f"&event=issue_comment{'&status=' + status if status else ''}&per_page={per}&page={page}", None)
    items, total = (r or {}).get("workflow_runs"), (r or {}).get("total_count")
    if not isinstance(items, list) or not isinstance(total, int) or isinstance(total, bool) or total < 0:
        raise _Unknown
    return items, total


def _difference(call, repo: str, lo: datetime, hi: datetime, minuend: str, subtrahend: str, depth: int = 0) -> int:
    """Runs created in [lo, hi] in `minuend` ("" for all) but not `subtrahend`.
    The subtrahend is counted first, so a run moving between them meanwhile
    can only make the difference larger (more runs to find: fail closed). A
    range where either count is exactly COUNT_CAP (perhaps capped) is split."""
    less = _runs_page(call, repo, lo, hi, subtrahend, 1, 1)[1]
    more = _runs_page(call, repo, lo, hi, minuend, 1, 1)[1]
    if COUNT_CAP in (less, more):
        if depth >= COUNT_SPLITS or hi - lo < timedelta(seconds=2):
            raise _Unknown
        mid = (lo + (hi - lo) / 2).replace(microsecond=0)
        return (_difference(call, repo, lo, mid, minuend, subtrahend, depth + 1)
                + _difference(call, repo, mid + timedelta(seconds=1), hi, minuend, subtrahend, depth + 1))
    if more < less:
        raise _Unknown  # an index that contradicts itself: never settle on it
    return more - less


def _listed(call, repo: str, lo: datetime, hi: datetime, status: str, key: str):
    """Each run in a status list once, checked to be in that status. In an
    in-flight list, a run whose copy already shows it completed and skipped
    (a comment's run that just settled) is passed over: the counts decide."""
    ids: set[str] = set()  # paging over a list that moves can repeat a run: each counts once
    for page in range(1, SESSION_PAGES + 1):
        items, total = _runs_page(call, repo, lo, hi, status, page, 100)
        for run in items:
            rid = _digits(run.get("id")) if isinstance(run, dict) else None
            if rid is None:
                raise _Unknown
            if rid in ids:
                continue
            ids.add(rid)
            if run.get(key) != status:
                if key == "status" and run.get("status") == "completed" and run.get("conclusion") == "skipped":
                    continue
                raise _Mismatch(False)  # moved on since it was listed, or a filter the API didn't apply
            yield rid, run
        if len(items) < 100 or len(ids) >= total:
            return
    raise _Unknown  # more than SESSION_PAGES pages


def _session_pass(call, repo, lo, hi, done_from, since, sleep, cache) -> str | None:
    expected = _difference(call, repo, lo, hi, "", "completed")
    listed, cleared = 0, True
    for status in RUN_STATES:
        if listed >= expected:
            break  # every run in flight accounted for
        for rid, run in _listed(call, repo, lo, hi, status, "status"):
            listed += 1
            ran, stranger = _wake_ran(call, repo, rid, since, True, sleep)
            if ran:
                return SESSION_RAN
            cleared = cleared and stranger
    want = _difference(call, repo, done_from, hi, "completed", "skipped")
    read = 0
    for group in (DONE_FIRST, DONE_REST):
        if group is DONE_REST and read >= want:
            break  # every completed run that isn't skipped accounted for
        for conclusion in group:
            for rid, run in _listed(call, repo, done_from, hi, conclusion, "conclusion"):
                read += 1
                updated = _when(run.get("updated_at"))
                if updated is not None and updated < since:
                    continue  # a run's update time is at or after its jobs' completion
                if _wake_ran(call, repo, rid, since, False, sleep, cache)[0]:
                    return SESSION_RAN
    if read < want:
        raise _Mismatch(False)  # a conclusion no list covers, or runs still completing
    latest = _difference(call, repo, lo, hi, "", "completed")
    if listed != expected or latest != expected:
        # Settling: every run listed in flight was someone else's, and none is left unlisted.
        raise _Mismatch(cleared and latest <= listed)
    return None


def _jobs(call, repo: str, rid: str) -> list[dict]:
    out: list[dict] = []
    for page in range(1, 4):
        j = call("GET", f"/repos/{repo}/actions/runs/{rid}/jobs?filter=all&per_page=100&page={page}", None)
        jobs, count = (j or {}).get("jobs"), (j or {}).get("total_count")
        if not isinstance(jobs, list) or not isinstance(count, int) or isinstance(count, bool) \
                or not all(isinstance(x, dict) for x in jobs):
            raise _Unknown
        out += jobs
        if len(jobs) < 100 or len(out) >= count:
            return out
    raise _Unknown


def _wake_ran(call, repo: str, rid: str, since: datetime, in_flight: bool, sleep,
              cache: dict | None = None) -> tuple[bool, bool]:
    """(whether run `rid` holds a session in the lookback, whether its gate
    finished without success), across all its attempts (`filter=all`: a
    re-run waiting for approval keeps its earlier attempts' jobs).

    Only a run with a `gate` job that succeeded can count: the `wake` job
    needs the gate to succeed, and the gate runs only for the runner's
    instruction. The `wake` job also runs only on a run's first attempt
    (`run_attempt == 1`), and gates are pooled across attempts here, so a
    later attempt can't hide the first one's session. Then a `wake` job
    counts when it wasn't skipped, isn't waiting for approval, and hadn't
    completed before the lookback. In flight, a gate that hasn't finished (or
    a wake job not there yet, or no jobs listed yet) is read again; still so
    after GATE_READS, or at the deadline, it counts as a session (_Unknown)."""
    for _ in range(GATE_READS):
        if cache is not None and rid in cache:
            jobs = cache[rid]
        else:
            jobs = _jobs(call, repo, rid)
            if cache is not None and not in_flight:
                cache[rid] = jobs
        gates = [x for x in jobs if x.get("name") == "gate"]
        wakes = [x for x in jobs if x.get("name") == "wake"]
        passed = any(g.get("status") == "completed" and g.get("conclusion") == "success" for g in gates)
        open_gate = not gates or any(g.get("status") != "completed" for g in gates)
        if not passed and not open_gate:
            return False, True  # not the runner's instruction, or refused at the gate
        if passed and (wakes or not in_flight):
            for job in wakes:
                if job.get("conclusion") == "skipped" or job.get("status") == "waiting":
                    continue
                done = _when(job.get("completed_at"))
                if job.get("completed_at") is None or done is None or done >= since:
                    return True, False
            return False, False  # every wake job skipped, waiting for approval, or completed before the lookback
        elif not in_flight:
            raise _Unknown  # a finished run without a finished gate
        sleep(GATE_WAIT_SECONDS)
    raise _Unknown


PENDING = (f"if a wake session ran inside the sweep's lookback, or that can't be read, `{WAKE_WORKFLOW}` "
           "is disabled after this post; check whether it is")


def _incomplete(env, transport, now, findings: list[str], monotonic, sleep=time.sleep) -> bool:
    """The owner's rule (2026-11-14) for an incomplete sweep without a bot
    finding: the escalation first, then the session check, then the disable
    if a session ran. A timeout can then miss the disable, never the post."""
    if env.get("GITHUB_WORKFLOW_REF") == f"{env['GITHUB_REPOSITORY']}/.github/workflows/{WAKE_WORKFLOW}@refs/heads/main":
        act(env, transport, findings + [f"{END_OF_WAKE}, so an incomplete sweep disables"], True)
        return True
    posted = act(env, transport, findings + [PENDING], False)
    why = _wake_session(env, transport, now, monotonic, sleep)
    if why is None:
        print("tripwire: no wake session in the lookback; not disabled")
        return posted
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
    return True  # disabled: someone will look


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


def cmd_sweep(env, transport, now, monotonic=time.monotonic, sleep=time.sleep) -> int:
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
        seen = True
    elif incomplete and not disable:
        seen = _incomplete(env, transport, now, findings, monotonic, sleep)
    else:
        seen = act(env, transport, findings, disable) or disable
    if failed:
        raise SystemExit("tripwire: the sweep failed")
    if not seen:  # nothing posted and nothing disabled: never a green, silent job
        raise SystemExit("tripwire: the escalation couldn't be posted and nothing was disabled")
    return 0


def main(argv=None, env=None, transport=None, clock=None, monotonic=None, sleep=None) -> int:
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
        return cmd_sweep(env, transport, (clock or (lambda: datetime.now(timezone.utc)))(), monotonic or time.monotonic,
                         sleep or time.sleep)
    raise SystemExit("usage: tripwire.py trip|sweep")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 - a traceback could carry values
        raise SystemExit(f"tripwire: internal error ({type(e).__name__})") from None
