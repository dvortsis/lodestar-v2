#!/usr/bin/env python3
"""Install the Akamai PAT into the precreated AWS Secrets Manager secret without CLI argument leakage."""
import getpass
import json
import logging
import os
import shutil
import subprocess
import sys
from pathlib import Path

import boto3
from botocore.exceptions import BotoCoreError, ClientError

ACCOUNT = os.environ.get("LODESTAR_AWS_ACCOUNT_ID", "").strip()
REGION = "us-east-1"
SECRET = "lodestar/akamai-token"
PROFILE = "lodestar-admin"
LOG = Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Lodestar" / "logs" / "akamai-secret-setup.log"


def configure_log():
    LOG.parent.mkdir(parents=True, exist_ok=True)
    logging.basicConfig(filename=LOG, level=logging.INFO,
                        format="%(asctime)s %(levelname)s %(message)s")


def main():
    configure_log()
    if not (ACCOUNT.isdecimal() and len(ACCOUNT) == 12):
        raise RuntimeError("Set LODESTAR_AWS_ACCOUNT_ID to the expected 12-digit AWS account ID before running the installer")
    aws = shutil.which("aws") or r"C:\Program Files\Amazon\AWSCLIV2\aws.exe"
    if not Path(aws).is_file():
        raise RuntimeError("AWS CLI not found. Install AWS CLI v2 and login as lodestar-admin.")
    proc = subprocess.run([aws, "configure", "export-credentials", "--profile", PROFILE,
                           "--format", "process"], text=True, capture_output=True,
                          timeout=40, check=False)
    if proc.returncode != 0:
        logging.error("AWS CLI credential export failed: exit_code=%d", proc.returncode)
        raise RuntimeError("AWS admin session unavailable. Run: aws login --profile lodestar-admin --region us-east-1")
    try:
        creds = json.loads(proc.stdout)
        if creds.get("Version") != 1 or not creds.get("SessionToken"):
            raise RuntimeError("Temporary AWS credential format invalid or session token missing")
        session = boto3.Session(aws_access_key_id=creds["AccessKeyId"],
                                aws_secret_access_key=creds["SecretAccessKey"],
                                aws_session_token=creds["SessionToken"],
                                region_name=REGION)
        sts = session.client("sts")
        arn = sts.get_caller_identity().get("Arn")
        expected = f"arn:aws:iam::{ACCOUNT}:user/{PROFILE}"
        if arn != expected:
            raise RuntimeError(f"Refusing to use unexpected AWS identity: {arn}")
        sm = session.client("secretsmanager")
        metadata = sm.describe_secret(SecretId=SECRET)
        if metadata.get("Name") != SECRET:
            raise RuntimeError("Secret name mismatch")
        if metadata.get("DeletedDate") is not None:
            raise RuntimeError("Secret is scheduled for deletion")
        print(f"Authenticated securely as {PROFILE} in AWS account {ACCOUNT}.")
        print(f"Installing Akamai PAT into {SECRET}, region {REGION}.")
        token = getpass.getpass("Paste the Akamai API token (input hidden): ")
        if not isinstance(token, str) or not (20 <= len(token) <= 512) or any(c.isspace() for c in token):
            raise RuntimeError("Akamai token format invalid. No secret updated.")
        sm.put_secret_value(SecretId=SECRET, SecretString=token)
        token = None
        updated = sm.describe_secret(SecretId=SECRET)
        versions = updated.get("VersionIdsToStages", {})
        if not any("AWSCURRENT" in stages for stages in versions.values()):
            raise RuntimeError("Secret write returned but AWSCURRENT version not verified")
        logging.info("Akamai secret version installed and verified, account=%s secret=%s", ACCOUNT, SECRET)
        print("SUCCESS: Secret version installed and AWSCURRENT verified. No token printed or logged.")
        print("Next: Run a Lambda dry-run to verify the Akamai API inventory read.")
    finally:
        creds = None
        proc = None


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.TimeoutExpired, ClientError, BotoCoreError, OSError, KeyError, ValueError) as exc:
        logging.error("Secret installation failed: %s", type(exc).__name__)
        print("ERROR: Token installation was not verified. Details: " + str(exc).splitlines()[0][:240], file=sys.stderr)
        sys.exit(1)
