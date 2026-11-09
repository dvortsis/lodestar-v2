"""fleet.yml's live mode (OP-2): its own Terraform entry point and the live backend.

Only when FLEET_LIVE=true; nothing here runs in CI or dry-run. The `.ps1`
scripts stay phrase-only (plan, Loosenings).

- Terraform runs through `Terraform`: init with the S3 backend's
  -backend-config, apply and destroy with -lock-timeout, a few retries on a
  held state lock and never `force-unlock`. Its output goes only to the
  private evidence bucket, never to the job log.
- `LiveBackend` keeps fleet.json (tombstoned, never deleted), the monthly
  spend ledger and the mismatch flag in S3, and reaches Linode through
  linode.py (tagged resources only, account checked).
- Host keys are generated fresh at each `up` with ssh-keygen, passed to
  Terraform as a sensitive variable, and never printed. Only their public
  halves and fingerprints go into fleet.json.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import subprocess
import tempfile
from datetime import datetime
from pathlib import Path
from typing import Callable

import audit
from limits import hourly_usd
from state import (FLEET_KEY, LEDGER_KEY, MISMATCH_KEY, FleetUnknown, LedgerUnreadable, is_tombstone, tombstone,
                   validate)
from timefmt import fmt, parse_ts

TF_DIR = Path(__file__).resolve().parents[1] / "akamai" / "terraform"
TF_STATE_KEY = "terraform/lodestar.tfstate"
REGION = "us-east-1"
LINODE_SECRET = "lodestar/akamai-fleet-token"
LOCK_HELD = "Error acquiring the state lock"
ROLE_LABELS = {"main": "main", "worker": "worker-01"}  # preset role -> Terraform role label
RULE_FIELDS = ("inbound", "inbound_policy", "outbound", "outbound_policy")  # the writable part of a rule set
# Terraform's environment is built from these and the run's own values, never os.environ
# whole (which holds the job's GitHub and OIDC tokens).
TF_ENV_PASS = ("PATH", "HOME", "TMPDIR", "RUNNER_TEMP", "SSL_CERT_FILE", "HTTPS_PROXY", "NO_PROXY")


class ApplyFailed(RuntimeError):
    pass


def s3_code(e: Exception) -> str:
    return str(getattr(e, "response", {}).get("Error", {}).get("Code", ""))


class Evidence:
    """Writes job output to the private evidence bucket only."""

    def __init__(self, s3, bucket: str, run_id: str):
        self.s3, self.bucket, self.run_id = s3, bucket, run_id

    def put(self, name: str, text: str) -> None:
        try:
            self.s3.put_object(Bucket=self.bucket, Key=f"fleet/{self.run_id}/{name}.log", Body=text.encode())
        except Exception:
            pass  # losing a log never stops a destroy; the output itself is never printed instead


def pinned_tf_dir(commit: str | None, run=subprocess.run) -> Path:
    """infra/akamai/terraform at the fleet's pinned commit, from a detached git
    worktree, so a merge mid-session can't change the configuration under a
    fleet. The commit must be an ancestor of main (checked again here)."""
    from ancestry import head_of, is_ancestor  # noqa: PLC0415

    repo = str(TF_DIR.parents[2])
    if commit is None or commit == head_of("HEAD", repo):
        return TF_DIR
    if not is_ancestor(commit, "origin/main", repo):
        raise ApplyFailed("the fleet's pinned commit is not an ancestor of main")
    d = tempfile.mkdtemp(prefix="fleet-pin-")
    run(["git", "-C", repo, "worktree", "add", "--detach", d, commit], check=True, capture_output=True)
    return Path(d) / "infra" / "akamai" / "terraform"


class Terraform:
    def __init__(self, workdir: Path, env: dict, evidence: Evidence, run=subprocess.run, attempts: int = 3,
                 pin: Callable[[str | None], Path] = pinned_tf_dir):
        self.workdir, self.env, self.evidence, self.run, self.attempts, self.pin = workdir, env, evidence, run, attempts, pin

    def at(self, commit: str | None) -> "Terraform":
        """This runner, working in the configuration at `commit`."""
        return Terraform(self.pin(commit), self.env, self.evidence, self.run, self.attempts, self.pin)

    def _tf(self, args: list[str], log: str | None) -> tuple[int, str]:
        r = self.run(["terraform", *args], cwd=self.workdir, env=self.env, capture_output=True, text=True)
        out = (r.stdout or "") + (r.stderr or "")
        if log:
            self.evidence.put(log, out)
        return r.returncode, out

    def init(self, bucket: str) -> None:
        rc, _ = self._tf(["init", "-input=false", "-no-color", f"-backend-config=bucket={bucket}",
                          f"-backend-config=key={TF_STATE_KEY}", f"-backend-config=region={REGION}"], "terraform-init")
        if rc != 0:
            raise ApplyFailed("terraform init failed")

    def _with_retries(self, verb: str, varfile: str) -> None:
        for n in range(1, self.attempts + 1):
            rc, out = self._tf([verb, "-input=false", "-no-color", "-auto-approve", "-lock-timeout=5m",
                                f"-var-file={varfile}"], f"terraform-{verb}-{n}")
            if rc == 0:
                return
            if LOCK_HELD not in out:
                break
        raise ApplyFailed(f"terraform {verb} failed")

    def apply(self, varfile: str) -> None:
        self._with_retries("apply", varfile)

    def destroy(self, varfile: str) -> None:
        self._with_retries("destroy", varfile)

    def outputs(self) -> dict:
        rc, out = self._tf(["output", "-json", "-no-color"], None)  # holds addresses: never logged
        if rc != 0:
            raise ApplyFailed("terraform output failed")
        return json.loads(out)


def fingerprint(public_key: str) -> str:
    blob = base64.b64decode(public_key.split()[1])
    return "SHA256:" + base64.b64encode(hashlib.sha256(blob).digest()).decode().rstrip("=")


def make_host_key(run=subprocess.run) -> tuple[str, str]:
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "host")
        run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "", "-f", path],
            check=True, capture_output=True)
        return Path(path).read_text(), Path(path + ".pub").read_text().strip()


class LiveBackend:
    def __init__(self, s3, linode, tf: Terraform, env: dict, limits: dict, clock: Callable[[], datetime],
                 lookup_run: Callable[[str], dict | None], meta: Callable[[], dict], keygen=make_host_key):
        self.s3, self.linode, self.tf, self.env, self.limits = s3, linode, tf, env, limits
        self.clock, self.lookup_run, self.meta, self.keygen = clock, lookup_run, meta, keygen
        self.state_bucket, self.control_bucket = env["STATE_BUCKET"], env["CONTROL_BUCKET"]

    # -- S3 documents --
    def _get(self, bucket: str, key: str):
        r = self.s3.get_object(Bucket=bucket, Key=key)
        return json.loads(r["Body"].read())

    def _put(self, bucket: str, key: str, doc, **kw) -> None:
        self.s3.put_object(Bucket=bucket, Key=key, Body=json.dumps(doc).encode(), **kw)

    def read_fleet(self):
        try:
            doc = self._get(self.state_bucket, FLEET_KEY)
        except Exception as e:
            if s3_code(e) == "NoSuchKey" and not self.lodestar_linodes():
                # The first tombstone: no fleet.json yet and nothing in the account.
                try:
                    self._put(self.state_bucket, FLEET_KEY, tombstone(self.clock()), IfNoneMatch="*")
                    self._clear_mismatch()
                except Exception:
                    raise FleetUnknown("fleet.json unreadable") from None
                return None
            raise FleetUnknown("fleet.json unreadable") from None
        if is_tombstone(doc):
            return None
        if not isinstance(doc, dict) or "roles" not in doc:
            raise FleetUnknown("fleet.json is neither a fleet nor a tombstone")
        return doc

    def write_fleet(self, fleet: dict) -> None:
        self._put(self.state_bucket, FLEET_KEY, fleet)

    def mismatch(self) -> bool:
        """The flag is always present once a tombstone is written, so its meaning
        never rests on 404 versus 403. Anything but a readable false is a mismatch."""
        try:
            doc = self._get(self.control_bucket, MISMATCH_KEY)
        except Exception:
            return True
        return not (isinstance(doc, dict) and doc.get("mismatch") is False)

    def set_mismatch(self, reasons: list[str]) -> None:
        self._put(self.control_bucket, MISMATCH_KEY, {"mismatch": True, "at": fmt(self.clock()), "reasons": reasons})

    def _clear_mismatch(self) -> None:
        self._put(self.control_bucket, MISMATCH_KEY, {"mismatch": False, "at": fmt(self.clock())})

    def _ledger(self) -> dict:
        try:
            doc = self._get(self.state_bucket, LEDGER_KEY)
        except Exception as e:
            if s3_code(e) == "NoSuchKey":
                return {}
            raise LedgerUnreadable("spend ledger unreadable") from None
        if not isinstance(doc, dict) or not all(isinstance(v, (int, float)) for v in doc.values()):
            raise LedgerUnreadable("spend ledger unreadable")
        return doc

    def _accrued(self, fleet, now) -> float:
        """Up to now, not the expiry: a fleet that outlives it is still billed."""
        if not fleet:
            return 0.0
        created = parse_ts(fleet["created_at"])
        return hourly_usd(self.limits, fleet["preset"]) * max(0.0, (now - created).total_seconds() / 3600)

    def month_spend_usd(self, now) -> float:
        fleet = self.read_fleet()
        return float(self._ledger().get(now.strftime("%Y-%m"), 0.0)) + self._accrued(fleet, now)

    # -- inventory --
    def lodestar_linodes(self) -> list[dict]:
        out = []
        for n in self.linode.linodes():
            created = str(n.get("created", "")).replace("Z", "")
            out.append({"id": str(n["id"]), "created": created + "Z"})
        return out

    def lodestar_firewalls(self) -> list[dict]:
        return [{"id": str(f["id"])} for f in self.linode.firewalls()]

    def delete_linodes(self, ids) -> None:
        """The age reap: those Linodes, then any tagged firewall left with no device."""
        for lid in ids:
            self.linode.delete_linode(lid)
        for f in self.linode.firewalls():
            if not self.linode.firewall_devices(f["id"]):
                self.linode.delete_firewall(f["id"])

    # -- Terraform --
    def _tfvars(self, d: str, host_keys: dict | None) -> str:
        owner_key = os.path.join(d, "owner.pub")
        Path(owner_key).write_text(self.env["OWNER_SSH_PUBLIC_KEY"].strip() + "\n")
        tfvars = {
            "deploy_main": True, "worker_count": 1, "deployment_id": "fleet",
            "ssh_public_key_path": owner_key,
            "extra_ssh_public_keys": [self.env["FLEET_SSH_PUBLIC_KEY"].strip()],
            "ssh_admin_ipv4_cidr": self.env["OWNER_ADMIN_CIDR"].strip(),
        }
        if host_keys:
            tfvars["host_keys"] = host_keys
        path = os.path.join(d, "fleet.tfvars.json")
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump(tfvars, f)
        return path

    def create(self, fleet: dict, fence: Callable[[], None]) -> dict:
        """Record the fleet first (so its expiry holds even if apply fails), then apply."""
        fence()
        self.write_fleet(fleet)
        try:
            keys = {ROLE_LABELS[r]: dict(zip(("private_key", "public_key"), self.keygen())) for r in fleet["roles"]}
            tf = self.tf.at(fleet["commit"])
        except Exception:
            raise ApplyFailed("host keys or the pinned configuration couldn't be prepared") from None
        with tempfile.TemporaryDirectory() as d:
            varfile = self._tfvars(d, keys)
            tf.init(self.state_bucket)
            fence()
            tf.apply(varfile)
        try:
            hosts = tf.outputs().get("fleet_hosts", {}).get("value") or {}
        except Exception:
            raise ApplyFailed("terraform output couldn't be read") from None
        for role, label in ROLE_LABELS.items():
            if role in fleet["roles"]:
                h = hosts.get(label) or {}
                pub = keys[label]["public_key"]
                fleet["roles"][role] = {"address": h.get("address"), "host_key": pub, "fingerprint": fingerprint(pub),
                                        "firewall_id": h.get("firewall_id"), "admin_ipv4": h.get("admin_ipv4")}
        fence()
        self.write_fleet(fleet)
        return fleet

    def destroy(self, fence: Callable[[], None]) -> bool:
        """terraform destroy (at the fleet's pinned commit, when one is recorded),
        then delete any tagged leftovers through the API. True once no tagged
        Linode or firewall is left."""
        fence()
        try:
            fleet = self.read_fleet()
            if fleet is not None:
                validate(fleet, self.limits)
        except (FleetUnknown, ValueError):
            fleet = None  # unknown or malformed: destroy with the current configuration
        try:
            tf = self.tf.at(fleet["commit"] if fleet else None)
            with tempfile.TemporaryDirectory() as d:
                varfile = self._tfvars(d, None)
                tf.init(self.state_bucket)
                tf.destroy(varfile)
        except Exception:  # noqa: BLE001 - down is the kill path: whatever failed above,
            pass           # the fenced API sweep below still runs
        fence()
        for item, delete in [(n, self.linode.delete_linode) for n in self.linode.linodes()] + \
                            [(f, self.linode.delete_firewall) for f in self.linode.firewalls()]:
            try:
                delete(item["id"])
            except Exception:  # noqa: BLE001 - try every one; what's left is checked below
                pass
        return not self.linode.linodes() and not self.linode.firewalls()

    def finish_down(self, fleet, fence: Callable[[], None]) -> None:
        """After an empty inventory: book the spend, tombstone fleet.json, clear the flag."""
        now = self.clock()
        fence()
        self._put(self.state_bucket, FLEET_KEY, tombstone(now))  # first, so nothing below can strand it
        fence()
        self._clear_mismatch()
        if fleet:
            fence()
            ledger = self._ledger()  # LedgerUnreadable: the caller escalates; up's cap check then refuses
            month = now.strftime("%Y-%m")
            ledger[month] = round(float(ledger.get(month, 0.0)) + self._accrued(fleet, now), 4)
            self._put(self.state_bucket, LEDGER_KEY, ledger)

    # -- firewalls --
    def audit(self, fence: Callable[[], None]) -> list[str]:
        """Sweep stale job rules (fenced writes), then check every tagged firewall."""
        ranges = audit.actions_ranges(self.meta())
        try:
            fleet = self.read_fleet()
        except FleetUnknown:
            fleet = None
        admin_by_fw = {str(s.get("firewall_id")): s.get("admin_ipv4")
                       for s in (fleet or {}).get("roles", {}).values() if s.get("firewall_id")}
        linode_ids = {str(n["id"]) for n in self.linode.linodes()}
        now = self.clock()
        # Read everything first (rules, devices, every run's status), so a failure
        # in any of those calls sweeps nothing.
        plan = []
        for fw in self.linode.firewalls():
            rules = self.linode.firewall_rules(fw["id"])
            swept, n = audit.sweep(rules, self.lookup_run, now)
            plan.append((fw, swept, n, self.linode.firewall_devices(fw["id"])))
        problems = []
        for fw, swept, n, devices in plan:
            if n:
                fence()
                self.linode.put_firewall_rules(fw["id"], writable(swept))
            problems += audit.audit_firewall(fw, swept, devices, admin_by_fw.get(str(fw["id"])), ranges, linode_ids)
        return sorted(set(problems))

    def set_job_rules(self, fleet: dict, run_id: str, attempt: str, cidr: str | None, fence: Callable[[], None]) -> None:
        """Open (cidr) or close (None) this job's /32 on every fleet firewall, fenced (for OP-3)."""
        ranges = audit.actions_ranges(self.meta())
        for fid in sorted({str(s["firewall_id"]) for s in fleet["roles"].values() if s.get("firewall_id")}):
            rules = self.linode.firewall_rules(fid)
            new = audit.open_rule(rules, run_id, attempt, cidr, ranges) if cidr else audit.close_rule(rules, run_id, attempt)
            fence()
            self.linode.put_firewall_rules(fid, writable(new))


def writable(rules: dict) -> dict:
    """Only the rule set's writable fields go back in a PUT (no version or fingerprint)."""
    return {k: rules[k] for k in RULE_FIELDS if k in rules}


def known_hosts(fleet: dict) -> str:
    """Pinned known_hosts lines from fleet.json (for OP-3's SSH steps)."""
    return "".join(f"{s['address']} {s['host_key']}\n" for s in fleet["roles"].values()
                   if s.get("address") and s.get("host_key"))


LIVE_ENV = ("AWS_FLEET_ROLE_ARN", "STATE_BUCKET", "EVIDENCE_BUCKET", "CONTROL_BUCKET", "OWNER_ADMIN_CIDR",
            "FLEET_SSH_PUBLIC_KEY", "OWNER_SSH_PUBLIC_KEY", "FLEET_LINODE_ACCOUNT_EUUID", "GITHUB_TOKEN",
            "GITHUB_REPOSITORY", "ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN")


def missing_live_env(env: dict) -> list[str]:
    return sorted(k for k in LIVE_ENV if not str(env.get(k, "")).strip())


def linode_token(secret_string: str) -> str:
    """The secret is the bare token, or JSON with a `token` field."""
    s = secret_string.strip()
    if s.startswith("{"):
        s = str(json.loads(s).get("token", "")).strip()
    if not s:
        raise ValueError("the fleet token secret is empty")
    return s


def github_json(token: str, path: str) -> dict | None:  # pragma: no cover - network
    import urllib.error
    import urllib.request
    req = urllib.request.Request("https://api.github.com" + path, headers={
        "Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json", "User-Agent": "lodestar-fleet"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise RuntimeError(f"GitHub API failed: HTTP {e.code}") from None
    except Exception:
        raise RuntimeError("GitHub API failed") from None


def oidc_session(env: dict, run_id: str):  # pragma: no cover - live only
    """Assume LodestarFleetOperator with this job's GitHub OIDC token, in-process.
    No third-party action, and the credentials never reach GITHUB_ENV or a log."""
    import urllib.request

    import boto3  # noqa: PLC0415

    req = urllib.request.Request(env["ACTIONS_ID_TOKEN_REQUEST_URL"] + "&audience=sts.amazonaws.com",
                                 headers={"Authorization": f"Bearer {env['ACTIONS_ID_TOKEN_REQUEST_TOKEN']}"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            jwt = json.loads(r.read())["value"]
        creds = boto3.client("sts", region_name=REGION).assume_role_with_web_identity(
            RoleArn=env["AWS_FLEET_ROLE_ARN"], RoleSessionName=f"fleet-{run_id}", WebIdentityToken=jwt,
            DurationSeconds=3600)["Credentials"]
    except Exception:
        raise SystemExit("fleet: couldn't assume the fleet role") from None
    keys = {"aws_access_key_id": creds["AccessKeyId"], "aws_secret_access_key": creds["SecretAccessKey"],
            "aws_session_token": creds["SessionToken"]}
    return boto3.Session(region_name=REGION, **keys), keys


def live_context(env: dict, limits: dict, clock, run_id: str):  # pragma: no cover - live only
    """The live backend and the S3 lock store. Refuses unless every setting is
    there and the Linode token belongs to the fleet's account."""
    missing = missing_live_env(env)
    if missing:
        raise SystemExit("fleet: live mode needs " + ", ".join(missing))
    from linode import Linode, urllib_transport  # noqa: PLC0415
    from lock import S3Store  # noqa: PLC0415

    session, keys = oidc_session(env, run_id)  # boto3: installed only when FLEET_LIVE=true
    s3 = session.client("s3")
    try:
        secret = session.client("secretsmanager").get_secret_value(SecretId=LINODE_SECRET)
        token = linode_token(secret["SecretString"])
    except Exception:
        raise SystemExit("fleet: the fleet token couldn't be read") from None
    linode = Linode(urllib_transport(token))
    try:
        linode.check_account(env["FLEET_LINODE_ACCOUNT_EUUID"].strip())
    except Exception:
        raise SystemExit("fleet: the Linode token is not for the fleet's account") from None
    # Terraform alone gets the Linode token and the role's credentials, in a minimal environment.
    tf_env = {k: os.environ[k] for k in TF_ENV_PASS if k in os.environ}
    tf_env.update(LINODE_TOKEN=token, TF_IN_AUTOMATION="1", TF_INPUT="0", AWS_REGION=REGION,
                  AWS_ACCESS_KEY_ID=keys["aws_access_key_id"], AWS_SECRET_ACCESS_KEY=keys["aws_secret_access_key"],
                  AWS_SESSION_TOKEN=keys["aws_session_token"])
    tf = Terraform(TF_DIR, tf_env, Evidence(s3, env["EVIDENCE_BUCKET"], run_id))
    gh, repo = env["GITHUB_TOKEN"], env["GITHUB_REPOSITORY"]
    backend = LiveBackend(
        s3, linode, tf, env, limits, clock,
        lookup_run=lambda rid: github_json(gh, f"/repos/{repo}/actions/runs/{int(rid)}"),
        meta=lambda: github_json(gh, "/meta") or {},
    )
    return backend, S3Store(env["CONTROL_BUCKET"], live=True, client=s3)
