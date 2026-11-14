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
6. One session: the agent version from claude-lock.json (routine), or an
   override to OPERATOR_MODEL_TRIAGE at high effort (triage); a $3 or $15
   budget; the instruction comment as its only user.message.
7. Watching it: a call paused for approval is denied with a message to
   escalate; a budget stop, an error, a stall or 30 minutes makes the bridge
   post the escalation itself. Every session is archived at the end, so none
   can be resumed later with a newer token.
8. Always: the token revoked, the credential reset, the lock released.

Comment text arrives only through COMMENT_BODY. Logs and comments carry
fixed lines; never a token, an ID from the vault or the lock, or the text.
"""

from __future__ import annotations

import hashlib
import os
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "fleet"))

import bridge  # noqa: E402
import ghapp  # noqa: E402
import message  # noqa: E402
import tripwire  # noqa: E402 - wake_run_too_old: the tripwire's own window
from apiclient import anthropic, github, pages  # noqa: E402
from bridge import MODEL_ID, Refusal  # noqa: E402
from oplock import OperatorLock  # noqa: E402
from reader import CONTROL_PLANE_USER_ID  # noqa: E402

TOTAL_CAP_CENTS = 20000  # the owner's $200 of API credits, in total (Q2)
BUDGET_CENTS = {"routine": 300, "triage": 1500}  # per session (plan §1)
PER_HOUR, PER_DAY = 6, 60
SESSION_MINUTES = 30
POLL_SECONDS = 10
STALL_POLLS = 6  # a requires_action with nothing left to deny, a minute long
INTERRUPT_SECONDS = 60  # after an interrupt, the wait for the session to stop before the archive
TRIAGE_EFFORT = "high"  # an override replaces the model object, effort included
UNSET_TOKEN = "revoked"
DENY = ("This call needs approval, which the bridge never gives. Don't retry or work around it: "
        "post an operator-report v1 with kind: escalation saying what you tried to do, then end your turn.")

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


# ---- the session ----

def session_body(msg: message.Message, lock: dict, env, body_text: str, run_id: str) -> dict:
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
        # Runner-authored text only: Managed Agents reads user.message as operator intent.
        "initial_events": [{"type": "user.message", "content": [{"type": "text", "text": body_text}]}],
    }


def _deny_pending(call, sid: str, denied: set) -> int:
    events = pages(call, f"/v1/sessions/{sid}/events", {})
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
    """'done', or the escalation line for a session that couldn't escalate itself.

    `stopped()` is true once operator-wake.yml is disabled (the tripwire, or the
    owner's kill switch): the live session is interrupted at the next poll."""
    deadline = monotonic() + SESSION_MINUTES * 60
    denied: set = set()
    stalled = 0
    while True:
        if monotonic() >= deadline:
            _interrupt(call, sid, monotonic, sleep)
            return f"the session ran past {SESSION_MINUTES} minutes and was stopped"
        if stopped():
            _interrupt(call, sid, monotonic, sleep)
            return "operator-wake.yml was disabled, so the session was stopped"
        s = call("GET", f"/v1/sessions/{sid}", None) or {}
        status, stop = s.get("status"), ((s.get("stop_reason") or {}).get("type"))
        if status == "terminated":
            return "done" if stop in (None, "end_turn") else "the session ended with an error"
        if status == "idle":
            if stop == "end_turn":
                return "done"
            if stop == "budget_reached":
                return "the session reached its budget"
            if stop == "requires_action":
                if _deny_pending(call, sid, denied):
                    stalled = 0
                else:
                    stalled += 1
                    if stalled >= STALL_POLLS:
                        return "the session stalled waiting for an action the bridge can't give"
            else:
                return "the session ended with an error"
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
        body = session_body(msg, ids, env, env["COMMENT_BODY"], run_id)
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
        outcome = watch(call, sid, mono, sleep, wake_disabled(env, gh_post))
        print(f"operator-wake: session {'ended' if outcome == 'done' else 'stopped'}")
        if outcome != "done":
            escalate(env, msg.id, outcome, gh_post)
        return 0
    except (Exception, SystemExit) as e:
        if sid:  # never leave a session spending its budget after a crash: stop it, then the archive below
            _interrupt(call, sid, mono, sleep)
        escalate(env, msg.id, f"the bridge failed ({type(e).__name__}); the session, if any, is archived", gh_post)
        raise
    finally:
        _cleanup(env, call, gh, sid, token, cred, lock, run_id, msg.id, gh_post)


def _cleanup(env, call, gh, sid, token, cred, lock, run_id, inst_id, gh_post) -> None:
    """Each step on its own: one failing never skips the next."""
    steps = []
    if sid:
        steps.append(("archive the session", lambda: call("POST", f"/v1/sessions/{sid}/archive", {})))
    if token:
        steps.append(("revoke the App token", lambda: ghapp.revoke(gh, token)))
    if cred:
        steps.append(("reset the vault credential",
                      lambda: call("POST", cred, {"auth": {"type": "environment_variable", "secret_value": UNSET_TOKEN}})))
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
