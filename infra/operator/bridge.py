"""What operator-wake and operator-apply share (OP-4; plan §2, §4).

- the settings check, which names a missing setting and never prints a value;
- the AWS connection: LodestarOperatorBridge by GitHub OIDC, in-process
  (infra/fleet/apply.py's oidc_session), with ExpectedBucketOwner on every S3
  call. The role reaches only `locks/operator.json` and
  `operator/claude-lock.json` in the control bucket, and `fleet/fleet.json`;
- claude-lock.json, which records the agent, environment, vault and
  credential IDs. It lives in the control bucket, never in the repo or a log;
- rendering the agent from its committed files and the repository variables.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Mapping

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "fleet"))

from oplock import CLAUDE_LOCK_KEY  # noqa: E402

AGENT_DIR = HERE / "agent"
MODEL_ID = re.compile(r"claude-[a-z0-9][a-z0-9.-]{0,63}\Z")
DIGITS = re.compile(r"[0-9]{1,20}\Z")
LOCK_FIELDS = ("agent_id", "agent_version", "environment_id", "vault_id", "credential_id")


class Refusal(SystemExit):
    """A fixed line, then a failing exit. Never carries a value."""

    def __init__(self, line: str):
        super().__init__(f"operator: {line}")


def missing(env: Mapping[str, str], names) -> list[str]:
    return sorted(k for k in names if not str(env.get(k, "")).strip())


def require(env: Mapping[str, str], names) -> None:
    gone = missing(env, names)
    if gone:
        raise Refusal("missing settings: " + ", ".join(gone))


def connect_store(env: Mapping[str, str], run_id: str):  # pragma: no cover - live only
    """The control-bucket store: the fleet lock's S3Store (conditional writes,
    ExpectedBucketOwner) plus `exists`, through the role's exact-prefix list
    grant. Without s3:ListBucket a GET on a missing key is a 403, not a 404, so
    a key is read only once a list says it's there; a failed list refuses."""
    import apply  # noqa: PLC0415 - infra/fleet/apply.py; needs boto3
    from lock import S3Store  # noqa: PLC0415

    session, _ = apply.oidc_session(dict(env), run_id, role_env="AWS_BRIDGE_ROLE_ARN")
    owner, bucket = env["FLEET_BUCKET_OWNER"].strip(), env["CONTROL_BUCKET"]
    raw = session.client("s3", config=apply.aws_config())
    try:
        owned = apply.OwnedS3(raw, owner)
    except ValueError:
        raise Refusal("FLEET_BUCKET_OWNER isn't a 12-digit account ID") from None

    class BridgeStore(S3Store):
        def exists(self, key: str) -> bool:
            try:
                r = raw.list_objects_v2(Bucket=bucket, Prefix=key, MaxKeys=1, ExpectedBucketOwner=owner)
            except Exception:
                raise Refusal("the control bucket couldn't be listed") from None
            return any(o.get("Key") == key for o in r.get("Contents") or [])

    return BridgeStore(bucket, live=True, client=owned)


def read_claude_lock(store) -> tuple[dict | None, str | None]:
    """(lock, etag); (None, None) when it doesn't exist yet. Malformed refuses."""
    if hasattr(store, "exists") and not store.exists(CLAUDE_LOCK_KEY):
        return None, None
    cur = store.get(CLAUDE_LOCK_KEY)
    if cur is None:
        return None, None
    try:
        d = json.loads(cur[0])
        ok = isinstance(d, dict) and all(isinstance(d.get(k), str) and d[k] for k in LOCK_FIELDS if k != "agent_version") \
            and isinstance(d.get("agent_version"), int) and not isinstance(d.get("agent_version"), bool)
    except ValueError:
        ok = False
    if not ok:
        raise Refusal("claude-lock.json is malformed; fix it by hand")
    return {k: d[k] for k in LOCK_FIELDS}, cur[1]


def render(env: Mapping[str, str]) -> tuple[dict, dict, dict]:
    """(agent, environment, vault) bodies from the committed files."""
    model = env.get("OPERATOR_MODEL_ROUTINE", "").strip()
    if not MODEL_ID.match(model):
        raise Refusal("OPERATOR_MODEL_ROUTINE isn't a model ID")
    ids = {k: env.get(k, "").strip() for k in ("RUNNER_USER_ID", "OPERATOR_BOT_USER_ID", "CHANNEL_PR")}
    for k, v in ids.items():
        if not DIGITS.match(v):
            raise Refusal(f"{k} isn't numeric")
    prompt = (AGENT_DIR / "system-prompt.md").read_text(encoding="utf-8")
    for k, v in ids.items():
        prompt = prompt.replace("{{" + k + "}}", v)
    if "{{" in prompt:
        raise Refusal("the system prompt has an unfilled placeholder")
    agent = json.loads((AGENT_DIR / "agent.json").read_text(encoding="utf-8"))
    agent["model"]["id"], agent["system"] = model, prompt
    environment = json.loads((AGENT_DIR / "environment.json").read_text(encoding="utf-8"))
    vault = json.loads((AGENT_DIR / "vault.json").read_text(encoding="utf-8"))
    return agent, environment, vault
