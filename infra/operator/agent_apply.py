"""operator-apply.yml: create or update the operator's agent, environment and vault.

    python3 infra/operator/agent_apply.py apply

The plan names `ant apply`; this does the same with the API directly (see the
OP-4 decisions file: no unverified binary runs beside the workspace key). The
agent comes from infra/operator/agent/ with the routine model from
OPERATOR_MODEL_ROUTINE at medium effort. IDs go to claude-lock.json in the
control bucket, written with If-Match, under the operator lock so no session
runs while the agent changes. The vault gets its one credential, GITHUB_TOKEN,
holding a placeholder until a wake fills it. Logs print fixed lines only.
"""

from __future__ import annotations

import json
import os
import sys
from datetime import datetime, timezone

import bridge
from apiclient import anthropic
from bridge import Refusal
from oplock import CLAUDE_LOCK_KEY, OperatorLock

SETTINGS = ("ANTHROPIC_API_KEY", "AWS_BRIDGE_ROLE_ARN", "CONTROL_BUCKET", "FLEET_BUCKET_OWNER",
            "OPERATOR_MODEL_ROUTINE", "RUNNER_USER_ID", "OPERATOR_BOT_USER_ID", "CHANNEL_PR",
            "ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN")
UNSET_TOKEN = "unset-until-a-wake"


def gate(env) -> None:
    if env.get("GITHUB_EVENT_NAME") != "workflow_dispatch" or env.get("GITHUB_RUN_ATTEMPT") != "1":
        raise Refusal("apply runs only from a first-attempt workflow_dispatch")
    owner, actor = env.get("OWNER_USER_ID", ""), env.get("GITHUB_ACTOR_ID", "")
    if not (owner.isdigit() and owner == actor):
        raise Refusal("apply runs only for the owner")
    if env.get("OPERATOR_STOP", "") != "false":
        raise Refusal("OPERATOR_STOP is set")


def _id(res, prefix: str) -> str:
    rid = (res or {}).get("id")
    if not isinstance(rid, str) or not rid.startswith(prefix):
        raise Refusal("the API returned no ID")
    return rid


def apply(env, call, store) -> dict:
    agent, environment, vault = bridge.render(env)
    lock, etag = bridge.read_claude_lock(store)
    if lock is None:
        env_id = _id(call("POST", "/v1/environments", environment), "env_")
        res = call("POST", "/v1/agents", agent)
        agent_id, version = _id(res, "agent_"), res.get("version")
        vault_id = _id(call("POST", "/v1/vaults", {"display_name": vault["display_name"]}), "vlt_")
        cred = dict(vault["credential"], auth=dict(vault["credential"]["auth"], secret_value=UNSET_TOKEN))
        cred_id = _id(call("POST", f"/v1/vaults/{vault_id}/credentials", cred), "")
    else:
        env_id, vault_id, cred_id = lock["environment_id"], lock["vault_id"], lock["credential_id"]
        call("POST", f"/v1/environments/{env_id}", environment)
        # Optimistic concurrency: a 409 means someone changed the agent outside this workflow.
        res = call("POST", f"/v1/agents/{lock['agent_id']}", dict(agent, version=lock["agent_version"]))
        agent_id, version = lock["agent_id"], res.get("version") if res else None
        call("POST", f"/v1/vaults/{vault_id}", {"display_name": vault["display_name"]})
    if isinstance(version, bool) or not isinstance(version, int):
        raise Refusal("the API returned no agent version")
    new = {"agent_id": agent_id, "agent_version": version, "environment_id": env_id,
           "vault_id": vault_id, "credential_id": cred_id}
    if lock and lock.get("last_session_id"):
        new["last_session_id"] = lock["last_session_id"]  # keeps the wake's ledger cross-check
    body = json.dumps(new, indent=1).encode()
    ok = store.put_if_absent(CLAUDE_LOCK_KEY, body) if etag is None else store.put_if_match(CLAUDE_LOCK_KEY, body, etag)
    if not ok:
        raise Refusal("claude-lock.json changed during apply; run it again")
    return new


def main(argv=None, env=None, call=None, store=None, clock=None) -> int:
    env = dict(os.environ if env is None else env)
    if (argv or sys.argv[1:]) != ["apply"]:
        raise SystemExit("usage: agent_apply.py apply")
    gate(env)
    bridge.require(env, SETTINGS)
    run_id = f"{env.get('GITHUB_RUN_ID', '')}-{env.get('GITHUB_RUN_ATTEMPT', '')}"
    if store is None:  # pragma: no cover - live only
        store = bridge.connect_store(env, run_id)
    call = call or anthropic(env["ANTHROPIC_API_KEY"])
    lock = OperatorLock(store, clock or (lambda: datetime.now(timezone.utc)))
    if not lock.acquire(run_id):
        raise Refusal("a session holds the operator lock; run apply again later")
    try:
        new = apply(env, call, store)
    finally:
        lock.release(run_id)
    print(f"operator: applied; agent version {new['agent_version']}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refusal:
        raise
    except Exception as e:  # noqa: BLE001 - a traceback could carry values
        raise SystemExit(f"operator: internal error ({type(e).__name__})") from None
