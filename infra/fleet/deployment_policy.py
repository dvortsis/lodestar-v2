"""Read-only checks before a live job requests OIDC/AWS credentials.

Native GitHub environment protection supplies the actual approval. This check
refuses credential use if those protections/configured claims have drifted.
The CLI audits settings with GET-only `gh api`; it changes nothing.
"""

from __future__ import annotations

import base64
import json
import subprocess
import sys
from pathlib import Path

POLICY = json.loads((Path(__file__).resolve().parents[1] / "aws" / "fleet-control" /
                     "github-deployment-policy.json").read_text(encoding="utf-8"))
REPO = POLICY["repository"]
OWNER, NAME = REPO.split("/")
PREFIX = f"repo:{OWNER}@{POLICY['owner_id']}/{NAME}@{POLICY['repository_id']}"
MAIN = "refs/heads/main"
ISSUER = "https://token.actions.githubusercontent.com"
ROLE_ENVIRONMENTS = {"AWS_FLEET_ROLE_ARN": "cloud-fleet", "AWS_PIA_ROLE_ARN": "pia-delivery",
                     "AWS_BRIDGE_ROLE_ARN": "cloud-operator"}


class DeploymentBlocked(RuntimeError):
    """Fixed, non-sensitive diagnostics only; never include HTTP bodies/tokens."""


def subject(environment: str, workflow: str) -> str:
    if workflow not in POLICY["environments"].get(environment, []):
        raise DeploymentBlocked("deployment: unapproved environment/workflow")
    return f"{PREFIX}:environment:{environment}:workflow_ref:{REPO}/.github/workflows/{workflow}@{MAIN}"


def configuration_errors(get, environments=None) -> list[str]:
    """get(path) returns parsed GitHub GET JSON. Any failed read blocks use."""
    problems = []
    root = f"/repos/{REPO}"
    try:
        repo = get(root)
        if (repo.get("id") != POLICY["repository_id"] or repo.get("owner", {}).get("id") != POLICY["owner_id"]
                or repo.get("full_name") != REPO or repo.get("default_branch") != "main"):
            problems.append("deployment: repository identity/default branch differs")
        oidc = get(root + "/actions/oidc/customization/sub")
        if any(oidc.get(k) != v for k, v in POLICY["oidc"].items()):
            problems.append("deployment: OIDC customization differs from the trust policy")
        if oidc.get("sub_claim_prefix", PREFIX) != PREFIX:
            problems.append("deployment: immutable OIDC repository prefix differs")
    except Exception:
        problems.append("deployment: repository/OIDC settings unreadable")
    for name in POLICY["environments"] if environments is None else environments:
        if name not in POLICY["environments"]:
            problems.append("deployment: unapproved environment")
            continue
        try:
            env = get(root + f"/environments/{name}")
            if env.get("deployment_branch_policy") != {"protected_branches": False, "custom_branch_policies": True}:
                problems.append(f"deployment: {name} requires selected main branch only")
            reviewers = [r for r in env.get("protection_rules", []) if r.get("type") == "required_reviewers"]
            owner_review = (len(reviewers) == 1 and
                            [(r.get("type"), r.get("reviewer", {}).get("id"))
                             for r in reviewers[0].get("reviewers", [])] == [("User", POLICY["owner_id"])])
            if not owner_review:
                problems.append(f"deployment: {name} requires the owner as its sole approval reviewer")
            if env.get("can_admins_bypass") is not False:
                problems.append(f"deployment: {name} must disable administrator approval bypass")
            # Exactly one branch rule; extra patterns/tags or a partial response fail closed.
            branches = get(root + f"/environments/{name}/deployment-branch-policies?per_page=100")
            rules = branches.get("branch_policies", [])
            if (branches.get("total_count") != 1 or len(rules) != 1
                    or rules[0].get("name") != "main" or rules[0].get("type") != "branch"):
                problems.append(f"deployment: {name} branch rules must be exactly main, with no tags")
        except Exception:
            problems.append(f"deployment: {name} protection settings unreadable")
    return problems


def require_for_run(env: dict, role_env: str, get) -> str:
    """Check job identity and live protection settings BEFORE requesting a JWT."""
    environment = ROLE_ENVIRONMENTS.get(role_env)
    workflow_ref = env.get("GITHUB_WORKFLOW_REF", "")
    allowed = [f"{REPO}/.github/workflows/{w}@{MAIN}" for w in POLICY["environments"].get(environment, [])]
    if (env.get("GITHUB_REF") != MAIN or env.get("GITHUB_REPOSITORY") != REPO
            or env.get("GITHUB_REPOSITORY_ID") != str(POLICY["repository_id"])
            or env.get("GITHUB_REPOSITORY_OWNER_ID") != str(POLICY["owner_id"]) or workflow_ref not in allowed):
        raise DeploymentBlocked("deployment: only the pinned repository workflow on main may assume this role")
    errors = configuration_errors(get, [environment])
    if errors:
        raise DeploymentBlocked("; ".join(errors))
    return environment


def require_token_claims(jwt: str, env: dict, environment: str) -> None:
    """Check actual token claims for consistency; STS still verifies the JWT signature.

    Parsing here is not authentication. No decoded token or input is logged.
    """
    try:
        if len(jwt) > 65536 or len(jwt.split(".")) != 3:
            raise ValueError
        part = jwt.split(".")[1]
        claims = json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))
        workflow = env["GITHUB_WORKFLOW_REF"].split("/.github/workflows/", 1)[1].split("@", 1)[0]
        expected = {"iss": ISSUER, "aud": "sts.amazonaws.com", "sub": subject(environment, workflow),
                    "repository": REPO, "repository_id": str(POLICY["repository_id"]),
                    "repository_owner_id": str(POLICY["owner_id"]), "environment": environment,
                    "workflow_ref": env["GITHUB_WORKFLOW_REF"], "ref": MAIN}
        if any(claims.get(k) != v for k, v in expected.items()) or "job_workflow_ref" in claims:
            raise ValueError
    except Exception:
        raise DeploymentBlocked("deployment: issued OIDC claims do not match the exact role trust") from None


def gh_get(path: str):
    # Explicit GET, no body/fields and no mutation mode; CLI authentication stays in gh.
    try:
        result = subprocess.run(["gh", "api", "--method", "GET", path], capture_output=True, text=True,
                                timeout=30, check=True)
        return json.loads(result.stdout)
    except Exception:
        raise DeploymentBlocked("deployment: GitHub settings read failed") from None


def main(argv: list[str]) -> int:
    if argv != ["audit"]:
        print("usage: deployment_policy.py audit", file=sys.stderr)
        return 2
    errors = configuration_errors(gh_get)
    for error in errors:
        print(error)
    if not errors:
        print("deployment: OIDC and owner-approved main-only environment settings match")
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
