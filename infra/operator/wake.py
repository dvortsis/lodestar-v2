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

import os
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "fleet"))

import bridge  # noqa: E402
import ghapp  # noqa: E402
import message  # noqa: E402
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


def cmd_gate(env) -> int:
    try:
        check_event(env)
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
    return 0


REFUSE_REASONS = set(message.REASONS) | {"OPERATOR_STOP is set"}


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


def ledger_refusal(sessions: list[dict], now: datetime, mode: str, inst_id: str) -> str | None:
    """The reason this wake can't start, or None. Fails closed on any doubt."""
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


def watch(call, sid: str, monotonic=time.monotonic, sleep=time.sleep) -> str:
    """'done', or the escalation line for a session that couldn't escalate itself."""
    deadline = monotonic() + SESSION_MINUTES * 60
    denied: set = set()
    stalled = 0
    while True:
        if monotonic() >= deadline:
            try:
                call("POST", f"/v1/sessions/{sid}/events", {"events": [{"type": "user.interrupt"}]})
            except Exception:  # noqa: BLE001 - the archive below stops it either way
                pass
            return f"the session ran past {SESSION_MINUTES} minutes and was stopped"
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


def cmd_wake(env, deps=None) -> int:
    deps = deps or {}
    try:
        msg = check_event(env)
    except (Gated, message.Refused, Refusal):
        print("operator-wake: not for the bridge")  # gate already answered it
        return 0
    bridge.require(env, WAKE_SETTINGS)
    run_id = f"{env['GITHUB_RUN_ID']}-{env['GITHUB_RUN_ATTEMPT']}"
    gh_post = deps.get("channel_transport")
    now = deps.get("clock", lambda: datetime.now(timezone.utc))
    store = deps.get("store")
    if store is None:  # pragma: no cover - live only
        store = bridge.connect_store(env, run_id)
    call = deps.get("anthropic") or anthropic(env["ANTHROPIC_API_KEY"])
    gh = deps.get("github") or github()
    lock = OperatorLock(store, now)
    if not lock.acquire(run_id):
        post(env, f"busy, retry (instruction {msg.id})", gh_post)
        return 0
    token = cred = sid = None
    try:
        ids, _ = bridge.read_claude_lock(store)
        if ids is None:
            escalate(env, msg.id, "the agent isn't applied yet (run operator-apply.yml)", gh_post)
            return 0
        reason = ledger_refusal(pages(call, "/v1/sessions", {"include_archived": "true"}), now(), msg.mode, msg.id)
        if reason:
            escalate(env, msg.id, f"no session started: {reason}", gh_post)
            return 0
        body = session_body(msg, ids, env, env["COMMENT_BODY"], run_id)
        jwt = ghapp.app_jwt(env["OPERATOR_APP_ID"].strip(), env["OPERATOR_APP_PRIVATE_KEY"])
        token = ghapp.mint(gh, env["GITHUB_REPOSITORY"], jwt)
        cred = f"/v1/vaults/{ids['vault_id']}/credentials/{ids['credential_id']}"
        call("POST", cred, {"auth": {"type": "environment_variable", "secret_value": token}})
        res = call("POST", "/v1/sessions", body) or {}
        sid = res.get("id")
        if not isinstance(sid, str) or not sid:
            raise Refusal("the API returned no session")
        print(f"operator-wake: session started ({msg.mode})")
        outcome = watch(call, sid, deps.get("monotonic", time.monotonic), deps.get("sleep", time.sleep))
        print(f"operator-wake: session {'ended' if outcome == 'done' else 'stopped'}")
        if outcome != "done":
            escalate(env, msg.id, outcome, gh_post)
        return 0
    except (Exception, Refusal) as e:
        escalate(env, msg.id, f"the bridge failed ({type(e).__name__}); no session runs", gh_post)
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
