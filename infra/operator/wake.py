"""operator-wake.yml's entry point: the bridge (OP-4; plan §1-§2, §4).

    wake.py gate     filter job, no secrets: channel PR, created only, the
                     runner's numeric ID, the header, the soft stop
    wake.py refuse   answers a refused instruction from the runner
    wake.py wake     cloud-operator job: everything again, then one session

`wake`, in order (each step refuses rather than guess):
1. The gates again, and every setting present.
2. The operator lock (`locks/operator.json`, If-None-Match). Taken: "busy, retry".
3. claude-lock.json for the agent, environment, vault and credential.
4. The ledger, from every session in the workspace (archived included): the
   instruction's id is new, at most 6 wakes an hour and 60 a day, and the
   $200 total plus this session's budget isn't passed. Any unreadable entry
   refuses.
5. A 1-hour App installation token, this repository and Pull requests write
   + Metadata read only, into the vault's GITHUB_TOKEN credential.
6. The helper files (thread.py, reader.py) from this job's own checkout,
   uploaded and mounted read-only into the session. The operator runs these
   copies; it downloads no code. A failed upload starts no session.
7. One session: the agent version from claude-lock.json (routine), or an
   override to OPERATOR_MODEL_TRIAGE at high effort (triage); a $3 or $15
   budget; the instruction comment as its only user.message.
8. Watching it: the session object gives only the status, so why it went
   idle comes from its event list (the last status event's stop reason). A
   call paused for approval is denied with a message to escalate; a budget
   stop, an error, a refusal, a stall or 30 minutes makes the bridge post the
   escalation itself, naming an error by the API's own type. So does a clean
   finish after an error the session didn't recover from, or in which the
   permission policy refused a tool call. Every session is
   archived at the end, so none can be resumed later with a newer token.
9. Always: the token revoked, the credential reset, the uploaded files
   deleted, the lock released.

Comment text arrives only through COMMENT_BODY. Logs and comments carry
fixed lines; never a token, an ID from the vault or the lock, or the text.
"""

from __future__ import annotations

import hashlib
import json
import os
import secrets
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "fleet"))

import bridge  # noqa: E402
import ghapp  # noqa: E402
import message  # noqa: E402
import tripwire  # noqa: E402 - wake_run_too_old: the tripwire's own window
from apiclient import (ANTHROPIC, ANTHROPIC_VERSION, MANAGED_AGENTS_BETA, ApiError,  # noqa: E402
                       anthropic, check_url, github, opener, pages)
from bridge import MODEL_ID, Refusal  # noqa: E402
from oplock import OperatorLock  # noqa: E402
from reader import CONTROL_PLANE_USER_ID  # noqa: E402

TOTAL_CAP_CENTS = 20000  # the owner's $200 of API credits, in total (Q2)
BUDGET_CENTS = {"routine": 300, "triage": 1500}  # per session (plan §1)
PER_HOUR, PER_DAY = 6, 60
SESSION_MINUTES = 30
POLL_SECONDS = 10
STALL_POLLS = 6  # a minute of idle polls with nothing to deny, or with no stop reason in the events
INTERRUPT_SECONDS = 60  # after an interrupt, the wait for the session to stop before the archive
TRIAGE_EFFORT = "high"  # an override replaces the model object, effort included
UNSET_TOKEN = "revoked"
# The session's own status events, in the event list. A session object has no
# stop reason: only its last `session.status_idle` event does.
STATUS_EVENTS = ("session.status_running", "session.status_idle", "session.status_rescheduled",
                 "session.status_terminated")
# `session.error`'s types and retry statuses, as the API lists them. An escalation
# names only a value found here, never the error's free-text message.
ERROR_TYPES = frozenset((
    "unknown_error", "model_overloaded_error", "model_rate_limited_error", "model_request_failed_error",
    "mcp_connection_failed_error", "mcp_authentication_failed_error", "billing_error",
    "credential_host_unreachable_error", "tool_use_limit_reached_error", "compaction_limit_reached_error",
    "repository_authentication_error", "repository_forbidden_error", "repository_not_found_error",
    "repository_checkout_error", "repository_clone_error"))
RETRY_STATUSES = frozenset(("retrying", "exhausted", "terminal"))
# The reason codes on a call the `auto` permission policy refused outright (the
# event's `evaluation.evaluated_permission`, type `deny`): judged high-risk, or
# no judgement reached and the call not held for approval.
POLICY_REASONS = frozenset(("high_risk", "indeterminate"))
DENY = ("This call needs approval, which the bridge never gives. Don't retry or work around it: "
        "post an operator-report v1 with kind: escalation saying what you tried to do, then end your turn.")

# The channel helper and the reader it imports, mounted into every session from
# this checkout: the commit the workflow runs, never a branch read at run time.
# A resource's mount_path lands under /mnt/session/uploads, so these are
# /mnt/session/uploads/op/<name>; the system prompt copies that folder.
MOUNT = "/op"
HELPERS = {"thread.py": HERE / "thread.py", "reader.py": HERE.parent / "fleet" / "reader.py"}

WAKE_SETTINGS = ("ANTHROPIC_API_KEY", "OPERATOR_APP_ID", "OPERATOR_APP_PRIVATE_KEY", "AWS_BRIDGE_ROLE_ARN",
                 "CONTROL_BUCKET", "FLEET_BUCKET_OWNER", "GITHUB_TOKEN", "GITHUB_REPOSITORY",
                 "ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN")


class Gated(Exception):
    """Not an event the bridge acts on; ignored without a comment."""


def _numeric(v) -> str:
    if not v or not (str(v).isascii() and str(v).isdigit()):
        raise Gated("unset or non-numeric id")
    return str(v)


def check_event(env) -> message.Message:
    """Raises Gated (silent), or message.Refused / Refusal (answered)."""
    if env.get("EVENT_NAME") != "issue_comment" or env.get("EVENT_ACTION") != "created":
        raise Gated("not a created comment")
    if env.get("IS_PR") != "true" or _numeric(env.get("ISSUE_NUMBER")) != _numeric(env.get("CHANNEL_PR")):
        raise Gated("not the channel PR")
    runner, bot = _numeric(env.get("RUNNER_USER_ID")), _numeric(env.get("OPERATOR_BOT_USER_ID"))
    if runner in (bot, CONTROL_PLANE_USER_ID) or _numeric(env.get("AUTHOR_ID")) != runner:
        raise Gated("author is not the runner")
    if env.get("GITHUB_RUN_ATTEMPT") != "1":
        raise Gated("a re-run")  # a re-run replays the old event: never a second session
    msg = message.parse(env.get("COMMENT_BODY", ""))
    if env.get("OPERATOR_STOP", "") != "false":  # fail closed: unset is a stop
        raise Refusal("OPERATOR_STOP is set")
    return msg


def _out(env, **kv) -> None:
    path = env.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            for k, v in kv.items():
                f.write(f"{k}={v}\n")


def _summary(env, msg: message.Message) -> None:
    """What the owner approves (X-operator-hardening): the parsed instruction's
    fixed fields only, in the job summary. The id matches message.ID and kind
    and mode are fixed words, so no comment text reaches the page. The body's
    short SHA-256 (fixed hex) tells apart two instructions with the same id."""
    path = env.get("GITHUB_STEP_SUMMARY")
    if not path:
        return
    digest = hashlib.sha256(env.get("COMMENT_BODY", "").encode("utf-8")).hexdigest()[:12]
    with open(path, "a", encoding="utf-8") as f:
        f.write(f"### Instruction to approve\n\n- id: `{msg.id}`\n- kind: `{msg.kind}`\n- mode: `{msg.mode}`\n"
                f"- in reply to: `{msg.in_reply_to or 'none'}`\n- body SHA-256 (first 12): `{digest}`\n\n"
                "Approve the `cloud-operator` deployment only if this is the instruction the runner posted.\n")


def cmd_gate(env) -> int:
    try:
        msg = check_event(env)
    except Gated:
        print("operator-wake: not for the bridge")
        _out(env, allowed="false", reason="")
        return 0
    except message.Refused as e:
        print(f"operator-wake: refused ({e.reason})")
        _out(env, allowed="false", reason=e.reason)
        return 0
    except Refusal:
        print("operator-wake: refused (OPERATOR_STOP is set)")
        _out(env, allowed="false", reason="OPERATOR_STOP is set")
        return 0
    print("operator-wake: allowed")
    _out(env, allowed="true", reason="")
    _summary(env, msg)
    return 0


# Every fixed reason but "not an operator instruction": an ordinary runner
# comment on the channel PR gets no answer.
REFUSE_REASONS = (set(message.REASONS) - {"not an operator instruction"}) | {"OPERATOR_STOP is set"}


def _channel(env, transport=None):
    from comments import Channel  # noqa: PLC0415 - infra/fleet/comments.py
    pr, repo = env.get("CHANNEL_PR", ""), env.get("GITHUB_REPOSITORY", "")
    if not (pr.isdigit() and repo):
        return None
    if transport is None:  # pragma: no cover - network
        from comments import urllib_transport  # noqa: PLC0415
        transport = urllib_transport(env.get("GITHUB_TOKEN", ""))
    return Channel(transport, repo, pr)


def post(env, line: str, transport=None) -> None:
    """Best effort, fixed text: a failure to post never changes what happens next."""
    print(f"operator-wake: {line}")
    try:
        ch = _channel(env, transport)
        if ch:
            ch.post(f"`operator-wake.yml`: {line}")
    except Exception:  # noqa: BLE001
        print("operator-wake: couldn't post to the channel")


def escalate(env, in_reply_to: str | None, line: str, transport=None) -> None:
    rid = "bridge-" + "-".join(x for x in (env.get("GITHUB_RUN_ID", ""), env.get("GITHUB_RUN_ATTEMPT", "")) if x.isdigit())
    body = message.report(rid, in_reply_to or "none", "escalation", f"`operator-wake.yml`: {line}")
    print(f"operator-wake: escalation: {line}")
    try:
        ch = _channel(env, transport)
        if ch:
            ch.post(body)
    except Exception:  # noqa: BLE001
        print("operator-wake: couldn't post to the channel")


def cmd_refuse(env, transport=None) -> int:
    reason = env.get("REFUSAL_REASON", "")
    if reason in REFUSE_REASONS:  # fixed strings only, never the comment
        post(env, f"instruction refused ({reason})", transport)
    return 0


# ---- the ledger ----

def _when(s: str) -> datetime:
    t = datetime.fromisoformat(s.replace("Z", "+00:00"))
    if t.tzinfo is None:
        raise ValueError("naive time")
    return t


def ledger_refusal(sessions: list[dict], now: datetime, mode: str, inst_id: str,
                   last_session: str | None = None) -> str | None:
    """The reason this wake can't start, or None. Fails closed on any doubt.

    `last_session` is the previous wake's session (claude-lock.json): it's
    archived by now, so a list without it means archived sessions are missing
    and every total below would be too low."""
    if last_session and last_session not in {s.get("id") for s in sessions}:
        return "the spend ledger is missing the last session"
    total, hour, day = 0, 0, 0
    for s in sessions:
        cost = ((s.get("usage") or {}).get("list_cost") or {})
        amount, created = cost.get("amount"), s.get("created_at")
        if not (isinstance(amount, str) and amount.isascii() and amount.isdigit()) or cost.get("currency") != "USD":
            return "the spend ledger is unreadable"
        try:
            age = now - _when(created)
        except (TypeError, ValueError, AttributeError):
            return "the spend ledger is unreadable"
        total += int(amount)
        hour += age < timedelta(hours=1)
        day += age < timedelta(days=1)
        if (s.get("metadata") or {}).get("instruction_id") == inst_id:
            return "this instruction id was already used"
    if total + BUDGET_CENTS[mode] > TOTAL_CAP_CENTS:
        return "the $200 total would be passed"
    if hour >= PER_HOUR:
        return "the rate limit (6 wakes an hour) is reached"
    if day >= PER_DAY:
        return "the rate limit (60 wakes a day) is reached"
    return None


# ---- the helper files ----

def upload(api_key: str):  # pragma: no cover - network
    """POST /v1/files as multipart, through apiclient's host check and its
    no-redirect opener. Returns a sender: (name, bytes) -> the file ID. The
    injection harness uploads through this too."""
    def send(name: str, data: bytes) -> str:
        path = "/v1/files"
        check_url(ANTHROPIC + path, path)
        boundary = "lodestar" + secrets.token_hex(12)
        body = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n"
                f"Content-Type: text/plain\r\n\r\n").encode() + data + f"\r\n--{boundary}--\r\n".encode()
        req = urllib.request.Request(ANTHROPIC + path, method="POST", data=body, headers={
            "x-api-key": api_key, "anthropic-version": ANTHROPIC_VERSION, "anthropic-beta": MANAGED_AGENTS_BETA,
            "content-type": f"multipart/form-data; boundary={boundary}", "User-Agent": "lodestar-operator"})
        try:
            with opener().open(req, timeout=30) as r:
                fid = json.loads(r.read()).get("id")
        except urllib.error.HTTPError as e:
            raise ApiError("POST /v1/files", e.code) from None
        except Exception:
            raise ApiError("POST /v1/files", None) from None
        if not isinstance(fid, str) or not fid:
            raise ApiError("POST /v1/files", None)
        return fid
    return send


def mount_helpers(send, files: list[str]) -> list[dict]:
    """Uploads each helper as it is in this checkout and returns the session's
    file resources. Every ID is appended to `files` as soon as it exists, so a
    failure part-way still leaves the caller the IDs to delete."""
    resources = []
    for name, src in HELPERS.items():
        fid = send(name, src.read_bytes())
        files.append(fid)
        resources.append({"type": "file", "file_id": fid, "mount_path": f"{MOUNT}/{name}"})
    return resources


# ---- the session ----

def session_body(msg: message.Message, lock: dict, env, body_text: str, run_id: str, resources: list[dict]) -> dict:
    if msg.mode == "triage":
        triage = env.get("OPERATOR_MODEL_TRIAGE", "").strip()
        if not MODEL_ID.match(triage):
            raise Refusal("OPERATOR_MODEL_TRIAGE isn't a model ID")
        agent = {"type": "agent_with_overrides", "id": lock["agent_id"], "version": lock["agent_version"],
                 "model": {"id": triage, "effort": TRIAGE_EFFORT}}
    else:
        agent = {"type": "agent", "id": lock["agent_id"], "version": lock["agent_version"]}
    return {
        "agent": agent,
        "environment_id": lock["environment_id"],
        "vault_ids": [lock["vault_id"]],
        "title": f"wake {msg.id}",
        "metadata": {"instruction_id": msg.id, "kind": msg.kind, "mode": msg.mode, "run_id": run_id},
        "budget": {"type": "limit", "max_list_cost": {"amount": str(BUDGET_CENTS[msg.mode]), "currency": "USD"}},
        "resources": resources,  # the helper files, read-only, under /mnt/session/uploads
        # Runner-authored text only: Managed Agents reads user.message as operator intent.
        "initial_events": [{"type": "user.message", "content": [{"type": "text", "text": body_text}]}],
    }


def _events(call, sid: str) -> list[dict]:
    """Every event of the session, oldest first. `_stop_reason` and the error
    readers below take the last matching event as the latest, so the order is
    asked for (the list's `order` parameter; `asc` is also its default) rather
    than assumed."""
    return pages(call, f"/v1/sessions/{sid}/events", {"order": "asc"})


def _stop_reason(events: list[dict]) -> str | None:
    """Why the session is idle: the stop reason of its last status event, when
    that event is a `session.status_idle`. None when the list doesn't show the
    idle yet (the status and the event list are written separately), or shows
    no reason: the caller polls again rather than guess."""
    for e in reversed(events):
        if e.get("type") in STATUS_EVENTS:
            reason = e.get("stop_reason") if e["type"] == "session.status_idle" else None
            kind = reason.get("type") if isinstance(reason, dict) else None
            return kind if isinstance(kind, str) and kind else None
    return None


def _error(event: dict) -> tuple[str, str]:
    """A `session.error` as two fixed words (type, retry status). Both are
    checked against the API's own lists, so no text from the session reaches a
    log or a comment."""
    err = event.get("error") if isinstance(event.get("error"), dict) else {}
    kind, retry = err.get("type"), err.get("retry_status")
    retry = retry.get("type") if isinstance(retry, dict) else None
    return (kind if isinstance(kind, str) and kind in ERROR_TYPES else "an unlisted error type",
            retry if isinstance(retry, str) and retry in RETRY_STATUSES else "an unlisted retry status")


def _error_words(events: list[dict]) -> str:
    """The session's last `session.error` as " (type, retry status)", or ""."""
    for e in reversed(events):
        if e.get("type") == "session.error":
            return " (%s, %s)" % _error(e)
    return ""


def unrecovered_error(events: list[dict]) -> str:
    """The escalation line for a session that ended its turn after an error it
    didn't recover from, or "". Only `retrying` is a recovery in progress:
    `exhausted`, `terminal` and any status this bridge doesn't know all count,
    so an unknown value fails closed. The last such error is the one named."""
    for e in reversed(events):
        if e.get("type") == "session.error" and _error(e)[1] != "retrying":
            return "the session finished after an error it didn't recover from (%s, %s)" % _error(e)
    return ""


def policy_denials(events: list[dict]) -> str:
    """The escalation line for a session whose tool calls the permission policy
    refused outright, or "". Such a session still ends its turn cleanly, but an
    operator that can't run its tools can't read the channel or post a report,
    so a finish with denied calls is never left silent. Calls the bridge itself
    denied (paused for approval) aren't counted: those stay `ask`. The reason
    is printed only when it's on the fixed list."""
    reasons = set()
    count = 0
    for e in events:
        if e.get("type") in ("agent.tool_use", "agent.mcp_tool_use") and e.get("evaluated_permission") == "deny":
            count += 1
            judged = e.get("evaluation") if isinstance(e.get("evaluation"), dict) else {}
            judged = judged.get("evaluated_permission") if isinstance(judged.get("evaluated_permission"), dict) else {}
            code = judged.get("reason_code")
            reasons.add(code if isinstance(code, str) and code in POLICY_REASONS else "an unlisted reason")
    if not count:
        return ""
    return (f"the session finished, but the permission policy denied {count} of its tool calls "
            f"({', '.join(sorted(reasons))}); the operator may have posted no report")


def _deny_pending(call, sid: str, events: list[dict], denied: set) -> int:
    answered = {e.get("tool_use_id") for e in events if e.get("type") == "user.tool_confirmation"}
    ask = [e["id"] for e in events if e.get("type") in ("agent.tool_use", "agent.mcp_tool_use")
           and e.get("evaluated_permission") == "ask" and isinstance(e.get("id"), str)
           and e["id"] not in answered and e["id"] not in denied]
    if ask:
        call("POST", f"/v1/sessions/{sid}/events", {"events": [
            {"type": "user.tool_confirmation", "tool_use_id": i, "result": "deny", "deny_message": DENY} for i in ask]})
        denied.update(ask)
    return len(ask)


def _interrupt(call, sid: str, monotonic, sleep) -> None:
    """Interrupt, then wait (at most a minute) until the session isn't running,
    so the archive that follows is accepted."""
    try:
        call("POST", f"/v1/sessions/{sid}/events", {"events": [{"type": "user.interrupt"}]})
    except Exception:  # noqa: BLE001 - the archive is retried by hand if it's refused
        return
    end = monotonic() + INTERRUPT_SECONDS
    while monotonic() < end:
        try:
            if (call("GET", f"/v1/sessions/{sid}", None) or {}).get("status") in ("idle", "terminated"):
                return
        except Exception:  # noqa: BLE001
            return
        sleep(POLL_SECONDS)


def watch(call, sid: str, monotonic=time.monotonic, sleep=time.sleep, stopped=lambda: False) -> str:
    """'done', or the escalation line for a session that couldn't escalate itself."""
    return watch_events(call, sid, monotonic, sleep, stopped)[0]


def finish_problem(events: list[dict]) -> str:
    """Why a session that ended its turn cleanly still needs an escalation, or
    "": an error it didn't recover from, tool calls the policy refused, or both."""
    return "; ".join(x for x in (unrecovered_error(events), policy_denials(events)) if x)


def watch_events(call, sid: str, monotonic=time.monotonic, sleep=time.sleep,
                 stopped=lambda: False) -> tuple[str, list[dict]]:
    """`watch`, and the event list a "done" was read from (empty otherwise), so
    what the bridge says about a finish comes from the same read that showed it.

    The session object carries a status and no stop reason, so an idle or
    terminated session is read from its event list. A session that finishes
    its work goes idle (`end_turn`); `terminated` is an unrecoverable error or
    an archive from outside, never a finish.

    `stopped()` is true once operator-wake.yml is disabled (the tripwire, or the
    owner's kill switch): the live session is interrupted at the next poll."""
    deadline = monotonic() + SESSION_MINUTES * 60
    denied: set = set()
    stalled = 0
    while True:
        if monotonic() >= deadline:
            _interrupt(call, sid, monotonic, sleep)
            return f"the session ran past {SESSION_MINUTES} minutes and was stopped", []
        if stopped():
            _interrupt(call, sid, monotonic, sleep)
            return "operator-wake.yml was disabled, so the session was stopped", []
        status = (call("GET", f"/v1/sessions/{sid}", None) or {}).get("status")
        if status == "terminated":
            return "the session was terminated" + _error_words(_events(call, sid)), []
        if status == "idle":
            events = _events(call, sid)
            stop = _stop_reason(events)
            if stop == "end_turn":
                return "done", events
            if stop == "budget_reached":
                return "the session reached its budget", []
            if stop == "refusal":
                return "the session stopped on a model refusal", []
            if stop == "requires_action" and _deny_pending(call, sid, events, denied):
                stalled = 0
            elif stop in ("requires_action", None):
                stalled += 1
                if stalled >= STALL_POLLS:
                    # A no-op on an idle session; it stops one whose status lagged behind a resume,
                    # so the archive that follows is never refused with the session still running.
                    _interrupt(call, sid, monotonic, sleep)
                    return ("the session stalled waiting for an action the bridge can't give" if stop
                            else "the session went idle and its events show no stop reason"), []
            else:  # retries_exhausted, or a reason this bridge doesn't know
                return "the session ended with an error" + _error_words(events), []
        else:
            stalled = 0
        sleep(POLL_SECONDS)


def wake_disabled(env, transport) -> Callable[[], bool]:
    """Reads operator-wake.yml's state with the job's token (actions: read). A
    failed read never stops a session: only a state other than `active` does."""
    path = f"/repos/{env.get('GITHUB_REPOSITORY', '')}/actions/workflows/operator-wake.yml"

    def check() -> bool:
        try:
            state = (transport("GET", path, None) or {}).get("state")
        except Exception:  # noqa: BLE001
            return False
        return isinstance(state, str) and state != "active"
    return check


def _run_created(env, transport) -> datetime | None:
    """This run's creation time, read with the job's token (actions: read), or
    None when it can't be read (the caller refuses: fail closed)."""
    try:
        r = transport("GET", f"/repos/{env['GITHUB_REPOSITORY']}/actions/runs/{int(env['GITHUB_RUN_ID'])}", None)
    except Exception:  # noqa: BLE001
        return None
    return tripwire._when(r.get("created_at")) if isinstance(r, dict) else None


def stale_sessions(sessions: list[dict]) -> list[str]:
    """Bridge sessions an earlier wake never archived (a cancelled or lost job):
    still attached to the vault, so they're archived before a new token goes in."""
    return [s["id"] for s in sessions if isinstance(s.get("id"), str) and not s.get("archived_at")
            and (s.get("metadata") or {}).get("run_id")]


def cmd_wake(env, deps=None) -> int:
    deps = deps or {}
    try:
        msg = check_event(env)
    except (Gated, message.Refused, Refusal):
        print("operator-wake: not for the bridge")  # gate already answered it
        return 0
    gh_post = deps.get("channel_transport")
    now = deps.get("clock", lambda: datetime.now(timezone.utc))
    mono, sleep = deps.get("monotonic", time.monotonic), deps.get("sleep", time.sleep)
    if gh_post is None:  # pragma: no cover - network
        from comments import urllib_transport  # noqa: PLC0415
        gh_post = urllib_transport(env["GITHUB_TOKEN"])
    # A run approved so late that a later sweep's window can't see it starts no session,
    # before anything is held: the tripwire must be able to find every session
    # (tripwire.wake_run_too_old).
    if tripwire.wake_run_too_old(_run_created(env, gh_post), now()):
        escalate(env, msg.id, "no session started: the instruction was approved too late for the "
                 "tripwire to see its session; post it again", gh_post)
        return 0
    try:  # nothing is held yet; a failure here still gets an answer
        bridge.require(env, WAKE_SETTINGS)
        run_id = f"{env['GITHUB_RUN_ID']}-{env['GITHUB_RUN_ATTEMPT']}"
        store = deps.get("store")
        if store is None:  # pragma: no cover - live only
            store = bridge.connect_store(env, run_id)
        call = deps.get("anthropic") or anthropic(env["ANTHROPIC_API_KEY"])
        gh = deps.get("github") or github()
        lock = OperatorLock(store, now)
        acquired = lock.acquire(run_id)
    except (Exception, SystemExit) as e:  # SystemExit too: OIDC and STS failures in apply.oidc_session exit
        escalate(env, msg.id, f"the bridge failed before the lock ({type(e).__name__}); no session runs", gh_post)
        raise
    if not acquired:
        post(env, f"busy, retry (instruction {msg.id})", gh_post)
        return 0
    token = cred = sid = None
    files: list[str] = []
    try:
        ids, etag = bridge.read_claude_lock(store)
        if ids is None:
            escalate(env, msg.id, "the agent isn't applied yet (run operator-apply.yml)", gh_post)
            return 0
        sessions = pages(call, "/v1/sessions", {"include_archived": "true"})
        for old in stale_sessions(sessions):
            call("POST", f"/v1/sessions/{old}/archive", {})
            escalate(env, msg.id, "an earlier wake left a session unarchived; it is archived now", gh_post)
        reason = ledger_refusal(sessions, now(), msg.mode, msg.id, ids.get("last_session_id"))
        if reason:
            escalate(env, msg.id, f"no session started: {reason}", gh_post)
            return 0
        send = deps.get("upload") or upload(env["ANTHROPIC_API_KEY"])
        try:  # before the token exists: a wake that can't mount its helpers holds nothing to revoke
            resources = mount_helpers(send, files)
        except Exception:  # noqa: BLE001 - a fixed line; the error may carry details
            escalate(env, msg.id, "no session started: the helper files couldn't be uploaded", gh_post)
            return 0
        body = session_body(msg, ids, env, env["COMMENT_BODY"], run_id, resources)
        jwt = ghapp.app_jwt(env["OPERATOR_APP_ID"].strip(), env["OPERATOR_APP_PRIVATE_KEY"])
        token = ghapp.mint(gh, env["GITHUB_REPOSITORY"], jwt)
        cred = f"/v1/vaults/{ids['vault_id']}/credentials/{ids['credential_id']}"
        call("POST", cred, {"auth": {"type": "environment_variable", "secret_value": token}})
        # From here a session may exist (a lost or malformed reply can hide one), so the
        # end-of-session sweep must run. Still after the ledger, so PER_HOUR bounds it.
        _out(env, started="true")
        res = call("POST", "/v1/sessions", body) or {}
        sid = res.get("id")
        if not isinstance(sid, str) or not sid:
            raise Refusal("the API returned no session")
        print(f"operator-wake: session started ({msg.mode})")
        if not bridge.record_session(store, ids, etag, sid):
            escalate(env, msg.id, "couldn't record the session in claude-lock.json; the next wake will refuse", gh_post)
        outcome, events = watch_events(call, sid, mono, sleep, wake_disabled(env, gh_post))
        print(f"operator-wake: session {'ended' if outcome == 'done' else 'stopped'}")
        if outcome == "done":
            outcome = finish_problem(events) or outcome
        if outcome != "done":
            escalate(env, msg.id, outcome, gh_post)
        return 0
    except (Exception, SystemExit) as e:
        if sid:  # never leave a session spending its budget after a crash: stop it, then the archive below
            _interrupt(call, sid, mono, sleep)
        escalate(env, msg.id, f"the bridge failed ({type(e).__name__}); the session, if any, is archived", gh_post)
        raise
    finally:
        _cleanup(env, call, gh, sid, token, cred, files, lock, run_id, msg.id, gh_post)


def _cleanup(env, call, gh, sid, token, cred, files, lock, run_id, inst_id, gh_post) -> None:
    """Each step on its own: one failing never skips the next."""
    steps = []
    if sid:
        steps.append(("archive the session", lambda: call("POST", f"/v1/sessions/{sid}/archive", {})))
    if token:
        steps.append(("revoke the App token", lambda: ghapp.revoke(gh, token)))
    if cred:
        steps.append(("reset the vault credential",
                      lambda: call("POST", cred, {"auth": {"type": "environment_variable", "secret_value": UNSET_TOKEN}})))
    for fid in files:  # after the archive: the session holds its own copies
        steps.append(("delete a helper file", lambda fid=fid: call("DELETE", f"/v1/files/{fid}", None)))

    def release():
        if not lock.release(run_id):
            raise RuntimeError("lock not released")
    steps.append(("release the operator lock", release))
    for what, step in steps:
        try:
            step()
        except Exception:  # noqa: BLE001
            escalate(env, inst_id, f"couldn't {what}; check it by hand", gh_post)


def main(argv=None, env=None, deps=None) -> int:
    env = dict(os.environ if env is None else env)
    argv = sys.argv[1:] if argv is None else argv
    if argv == ["gate"]:
        return cmd_gate(env)
    if argv == ["refuse"]:
        return cmd_refuse(env, (deps or {}).get("channel_transport"))
    if argv == ["wake"]:
        return cmd_wake(env, deps)
    raise SystemExit("usage: wake.py gate|refuse|wake")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refusal:
        raise
    except Exception as e:  # noqa: BLE001 - a traceback could carry values
        raise SystemExit(f"operator: internal error ({type(e).__name__})") from None
