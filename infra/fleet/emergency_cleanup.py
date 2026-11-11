"""Invoke only the independently installed emergency broker, never Terraform."""
import os
import re
import sys
import json
from datetime import datetime, timedelta, timezone


def main(argv=None, env=None):
    env = dict(os.environ) if env is None else env
    if (sys.argv[1:] if argv is None else argv) != ["invoke"]:
        return 2
    if env.get("FLEET_CLEANUP_ENABLED") != "true":
        print("cleanup: not enabled; no live operation")
        return 0
    try:
        import apply
        function, role = env["AWS_CLEANUP_FUNCTION_ARN"], env["AWS_CLEANUP_ROLE_ARN"]
        account = re.fullmatch(r"arn:aws:iam::([0-9]{12}):role/LodestarFleetCleanupInvoker", role)
        if not account or not re.fullmatch(r"arn:aws:lambda:us-east-1:" + account[1] + r":function:[A-Za-z0-9_-]+-emergency", function):
            raise ValueError
        run = apply.github_json(env["GITHUB_TOKEN"], f"/repos/dvortsis/lodestar-v2/actions/runs/{int(env['GITHUB_RUN_ID'])}")
        requested = run["created_at"]
        when = datetime.fromisoformat(requested.replace("Z", "+00:00"))
        if not datetime.now(timezone.utc) - timedelta(minutes=10) <= when <= datetime.now(timezone.utc):
            raise ValueError
        session, _ = apply.oidc_session(env, f"cleanup-{env['GITHUB_RUN_ID']}", role_env="AWS_CLEANUP_ROLE_ARN")
        from botocore.config import Config
        client = session.client("lambda", config=Config(connect_timeout=5, read_timeout=330, retries={"total_max_attempts": 1}))
        def invoke(body):
            result = client.invoke(FunctionName=function, InvocationType="RequestResponse", Payload=json.dumps(body).encode())
            if result.get("StatusCode") != 200 or result.get("FunctionError"):
                raise ValueError
            return json.loads(result["Payload"].read(65537))
        prepared = invoke({"operation": "prepare", "requested_at": requested})
        version = prepared.get("authorization_version")
        if prepared.get("status") != "prepared" or not isinstance(version, str) or not 1 <= len(version) <= 256:
            raise ValueError
        payload = invoke({"requested_at": requested, "authorization_version": version})
        if payload.get("status") != "complete":
            raise ValueError
        print("cleanup: broker completed; owner state reconciliation remains required")
        return 0
    except (Exception, SystemExit):
        print("cleanup: failed or refused; inspect the broker alarm and scoped receipt; do not assume an empty fleet")
        return 1


if __name__ == "__main__":
    sys.exit(main())
