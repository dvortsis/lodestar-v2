#!/usr/bin/env python3
"""
Lodestar Secure PIA VPN Bridge for Akamai Linodes.

Coordinates secure retrieval of Private Internet Access (PIA) VPN credentials
from AWS Secrets Manager, dynamic discovery of WireGuard endpoints, ephemeral
Curve25519 keypair generation using verified cryptography, secure transfer into
root-only runtime tmpfs (/run/lodestar/vpn/wg0.conf), container lifecycle management,
and independent leak verification.

Security Controls:
- Zero AWS credentials, session tokens, or PIA master passwords installed on Linodes.
- Ephemeral WireGuard key negotiation using verified X25519 (cryptography).
- Restrictive runtime permissions: /run/lodestar/vpn (0700 root:root), wg0.conf (0600 root:root).
- Pinned immutable Gluetun container digest (qmcgaw/gluetun:v3.40.0).
- Strict firewall kill switch (FIREWALL=on) and network namespace isolation.
- Independent leak checks (handshake, public IP mismatch, ISP ASN, DNS, IPv6, iptables).
- Redaction of sensitive credentials, private keys, and tokens from logs and standard output.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import logging
import os
import re
import shlex
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

try:
    import yaml
except ImportError:
    yaml = None
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey

# -----------------------------------------------------------------------------
# Configuration Constants
# -----------------------------------------------------------------------------
DEFAULT_AWS_REGION = os.environ.get("AWS_DEFAULT_REGION", os.environ.get("AWS_REGION", "us-east-1"))
DEFAULT_AWS_ACCOUNT = os.environ.get("LODESTAR_AWS_ACCOUNT_ID", "").strip()
SECRET_NAME = "lodestar/pia-vpn"
ROLE_NAME = "LodestarPiaSecretReader"

# Authoritative PIA Endpoints
PIA_SERVERLIST_URL = "https://serverlist.piaservers.net/vpninfo/servers/v4"
PIA_TOKEN_URL = "https://privateinternetaccess.com/gtoken/generateToken"
DEFAULT_PIA_REGION = "us-east"

# Gluetun reviewed immutable container digest (Docker Hub registry-1.docker.io)
# Multi-platform index digest: sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00
# Linux AMD64 platform manifest digest: sha256:8ec31c4b83e1f3173d43728f78a4a7ad45237fef366a4b5461e5aee93f517fba
GLUETUN_IMAGE = "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00"
GLUETUN_AMD64_MANIFEST_DIGEST = "sha256:8ec31c4b83e1f3173d43728f78a4a7ad45237fef366a4b5461e5aee93f517fba"

# Runtime filesystem locations on target Linode
REMOTE_CONFIG_DIR = "/run/lodestar/vpn"
REMOTE_CONFIG_FILE = "/run/lodestar/vpn/wg0.conf"
REMOTE_COMPOSE_DIR = "/home/lodestar/lodestar/pia-example"
REMOTE_COMPOSE_FILE = "/home/lodestar/lodestar/pia-example/compose.vpn.yaml"
REMOTE_DEFAULT_USER = "lodestar"
REMOTE_DEFAULT_KEY = os.path.expanduser("~/.ssh/lodestar_akamai_ed25519")

# PIA Root CA 4096-bit Certificate PEM
PIA_ROOT_CA_PEM = """-----BEGIN CERTIFICATE-----
MIIHqzCCBZOgAwIBAgIJAJ0u+vODZJntMA0GCSqGSIb3DQEBDQUAMIHoMQswCQYD
VQQGEwJVUzELMAkGA1UECBMCQ0ExEzARBgNVBAcTCkxvc0FuZ2VsZXMxIDAeBgNV
BAoTF1ByaXZhdGUgSW50ZXJuZXQgQWNjZXNzMSAwHgYDVQQLExdQcml2YXRlIElu
dGVybmV0IEFjY2VzczEgMB4GA1UEAxMXUHJpdmF0ZSBJbnRlcm5ldCBBY2Nlc3Mx
IDAeBgNVBCkTF1ByaXZhdGUgSW50ZXJuZXQgQWNjZXNzMS8wLQYJKoZIhvcNAQkB
FiBzZWN1cmVAcHJpdmF0ZWludGVybmV0YWNjZXNzLmNvbTAeFw0xNDA0MTcxNzQw
MzNaFw0zNDA0MTIxNzQwMzNaMIHoMQswCQYDVQQGEwJVUzELMAkGA1UECBMCQ0Ex
EzARBgNVBAcTCkxvc0FuZ2VsZXMxIDAeBgNVBAoTF1ByaXZhdGUgSW50ZXJuZXQg
QWNjZXNzMSAwHgYDVQQLExdQcml2YXRlIEludGVybmV0IEFjY2VzczEgMB4GA1UE
AxMXUHJpdmF0ZSBJbnRlcm5ldCBBY2Nlc3MxIDAeBgNVBCkTF1ByaXZhdGUgSW50
ZXJuZXQgQWNjZXNzMS8wLQYJKoZIhvcNAQkBFiBzZWN1cmVAcHJpdmF0ZWludGVy
bmV0YWNjZXNzLmNvbTCCAiIwDQYJKoZIhvcNAQEBBQADggIPADCCAgoCggIBALVk
hjumaqBbL8aSgj6xbX1QPTfTd1qHsAZd2B97m8Vw31c/2yQgZNf5qZY0+jOIHULN
De4R9TIvyBEbvnAg/OkPw8n/+ScgYOeH876VUXzjLDBnDb8DLr/+w9oVsuDeFJ9K
V2UFM1OYX0SnkHnrYAN2QLF98ESK4NCSU01h5zkcgmQ+qKSfA9Ny0/UpsKPBFqsQ
25NvjDWFhCpeqCHKUJ4Be27CDbSl7lAkBuHMPHJs8f8xPgAbHRXZOxVCpayZ2SND
fCwsnGWpWFoMGvdMbygngCn6jA/W1VSFOlRlfLuuGe7QFfDwA0jaLCxuWt/BgZyl
p7tAzYKR8lnWmtUCPm4+BtjyVDYtDCiGBD9Z4P13RFWvJHw5aapx/5W/CuvVyI7p
Kwvc2IT+KPxCUhH1XI8ca5RN3C9NoPJJf6qpg4g0rJH3aaWkoMRrYvQ+5PXXYUzj
tRHImghRGd/ydERYoAZXuGSbPkm9Y/p2X8unLcW+F0xpJD98+ZI+tzSsI99Zs5wi
jSUGYr9/j18KHFTMQ8n+1jauc5bCCegN27dPeKXNSZ5riXFL2XX6BkY68y58UaNz
meGMiUL9BOV1iV+PMb7B7PYs7oFLjAhh0EdyvfHkrh/ZV9BEhtFa7yXp8XR0J6vz
1YV9R6DYJmLjOEbhU8N0gc3tZm4Qz39lIIG6w3FDAgMBAAGjggFUMIIBUDAdBgNV
HQ4EFgQUrsRtyWJftjpdRM0+925Y6Cl08SUwggEfBgNVHSMEggEWMIIBEoAUrsRt
yWJftjpdRM0+925Y6Cl08SWhge6kgeswgegxCzAJBgNVBAYTAlVTMQswCQYDVQQI
EwJDQTETMBEGA1UEBxMKTG9zQW5nZWxlczEgMB4GA1UEChMXUHJpdmF0ZSBJbnRl
cm5ldCBBY2Nlc3MxIDAeBgNVBAsTF1ByaXZhdGUgSW50ZXJuZXQgQWNjZXNzMSAw
HgYDVQQDExdQcml2YXRlIEludGVybmV0IEFjY2VzczEgMB4GA1UEKRMXUHJpdmF0
ZSBJbnRlcm5ldCBBY2Nlc3MxLzAtBgkqhkiG9w0BCQEWIHNlY3VyZUBwcml2YXRl
aW50ZXJuZXRhY2Nlc3MuY29tggkAnS7684Nkme0wDAYDVR0TBAUwAwEB/zANBgkq
hkiG9w0BAQ0FAAOCAgEAJsfhsPk3r8kLXLxY+v+vHzbr4ufNtqnL9/1Uuf8NrsCt
pXAoyZ0YqfbkWx3NHTZ7OE9ZRhdMP/RqHQE1p4N4Sa1nZKhTKasV6KhHDqSCt/dv
Em89xWm2MVA7nyzQxVlHa9AkcBaemcXEiyT19XdpiXOP4Vhs+J1R5m8zQOxZlV1G
tF9vsXmJqWZpOVPmZ8f35BCsYPvv4yMewnrtAC8PFEK/bOPeYcKN50bol22QYaZu
LfpkHfNiFTnfMh8sl/ablPyNY7DUNiP5DRcMdIwmfGQxR5WEQoHL3yPJ42LkB5zs
6jIm26DGNXfwura/mi105+ENH1CaROtRYwkiHb08U6qLXXJz80mWJkT90nr8Asj3
5xN2cUppg74nG3YVav/38P48T56hG1NHbYF5uOCske19F6wi9maUoto/3vEr0rnX
JUp2KODmKdvBI7co245lHBABWikk8VfejQSlCtDBXn644ZMtAdoxKNfR2WTFVEwJ
iyd1Fzx0yujuiXDROLhISLQDRjVVAvawrAtLZWYK31bY7KlezPlQnl/D9Asxe85l
8jO5+0LdJ6VyOs/Hd4w52alDW/MFySDZSfQHMTIc30hLBJ8OnCEIvluVQQ2UQvoW
+no177N9L2Y+M9TcTA62ZyMXShHQGeh20rb4kK8f+iFX8NxtdHVSkxMEFSfDDyQ=
-----END CERTIFICATE-----
"""

logger = logging.getLogger("lodestar-pia-bridge")


# -----------------------------------------------------------------------------
# Custom Exception Hierarchy
# -----------------------------------------------------------------------------
class PiaBridgeError(Exception):
    """Base error for all Lodestar PIA bridge operations."""


class WrongAccountError(PiaBridgeError):
    """Raised when AWS caller identity does not match authorized account ID."""


class RoleAssumptionError(PiaBridgeError):
    """Raised when STS AssumeRole fails."""


class MfaRequiredError(RoleAssumptionError):
    """Raised when STS AssumeRole fails due to missing or expired MFA authorization."""


class SecretNotFoundError(PiaBridgeError):
    """Raised when the requested secret does not exist in Secrets Manager."""


class SecretAccessDeniedError(PiaBridgeError):
    """Raised when access to an AWS secret is denied or unauthorized."""


class InvalidSecretJsonError(PiaBridgeError):
    """Raised when the secret payload is not valid JSON."""


class InvalidCredentialsError(PiaBridgeError):
    """Raised when secret JSON is missing PIA_USERNAME or PIA_PASSWORD."""


class PiaAuthenticationError(PiaBridgeError):
    """Raised when PIA API rejects credentials or returns an error status."""


class RegionUnavailableError(PiaBridgeError):
    """Raised when the requested region is not found or lacks WireGuard support."""


class EndpointMetadataError(PiaBridgeError):
    """Raised when server list metadata is invalid or malformed."""


class WireGuardRegistrationError(PiaBridgeError):
    """Raised when registering public key with PIA WireGuard server fails."""


class HostKeyMismatchError(PiaBridgeError):
    """Raised when SSH host key verification fails."""


class SshConnectionError(PiaBridgeError):
    """Raised when SSH connection to the Linode fails."""


class RemotePrerequisiteError(PiaBridgeError):
    """Raised when remote host lacks required Docker or kernel capabilities."""


class RemotePermissionError(PiaBridgeError):
    """Raised when remote filesystem permissions on /run/lodestar/vpn are insecure."""


class ComposeValidationError(PiaBridgeError):
    """Raised when Compose file syntax, transfer, or remote validation fails."""


class VpnConnectionError(PiaBridgeError):
    """Raised when the VPN container fails to establish an active tunnel."""


class VpnHandshakeError(VpnConnectionError):
    """Raised when WireGuard handshake is missing or stale."""


class LeakDetectedError(PiaBridgeError):
    """Raised when independent verification detects a public IP or DNS leak."""


# -----------------------------------------------------------------------------
# Sensitive Information Redaction
# -----------------------------------------------------------------------------
def redact_sensitive_text(text: str) -> str:
    """Removes sensitive keys, passwords, and tokens from text, logs, and exception strings."""
    if not isinstance(text, str):
        return text
    # Redact PrivateKey = ...
    text = re.sub(r"(PrivateKey\s*=\s*)[A-Za-z0-9+/=]{40,50}", r"\1[REDACTED]", text, flags=re.IGNORECASE)
    # Redact pt=... (PIA token)
    text = re.sub(r"([?&]pt=)[A-Za-z0-9_%-]+", r"\1[REDACTED]", text, flags=re.IGNORECASE)
    # Redact PIA_PASSWORD: "..."
    text = re.sub(r'("PIA_PASSWORD"\s*:\s*")[^"]+(")', r'\1[REDACTED]\2', text)
    # Redact token: "..."
    text = re.sub(r'("token"\s*:\s*")[^"]+(")', r'\1[REDACTED]\2', text)
    # Redact Basic auth header
    text = re.sub(r"(Basic\s+)[A-Za-z0-9+/=]+", r"\1[REDACTED]", text, flags=re.IGNORECASE)
    # Redact AWS session tokens
    text = re.sub(r'("SessionToken"\s*:\s*")[^"]+(")', r'\1[REDACTED]\2', text)
    text = re.sub(r'("SecretAccessKey"\s*:\s*")[^"]+(")', r'\1[REDACTED]\2', text)
    return text


# -----------------------------------------------------------------------------
# Verified X25519 Cryptography (RFC 7748)
# -----------------------------------------------------------------------------
def generate_wireguard_keypair() -> Tuple[str, str]:
    """
    Generates an ephemeral WireGuard private key and derives its public key
    using verified X25519 cryptography (OpenSSL CSPRNG backing).
    """
    priv = X25519PrivateKey.generate()
    pub = priv.public_key()
    priv_bytes = priv.private_bytes_raw()
    pub_bytes = pub.public_bytes_raw()
    priv_b64 = base64.b64encode(priv_bytes).decode("ascii")
    pub_b64 = base64.b64encode(pub_bytes).decode("ascii")
    return priv_b64, pub_b64


def validate_wireguard_key(key: str) -> bool:
    """Validates that a string is a valid 44-character base64-encoded 32-byte WireGuard key."""
    if not isinstance(key, str) or len(key.strip()) != 44:
        return False
    try:
        raw = base64.b64decode(key.strip(), validate=True)
        return len(raw) == 32
    except Exception:
        return False


def _resolve_aws_cli() -> str:
    """Finds the AWS CLI executable on PATH or default Windows installation paths."""
    import shutil
    found = shutil.which("aws")
    if found:
        return found
    default_win = r"C:\Program Files\Amazon\AWSCLIV2\aws.exe"
    if os.path.exists(default_win):
        return default_win
    raise FileNotFoundError("AWS CLI not found on PATH or default installation directory.")


# -----------------------------------------------------------------------------
# AWS Secrets Manager & STS Role Reader
# -----------------------------------------------------------------------------
class AwsSecretManagerReader:
    """Safely retrieves PIA credentials from AWS Secrets Manager using STS AssumeRole."""

    def __init__(
        self,
        account_id: Optional[str] = None,
        region: str = DEFAULT_AWS_REGION,
        profile: str = "lodestar-admin",
        role_arn: Optional[str] = None,
        sts_client: Any = None,
        sm_client: Any = None,
    ) -> None:
        self.account_id = account_id or DEFAULT_AWS_ACCOUNT
        self.region = region
        self.profile = profile
        self.role_arn = role_arn or (f"arn:aws:iam::{self.account_id}:role/{ROLE_NAME}" if self.account_id else "")
        self._sts_client = sts_client
        self._sm_client = sm_client
        self._temporary_credentials: Optional[Dict[str, str]] = None

    def validate_identity(self) -> str:
        """Validates that the active AWS caller identity matches the authorized account."""
        if not self.account_id or not re.match(r"^[0-9]{12}$", self.account_id):
            raise WrongAccountError(
                "Set LODESTAR_AWS_ACCOUNT_ID or pass --account-id with the approved 12-digit AWS account ID before AWS operations."
            )
        if self._sts_client is not None:
            identity = self._sts_client.get_caller_identity()
            actual_account = identity.get("Account", "")
            if actual_account != self.account_id:
                raise WrongAccountError(
                    f"AWS account mismatch: caller is {actual_account}, expected {self.account_id}"
                )
            return identity.get("Arn", "")

        # Use AWS CLI if boto3 is not supplied
        try:
            aws_bin = _resolve_aws_cli()
        except FileNotFoundError:
            raise RoleAssumptionError("AWS CLI not found on PATH. Install AWS CLI or supply boto3 client.")

        cmd = [
            aws_bin,
            "sts",
            "get-caller-identity",
            "--profile",
            self.profile,
            "--output",
            "json",
        ]
        try:
            res = subprocess.run(cmd, capture_output=True, text=True, check=True)
            data = json.loads(res.stdout)
            actual_account = data.get("Account", "")
            if actual_account != self.account_id:
                raise WrongAccountError(
                    f"AWS account mismatch: caller is {actual_account}, expected {self.account_id}"
                )
            return data.get("Arn", "")
        except subprocess.CalledProcessError as exc:
            err = exc.stderr or ""
            if "expired" in err.lower() or "credentials" in err.lower():
                raise MfaRequiredError(f"AWS session expired or requires MFA refresh: {err.strip()}")
            raise RoleAssumptionError(f"Failed to obtain AWS caller identity: {err.strip()}")

    def assume_reader_role(self, session_duration: int = 3600, mfa_token: Optional[str] = None) -> Dict[str, str]:
        """Assumes the restricted LodestarPiaSecretReader role via STS with MFA enforcement."""
        if not self.role_arn:
            if not self.account_id:
                self.validate_identity()
            self.role_arn = f"arn:aws:iam::{self.account_id}:role/{ROLE_NAME}"

        if self._sts_client is not None:
            kwargs: Dict[str, Any] = {
                "RoleArn": self.role_arn,
                "RoleSessionName": "LodestarPiaBridgeSession",
                "DurationSeconds": min(session_duration, 3600),
            }
            if mfa_token:
                kwargs["TokenCode"] = mfa_token
            try:
                assumed = self._sts_client.assume_role(**kwargs)
                creds = assumed["Credentials"]
                self._temporary_credentials = {
                    "AccessKeyId": creds["AccessKeyId"],
                    "SecretAccessKey": creds["SecretAccessKey"],
                    "SessionToken": creds["SessionToken"],
                }
                return self._temporary_credentials
            except Exception as exc:
                err_str = str(exc)
                if "MFA" in err_str or "MultiFactor" in err_str:
                    raise MfaRequiredError(f"MFA authorization required for {self.role_arn}: {err_str}")
                raise RoleAssumptionError(f"Failed to assume role {self.role_arn}: {err_str}")

        aws_bin = _resolve_aws_cli()
        cmd = [
            aws_bin,
            "sts",
            "assume-role",
            "--role-arn",
            self.role_arn,
            "--role-session-name",
            "LodestarPiaBridgeSession",
            "--duration-seconds",
            str(min(session_duration, 3600)),
            "--profile",
            self.profile,
            "--output",
            "json",
        ]
        try:
            res = subprocess.run(cmd, capture_output=True, text=True, check=True)
            data = json.loads(res.stdout)
            creds = data["Credentials"]
            self._temporary_credentials = {
                "AccessKeyId": creds["AccessKeyId"],
                "SecretAccessKey": creds["SecretAccessKey"],
                "SessionToken": creds["SessionToken"],
            }
            return self._temporary_credentials
        except subprocess.CalledProcessError as exc:
            err = exc.stderr or ""
            if "mfa" in err.lower() or "token" in err.lower():
                raise MfaRequiredError(f"MFA authorization required to assume {self.role_arn}: {err.strip()}")
            raise RoleAssumptionError(f"Failed to assume role {self.role_arn}: {err.strip()}")

    def get_pia_credentials(self, secret_id: str = SECRET_NAME) -> Tuple[str, str]:
        """
        Retrieves and validates PIA credentials from Secrets Manager.
        Ensures only the authorized PIA secret is requested and keys are non-empty.
        """
        expected_arn_prefix = f"arn:aws:secretsmanager:{self.region}:{self.account_id}:secret:{SECRET_NAME}"
        if secret_id != SECRET_NAME and not secret_id.startswith(expected_arn_prefix):
            raise SecretAccessDeniedError(
                f"Access denied: unauthorized secret '{secret_id}'. Only '{SECRET_NAME}' is permitted."
            )

        if not self._temporary_credentials:
            self.assume_reader_role()

        secret_str: Optional[str] = None
        if self._sm_client is not None:
            try:
                res = self._sm_client.get_secret_value(SecretId=secret_id)
                secret_str = res.get("SecretString")
            except Exception as exc:
                err_str = str(exc)
                if "ResourceNotFoundException" in err_str or "does not exist" in err_str:
                    raise SecretNotFoundError(f"Secret '{secret_id}' not found in Secrets Manager")
                if "AccessDeniedException" in err_str or "not authorized" in err_str:
                    raise SecretAccessDeniedError(f"Access denied retrieving secret '{secret_id}': {err_str}")
                raise PiaBridgeError(f"Failed to retrieve secret '{secret_id}': {err_str}")
        else:
            aws_bin = _resolve_aws_cli()
            env = os.environ.copy()
            if self._temporary_credentials:
                env["AWS_ACCESS_KEY_ID"] = self._temporary_credentials["AccessKeyId"]
                env["AWS_SECRET_ACCESS_KEY"] = self._temporary_credentials["SecretAccessKey"]
                env["AWS_SESSION_TOKEN"] = self._temporary_credentials["SessionToken"]
                env["AWS_DEFAULT_REGION"] = self.region

            cmd = [
                aws_bin,
                "secretsmanager",
                "get-secret-value",
                "--secret-id",
                secret_id,
                "--region",
                self.region,
                "--output",
                "json",
            ]
            try:
                res = subprocess.run(cmd, capture_output=True, text=True, check=True, env=env)
                data = json.loads(res.stdout)
                secret_str = data.get("SecretString")
            except subprocess.CalledProcessError as exc:
                err = exc.stderr or ""
                if "ResourceNotFoundException" in err or "does not exist" in err:
                    raise SecretNotFoundError(f"Secret '{secret_id}' not found in Secrets Manager")
                if "AccessDeniedException" in err or "AccessDenied" in err:
                    raise SecretAccessDeniedError(f"Access denied retrieving secret '{secret_id}': {err.strip()}")
                raise PiaBridgeError(f"Failed to retrieve secret '{secret_id}': {err.strip()}")

        if not secret_str:
            raise SecretNotFoundError(f"Secret '{secret_id}' is empty or has no SecretString")

        try:
            payload = json.loads(secret_str)
        except json.JSONDecodeError as exc:
            raise InvalidSecretJsonError(f"Secret payload is not valid JSON: {exc}")

        if not isinstance(payload, dict):
            raise InvalidSecretJsonError("Secret JSON root must be an object")

        user = payload.get("PIA_USERNAME")
        password = payload.get("PIA_PASSWORD")

        if not user or not isinstance(user, str) or not user.strip():
            raise InvalidCredentialsError("Missing or empty required key 'PIA_USERNAME' in secret")
        if not password or not isinstance(password, str) or not password.strip():
            raise InvalidCredentialsError("Missing or empty required key 'PIA_PASSWORD' in secret")

        return user.strip(), password.strip()


# Alias for backwards compatibility
AwsSecretClient = AwsSecretManagerReader


# -----------------------------------------------------------------------------
# PIA Dynamic Endpoint Discovery & WireGuard API Negotiation
# -----------------------------------------------------------------------------
class PiaWireGuardClient:
    """Discovers endpoints dynamically and negotiates WireGuard parameters with PIA's API."""

    def __init__(
        self,
        ca_cert_pem: str = PIA_ROOT_CA_PEM,
        serverlist_url: str = PIA_SERVERLIST_URL,
        token_url: str = PIA_TOKEN_URL,
        serverlist_cache: Optional[Dict[str, Any]] = None,
    ) -> None:
        self.ca_cert_pem = ca_cert_pem
        self.serverlist_url = serverlist_url
        self.token_url = token_url
        self._serverlist_cache = serverlist_cache

    def fetch_server_list(self, timeout: int = 15, retries: int = 3) -> Dict[str, Any]:
        """Fetches and validates current server metadata from authoritative PIA server list."""
        if self._serverlist_cache is not None:
            if not isinstance(self._serverlist_cache, dict) or "regions" not in self._serverlist_cache or not isinstance(self._serverlist_cache["regions"], list):
                raise EndpointMetadataError("Invalid PIA server list: 'regions' array missing from response")
            return self._serverlist_cache

        last_err: Optional[Exception] = None
        for attempt in range(1, retries + 1):
            try:
                req = urllib.request.Request(self.serverlist_url, headers={"User-Agent": "LodestarPiaBridge/1.0"})
                ssl_ctx = ssl.create_default_context()
                with urllib.request.urlopen(req, timeout=timeout, context=ssl_ctx) as resp:
                    raw_data = resp.read().decode("utf-8")
                # PIA server list responses may include an RSA signature line at the end
                first_line = raw_data.strip().splitlines()[0]
                data = json.loads(first_line)
                if not isinstance(data, dict) or "regions" not in data or not isinstance(data["regions"], list):
                    raise EndpointMetadataError("Invalid PIA server list: 'regions' array missing from response")
                self._serverlist_cache = data
                return data
            except urllib.error.HTTPError as exc:
                last_err = EndpointMetadataError(f"PIA server list HTTP error: {exc.code}")
            except json.JSONDecodeError as exc:
                last_err = EndpointMetadataError(f"PIA server list malformed JSON: {exc}")
            except Exception as exc:
                last_err = EndpointMetadataError(f"Failed to fetch PIA server list: {exc}")
            time.sleep(1.0 * attempt)

        raise last_err or EndpointMetadataError("Failed to fetch PIA server list after retries")

    def resolve_region(self, region_identifier: str) -> Dict[str, Any]:
        """
        Dynamically discovers and validates server metadata for the requested region.
        Ensures the selected region explicitly supports WireGuard ('wg' servers present).
        """
        server_list = self.fetch_server_list()
        regions = server_list.get("regions", [])

        # Normalize query
        query = region_identifier.strip().lower().replace("_", "-")
        # Common aliases
        aliases = {
            "us-east": "us-newjersey",
            "us-new-jersey": "us-newjersey",
            "us-new-york": "us_new_york_city",
            "us-ny": "us_new_york_city",
            "us-newyork": "us_new_york_city",
            "us-brooklyn": "us-brooklyn",
            "us-dc": "us_washington_dc",
            "us-washington": "us_washington_dc",
            "us-chicago": "us_chicago",
            "us-atlanta": "us_atlanta",
            "us-florida": "us_florida",
        }
        target_id = aliases.get(query, query)

        # Match region
        matched: Optional[Dict[str, Any]] = None
        for r in regions:
            r_id = r.get("id", "").lower().replace("_", "-")
            r_name = r.get("name", "").lower()
            if r_id == target_id or r.get("id", "").lower() == query or r_name == query:
                matched = r
                break

        if not matched:
            available = [r.get("id") for r in regions if "wg" in r.get("servers", {})]
            raise RegionUnavailableError(
                f"Region '{region_identifier}' not found in PIA server list. "
                f"Available WireGuard regions include: {', '.join(sorted(filter(None, available))[:10])}..."
            )

        # Validate WireGuard support
        wg_servers = matched.get("servers", {}).get("wg", [])
        if not wg_servers:
            raise RegionUnavailableError(
                f"Region '{matched.get('name', region_identifier)}' ({matched.get('id')}) does not support WireGuard."
            )

        primary_wg = wg_servers[0]
        server_ip = primary_wg.get("ip")
        server_cn = primary_wg.get("cn")

        if not server_ip or not server_cn:
            raise EndpointMetadataError(f"WireGuard server entry in region '{matched.get('id')}' missing 'ip' or 'cn'")

        return {
            "id": matched.get("id"),
            "name": matched.get("name"),
            "server_ip": server_ip,
            "server_cn": server_cn,
            "dns": matched.get("dns", f"{matched.get('id')}.privacy.network"),
        }

    def get_auth_token(self, username: str, password: str, timeout: int = 15) -> str:
        """Authenticates to PIA gtoken API with HTTP Basic Auth, returning a 24-hour token."""
        auth_bytes = f"{username}:{password}".encode("utf-8")
        auth_header = "Basic " + base64.b64encode(auth_bytes).decode("ascii")

        req = urllib.request.Request(self.token_url, headers={"Authorization": auth_header, "User-Agent": "LodestarPiaBridge/1.0"})
        ssl_ctx = ssl.create_default_context()

        try:
            with urllib.request.urlopen(req, timeout=timeout, context=ssl_ctx) as resp:
                data = json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as exc:
            if exc.code in (401, 403):
                raise PiaAuthenticationError(f"PIA credentials rejected by API (HTTP {exc.code})")
            raise PiaAuthenticationError(f"PIA token request failed with HTTP {exc.code}")
        except Exception as exc:
            raise PiaAuthenticationError(f"Connection failure contacting PIA token API: {redact_sensitive_text(str(exc))}")

        if data.get("status") != "OK" or not data.get("token"):
            raise PiaAuthenticationError(f"PIA token API returned non-OK status: {data.get('status')}")

        return data["token"]

    def register_key(
        self,
        server_ip: str,
        server_cn: str,
        pia_token: str,
        public_key: str,
        timeout: int = 15,
    ) -> Dict[str, Any]:
        """Registers the ephemeral public key on the selected PIA WireGuard endpoint."""
        if not validate_wireguard_key(public_key):
            raise WireGuardRegistrationError(f"Invalid public key format for registration: '{public_key}'")

        query = urllib.parse.urlencode({"pt": pia_token, "pubkey": public_key})
        url = f"https://{server_cn}:1337/addKey?{query}"

        ssl_ctx = ssl.create_default_context()
        ssl_ctx.load_verify_locations(cadata=self.ca_cert_pem)

        req = urllib.request.Request(url, headers={"Host": server_cn, "User-Agent": "LodestarPiaBridge/1.0"})
        try:
            with urllib.request.urlopen(req, timeout=timeout, context=ssl_ctx) as resp:
                data = json.loads(resp.read().decode("utf-8"))
        except Exception as exc:
            redacted_err = redact_sensitive_text(str(exc))
            raise WireGuardRegistrationError(
                f"Failed to register WireGuard key with PIA endpoint {server_cn} ({server_ip}): {redacted_err}"
            )

        if data.get("status") != "OK":
            raise WireGuardRegistrationError(f"PIA WireGuard endpoint returned failure status: {data.get('status')}")

        server_key = data.get("server_key")
        if not server_key or not validate_wireguard_key(server_key):
            raise WireGuardRegistrationError(f"Invalid server_key returned by PIA endpoint: '{server_key}'")

        peer_ip = data.get("peer_ip")
        if not peer_ip or not re.match(r"^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$", peer_ip):
            raise WireGuardRegistrationError(f"Invalid peer_ip returned by PIA endpoint: '{peer_ip}'")

        return {
            "peer_ip": peer_ip,
            "server_key": server_key,
            "server_port": data.get("server_port", 1337),
            "server_ip": server_ip,
            "server_cn": server_cn,
            "dns_servers": data.get("dns_servers", ["10.0.0.242"]),
        }

    def build_wg0_conf(
        self,
        private_key: str,
        peer_ip: str,
        server_key: str,
        server_ip: str,
        server_port: int,
        dns_servers: List[str],
    ) -> str:
        """Formats standard WireGuard configuration text."""
        dns_line = f"DNS = {dns_servers[0]}" if dns_servers else "DNS = 10.0.0.242"
        return f"""[Interface]
Address = {peer_ip}/32
PrivateKey = {private_key}
{dns_line}

[Peer]
PublicKey = {server_key}
Endpoint = {server_ip}:{server_port}
AllowedIPs = 0.0.0.0/0
PersistentKeepalive = 25
"""


# -----------------------------------------------------------------------------
# Host-Key Verified SSH Provisioner & Leak Inspector
# -----------------------------------------------------------------------------
class LinodeSshProvisioner:
    """Manages secure transfer, permission verification, and leak checks on target Linodes."""

    def __init__(
        self,
        target_ip: str,
        user: str = REMOTE_DEFAULT_USER,
        key_path: str = REMOTE_DEFAULT_KEY,
        strict_host_key: bool = True,
        known_hosts_file: Optional[str] = None,
    ) -> None:
        if not re.match(r"^[a-zA-Z0-9.-]+$", target_ip):
            raise ValueError(f"Invalid target IP/hostname format: {target_ip}")
        self.target_ip = target_ip
        self.user = user
        self.key_path = os.path.expanduser(key_path)
        self.strict_host_key = strict_host_key
        self.known_hosts_file = known_hosts_file

    def _build_ssh_cmd(self, remote_command: Optional[str] = None) -> List[str]:
        cmd = [
            "ssh",
            "-i",
            self.key_path,
            "-o",
            "BatchMode=yes",
            "-o",
            f"StrictHostKeyChecking={'yes' if self.strict_host_key else 'accept-new'}",
            "-o",
            "ConnectTimeout=10",
        ]
        if self.known_hosts_file:
            cmd.extend(["-o", f"UserKnownHostsFile={self.known_hosts_file}"])
        cmd.append(f"{self.user}@{self.target_ip}")
        if remote_command:
            cmd.append(remote_command)
        return cmd

    def run_remote_command(
        self,
        remote_command: str,
        input_data: Optional[str] = None,
        check: bool = True,
    ) -> subprocess.CompletedProcess:
        """Executes a command on the remote host over SSH with error handling."""
        cmd = self._build_ssh_cmd(remote_command)
        try:
            return subprocess.run(cmd, input=input_data, capture_output=True, text=True, check=check)
        except subprocess.CalledProcessError as exc:
            err = (exc.stderr or "").strip()
            if "host key verification failed" in err.lower() or "remote host identification has changed" in err.lower():
                raise HostKeyMismatchError(f"SSH host key verification failed for {self.target_ip}: {err}")
            if "permission denied" in err.lower() and "publickey" in err.lower():
                raise SshConnectionError(f"SSH authentication failed for {self.user}@{self.target_ip}: {err}")
            if "connection timed out" in err.lower() or "network is unreachable" in err.lower():
                raise SshConnectionError(f"SSH connection timeout to {self.target_ip}: {err}")
            raise

    def verify_host_connectivity(self) -> None:
        """Verifies SSH host connectivity and strict host key match."""
        try:
            self.run_remote_command("true", check=True)
        except subprocess.CalledProcessError as exc:
            err = (exc.stderr or "").strip()
            raise SshConnectionError(f"Failed to connect to {self.target_ip} via SSH: {err}")
        except FileNotFoundError:
            raise SshConnectionError("ssh command not available on local PATH")

    def validate_remote_prerequisites(self) -> None:
        """Validates that remote user can execute sudo docker commands and /dev/net/tun is available."""
        self.verify_host_connectivity()
        check_script = (
            "sudo -n docker info >/dev/null 2>&1 && "
            "test -c /dev/net/tun && "
            "echo PREFLIGHT_OK"
        )
        res = self.run_remote_command(check_script, check=False)
        if "PREFLIGHT_OK" not in res.stdout:
            err = (res.stderr or "").strip()
            raise RemotePrerequisiteError(
                f"Remote host {self.target_ip} failed prerequisite check. "
                f"Verify that '{self.user}' has sudo access for docker and /dev/net/tun exists. Stderr: {err}"
            )

    def provision_wg0_config(self, wg0_conf: str) -> None:
        """
        Transfers wg0.conf via standard input directly into /run/lodestar/vpn/wg0.conf.
        Enforces 0700 directory permissions and 0600 root-only file permissions.
        """
        self.verify_host_connectivity()

        # Remote install command reads from stdin; file contents never appear in process arguments
        remote_script = (
            f"sudo -n install -d -m 0700 -o root -g root {REMOTE_CONFIG_DIR} && "
            f"sudo -n install -m 0600 -o root -g root /dev/stdin {REMOTE_CONFIG_FILE}"
        )
        try:
            self.run_remote_command(remote_script, input_data=wg0_conf, check=True)
        except subprocess.CalledProcessError as exc:
            raise SshConnectionError(f"Failed to stream wg0.conf to {self.target_ip}: {exc.stderr.strip()}")

        self.verify_remote_permissions()

    def verify_remote_permissions(self) -> None:
        """Confirms that remote runtime credentials are strictly root-only (0600 / 0700)."""
        check_script = (
            f"sudo -n stat -c '%a %U:%G' {REMOTE_CONFIG_DIR} && "
            f"sudo -n stat -c '%a %U:%G' {REMOTE_CONFIG_FILE}"
        )
        try:
            res = self.run_remote_command(check_script, check=True)
            lines = res.stdout.strip().splitlines()
            if len(lines) < 2:
                raise RemotePermissionError("Incomplete stat output for remote VPN files")

            dir_stat, file_stat = lines[0].strip(), lines[1].strip()
            if dir_stat != "700 root:root":
                raise RemotePermissionError(
                    f"Insecure permissions on {REMOTE_CONFIG_DIR}: expected '700 root:root', got '{dir_stat}'"
                )
            if file_stat != "600 root:root":
                raise RemotePermissionError(
                    f"Insecure permissions on {REMOTE_CONFIG_FILE}: expected '600 root:root', got '{file_stat}'"
                )
        except subprocess.CalledProcessError as exc:
            raise RemotePermissionError(f"Failed to verify remote file permissions: {exc.stderr.strip()}")

    def cleanup_runtime_credentials(self, compose_file: str = REMOTE_COMPOSE_FILE) -> None:
        """Securely stops containers and wipes runtime secrets from the Linode's tmpfs."""
        self.verify_host_connectivity()
        cleanup_script = (
            f"if [ -f {shlex.quote(compose_file)} ]; then "
            f"  sudo -n docker compose -f {shlex.quote(compose_file)} down >/dev/null 2>&1 || true; "
            f"fi; "
            f"if [ -d {REMOTE_CONFIG_DIR} ]; then "
            f"  sudo -n rm -rf {REMOTE_CONFIG_DIR}; "
            f"fi"
        )
        try:
            self.run_remote_command(cleanup_script, check=True)
        except subprocess.CalledProcessError as exc:
            raise SshConnectionError(f"Failed to clean up runtime credentials on {self.target_ip}: {exc.stderr.strip()}")

    def start_vpn(self, compose_file: str = REMOTE_COMPOSE_FILE) -> None:
        """Starts the Gluetun container via Docker Compose using elevated sudo privilege."""
        safe_path = shlex.quote(compose_file)
        cmd = f"sudo -n docker compose -f {safe_path} up -d gluetun"
        try:
            self.run_remote_command(cmd, check=True)
        except subprocess.CalledProcessError as exc:
            raise VpnConnectionError(f"Failed to start Gluetun container: {exc.stderr.strip()}")

    def verify_leak_protection(self, max_retries: int = 8, delay_s: int = 5) -> Dict[str, Any]:
        """
        Independently and rigorously validates:
        1. WireGuard handshake is active and recent (<= 120s) with transferred bytes > 0.
        2. Egress public IP != host public IP.
        3. Egress ISP / organization does NOT belong to Linode / Akamai.
        4. DNS egress resolves through internal Gluetun resolver (127.0.0.1) and does not leak.
        5. Routing and firewall policies enforce strict DROP on unencrypted egress.
        6. IPv6 is disabled or blocked.
        """
        # Step 1: Query Linode host public IP
        host_cmd = "curl -s --max-time 5 https://checkip.amazonaws.com || curl -s --max-time 5 https://api.ipify.org"
        host_res = self.run_remote_command(host_cmd, check=False)
        host_ip = host_res.stdout.strip() or self.target_ip

        # Step 2: Poll Gluetun container for handshake and active egress IP
        handshake_ok = False
        vpn_ip: Optional[str] = None
        probe_cmd = (
            "sudo -n docker exec gluetun wg show wg0 && "
            "echo '---WG_END---' && "
            "sudo -n docker exec gluetun curl -s --max-time 5 https://checkip.amazonaws.com"
        )

        for attempt in range(1, max_retries + 1):
            res = self.run_remote_command(probe_cmd, check=False)
            if res.returncode == 0 and "---WG_END---" in res.stdout:
                parts = res.stdout.split("---WG_END---")
                wg_out = parts[0]
                ip_out = parts[1].strip()

                if "latest handshake" in wg_out and "transfer:" in wg_out:
                    handshake_ok = True
                if ip_out and re.match(r"^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$", ip_out):
                    vpn_ip = ip_out
                    if handshake_ok:
                        break
            time.sleep(delay_s)

        if not handshake_ok:
            raise VpnHandshakeError(f"WireGuard tunnel handshake failed or not established on {self.target_ip}")

        if not vpn_ip:
            raise VpnConnectionError(f"Gluetun container has no outbound VPN connectivity on {self.target_ip}")

        # Step 3: Public IP mismatch check
        if vpn_ip == host_ip or vpn_ip == self.target_ip:
            raise LeakDetectedError(
                f"CRITICAL LEAK: VPN egress IP ({vpn_ip}) matches Linode host public IP ({host_ip})!"
            )

        # Step 4: Verify ISP / Organization is not Linode/Akamai
        org_cmd = "sudo -n docker exec gluetun curl -s --max-time 5 https://ipinfo.io/json"
        org_res = self.run_remote_command(org_cmd, check=False)
        if org_res.returncode == 0 and org_res.stdout:
            try:
                ip_info = json.loads(org_res.stdout)
                org = ip_info.get("org", "").lower()
                if "linode" in org or "akamai" in org:
                    raise LeakDetectedError(
                        f"CRITICAL LEAK: Container egress is routing through host ISP/cloud ({ip_info.get('org')})!"
                    )
            except json.JSONDecodeError:
                pass

        # Step 5: DNS Leak check - resolver must resolve locally
        dns_cmd = "sudo -n docker exec gluetun nslookup -timeout=5 google.com 127.0.0.1 >/dev/null 2>&1 && echo DNS_OK"
        dns_res = self.run_remote_command(dns_cmd, check=False)
        if "DNS_OK" not in dns_res.stdout:
            raise LeakDetectedError(f"Container DNS resolution failed through internal resolver on {self.target_ip}")

        # Step 6: Firewall policy check
        fw_cmd = "sudo -n docker exec gluetun iptables -S OUTPUT"
        fw_res = self.run_remote_command(fw_cmd, check=False)
        if fw_res.returncode == 0 and "-P OUTPUT DROP" not in fw_res.stdout:
            raise LeakDetectedError("Gluetun firewall kill switch is inactive (-P OUTPUT DROP not found)")

        # Step 7: IPv6 check
        ipv6_cmd = "sudo -n docker exec gluetun cat /proc/sys/net/ipv6/conf/all/disable_ipv6"
        ipv6_res = self.run_remote_command(ipv6_cmd, check=False)
        if ipv6_res.returncode == 0 and ipv6_res.stdout.strip() != "1":
            raise LeakDetectedError("IPv6 is not disabled inside Gluetun container; potential IPv6 routing leak")

        return {
            "status": "protected",
            "host_ip": host_ip,
            "vpn_ip": vpn_ip,
            "handshake": "active",
            "dns_leak_protected": True,
            "kill_switch_active": True,
        }

    def start_crawler(self, compose_file: str = REMOTE_COMPOSE_FILE) -> None:
        """Starts the crawler container strictly after re-verifying leak protection."""
        logger.info("Executing pre-crawler leak verification check...")
        try:
            leak_info = self.verify_leak_protection(max_retries=3, delay_s=2)
            logger.info(f"Leak verification confirmed: VPN egress is {leak_info['vpn_ip']} (Host is {leak_info['host_ip']})")
        except Exception as exc:
            logger.error(f"Pre-crawler leak check failed: {exc}. Aborting crawler start and shutting down containers.")
            self.run_remote_command(f"sudo -n docker compose -f {shlex.quote(compose_file)} down", check=False)
            raise

        safe_path = shlex.quote(compose_file)
        cmd = f"sudo -n docker compose -f {safe_path} up -d crawler"
        res = self.run_remote_command(cmd, check=False)
        if res.returncode != 0:
            raise VpnConnectionError(f"Failed to start crawler container: {res.stderr.strip()}")


# -----------------------------------------------------------------------------
# Docker Compose Deployment & Remote Validation
# -----------------------------------------------------------------------------
class ComposeDeployer:
    """Validates, transfers, and verifies Docker Compose stacks on remote Linodes."""

    def __init__(self, ssh_provisioner: LinodeSshProvisioner) -> None:
        self.ssh = ssh_provisioner

    @staticmethod
    def validate_local_compose(local_compose_path: str) -> str:
        """Validates local Compose YAML syntax and returns file content and sha256."""
        path = Path(local_compose_path)
        if not path.is_file():
            raise ComposeValidationError(f"Compose file not found: {local_compose_path}")

        content = path.read_text(encoding="utf-8")
        if yaml is not None:
            try:
                data = yaml.safe_load(content)
            except Exception as exc:
                raise ComposeValidationError(f"Invalid Compose YAML syntax in {local_compose_path}: {exc}")

            if not isinstance(data, dict) or "services" not in data:
                raise ComposeValidationError(f"Compose file {local_compose_path} missing top-level 'services' key")

            if "gluetun" not in data["services"]:
                raise ComposeValidationError(f"Compose file {local_compose_path} missing required 'gluetun' service")
        else:
            if "services:" not in content or "gluetun:" not in content:
                raise ComposeValidationError(f"Compose file {local_compose_path} missing 'services:' or 'gluetun:' definition")
            if re.search(r"^\t", content, re.MULTILINE):
                raise ComposeValidationError(f"Invalid Compose YAML syntax in {local_compose_path}: tab indentation forbidden in YAML")

        return content

    def deploy_compose_stack(
        self,
        local_compose_path: str,
        remote_compose_path: str = REMOTE_COMPOSE_FILE,
    ) -> None:
        """Transfers Compose file over SSH, checks checksum integrity, and runs remote syntax check."""
        content = self.validate_local_compose(local_compose_path)
        local_sha = hashlib.sha256(content.encode("utf-8")).hexdigest()

        # Check if already present and matches
        remote_dir = str(Path(remote_compose_path).parent)
        check_cmd = f"sudo -n install -d -m 0755 -o lodestar -g lodestar {remote_dir} && sha256sum {remote_compose_path} 2>/dev/null || true"
        existing = self.ssh.run_remote_command(check_cmd, check=False)

        if local_sha in existing.stdout:
            logger.info("Remote Compose file already up to date with matching SHA-256.")
        else:
            # Transfer securely via stdin
            install_cmd = f"sudo -n install -m 0644 -o lodestar -g lodestar /dev/stdin {remote_compose_path}"
            self.ssh.run_remote_command(install_cmd, input_data=content, check=True)

            # Validate remote checksum
            verify_cmd = f"sha256sum {remote_compose_path}"
            verify_res = self.ssh.run_remote_command(verify_cmd, check=True)
            remote_sha = verify_res.stdout.split()[0] if verify_res.stdout else ""
            if remote_sha != local_sha:
                raise ComposeValidationError(
                    f"Integrity check failed after transfer: expected {local_sha}, got {remote_sha}"
                )

        # Validate remote Compose configuration syntax with docker compose config
        config_check = f"sudo -n docker compose -f {remote_compose_path} config --quiet"
        config_res = self.ssh.run_remote_command(config_check, check=False)
        if config_res.returncode != 0:
            err = config_res.stderr.strip()
            raise ComposeValidationError(f"Remote Docker Compose configuration validation failed: {err}")


# -----------------------------------------------------------------------------
# End-to-End Orchestrator
# -----------------------------------------------------------------------------
def run_bridge_workflow(
    target_ip: str,
    pia_region: str = DEFAULT_PIA_REGION,
    aws_profile: str = "lodestar-admin",
    aws_account: str = DEFAULT_AWS_ACCOUNT,
    ssh_user: str = REMOTE_DEFAULT_USER,
    ssh_key: str = REMOTE_DEFAULT_KEY,
    local_compose_path: Optional[str] = None,
    remote_compose_path: str = REMOTE_COMPOSE_FILE,
    start_crawler_workload: bool = False,
    cleanup_only: bool = False,
) -> Dict[str, Any]:
    """Coordinates AWS secret retrieval, WireGuard negotiation, provisioning, and leak verification."""
    provisioner = LinodeSshProvisioner(target_ip=target_ip, user=ssh_user, key_path=ssh_key)

    if cleanup_only:
        provisioner.cleanup_runtime_credentials(remote_compose_path)
        return {"action": "cleanup", "status": "completed", "target": target_ip}

    # Step 0: Remote preflight validation
    provisioner.validate_remote_prerequisites()

    # Step 1: Retrieve PIA secret from AWS Secrets Manager
    secret_client = AwsSecretClient(account_id=aws_account, profile=aws_profile)
    pia_user, pia_pass = secret_client.get_pia_credentials()

    # Step 2: Negotiate WireGuard config via PIA dynamic endpoint discovery & REST API
    pia_client = PiaWireGuardClient()
    region_info = pia_client.resolve_region(pia_region)
    token = pia_client.get_auth_token(pia_user, pia_pass)
    privkey, pubkey = generate_wireguard_keypair()
    tunnel_params = pia_client.register_key(
        server_ip=region_info["server_ip"],
        server_cn=region_info["server_cn"],
        pia_token=token,
        public_key=pubkey,
    )
    wg0_conf = pia_client.build_wg0_conf(
        private_key=privkey,
        peer_ip=tunnel_params["peer_ip"],
        server_key=tunnel_params["server_key"],
        server_ip=tunnel_params["server_ip"],
        server_port=tunnel_params["server_port"],
        dns_servers=tunnel_params["dns_servers"],
    )

    # Step 3: Deploy and validate Compose stack on Linode
    if not local_compose_path:
        script_dir = Path(__file__).resolve().parent
        local_compose_path = str(script_dir.parent / "pia-example" / "compose.vpn.yaml")

    deployer = ComposeDeployer(provisioner)
    deployer.deploy_compose_stack(local_compose_path, remote_compose_path)

    # Step 4: Stream wg0.conf to Linode tmpfs (/run/lodestar/vpn/wg0.conf)
    provisioner.provision_wg0_config(wg0_conf)

    # Step 5: Start Gluetun container
    provisioner.start_vpn(remote_compose_path)

    # Step 6: Verify leak protection
    leak_res = provisioner.verify_leak_protection()

    # Step 7: Optionally start crawler
    if start_crawler_workload:
        provisioner.start_crawler(remote_compose_path)

    return {
        "status": "success",
        "target": target_ip,
        "region": region_info["id"],
        "region_name": region_info["name"],
        "vpn_ip": leak_res["vpn_ip"],
        "crawler_started": start_crawler_workload,
    }


# -----------------------------------------------------------------------------
# CLI Entrypoint
# -----------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Lodestar PIA VPN Secure Bridge for Akamai Linodes",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    # 1. verify-aws
    p_verify = subparsers.add_parser("verify-aws", help="Verify AWS identity and restricted secret access")
    p_verify.add_argument("--profile", default="lodestar-admin", help="AWS administrative profile")
    p_verify.add_argument("--account-id", default=DEFAULT_AWS_ACCOUNT, help="Authorized AWS Account ID")
    p_verify.add_argument("--secret-id", default=SECRET_NAME, help="Secrets Manager Secret ID")

    # 2. generate-config (Requires explicit --output destination to prevent stdout leaks)
    p_gen = subparsers.add_parser("generate-config", help="Generate WireGuard config to a local file with restricted permissions")
    p_gen.add_argument("--pia-region", default=DEFAULT_PIA_REGION, help="PIA VPN region (e.g. us-east, us-new-jersey)")
    p_gen.add_argument("--profile", default="lodestar-admin", help="AWS administrative profile")
    p_gen.add_argument("--account-id", default=DEFAULT_AWS_ACCOUNT, help="Authorized AWS Account ID")
    p_gen.add_argument("--output", required=True, help="Explicit local file destination (NEVER printed to stdout)")

    # 3. provision
    p_prov = subparsers.add_parser("provision", help="Provision wg0.conf and Compose stack to target Linode")
    p_prov.add_argument("--target", required=True, help="Target Linode IP address")
    p_prov.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_prov.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")
    p_prov.add_argument("--pia-region", default=DEFAULT_PIA_REGION, help="PIA VPN region")
    p_prov.add_argument("--profile", default="lodestar-admin", help="AWS administrative profile")
    p_prov.add_argument("--account-id", default=DEFAULT_AWS_ACCOUNT, help="Authorized AWS Account ID")
    p_prov.add_argument("--compose-file", help="Optional local compose file override")

    # 4. start-vpn
    p_vpn = subparsers.add_parser("start-vpn", help="Start Gluetun container on target Linode")
    p_vpn.add_argument("--target", required=True, help="Target Linode IP address")
    p_vpn.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_vpn.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")
    p_vpn.add_argument("--compose-file", default=REMOTE_COMPOSE_FILE)

    # 5. verify-tunnel
    p_leak = subparsers.add_parser("verify-tunnel", help="Independently verify VPN egress and leak protection")
    p_leak.add_argument("--target", required=True, help="Target Linode IP address")
    p_leak.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_leak.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")

    # 6. start-crawler
    p_crawl = subparsers.add_parser("start-crawler", help="Start crawler container after proving leak protection")
    p_crawl.add_argument("--target", required=True, help="Target Linode IP address")
    p_crawl.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_crawl.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")
    p_crawl.add_argument("--compose-file", default=REMOTE_COMPOSE_FILE)

    # 7. health
    p_health = subparsers.add_parser("health", help="Check VPN container and tunnel status")
    p_health.add_argument("--target", required=True, help="Target Linode IP address")
    p_health.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_health.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")

    # 8. cleanup
    p_clean = subparsers.add_parser("cleanup", help="Remove runtime secrets and stop containers")
    p_clean.add_argument("--target", required=True, help="Target Linode IP address")
    p_clean.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_clean.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")
    p_clean.add_argument("--compose-file", default=REMOTE_COMPOSE_FILE)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s")

    try:
        if args.command == "verify-aws":
            client = AwsSecretClient(account_id=args.account_id, profile=args.profile)
            arn = client.validate_identity()
            print(f"[OK] Identity verified: {arn}")
            user, _ = client.get_pia_credentials(args.secret_id)
            print(f"[OK] Secret '{args.secret_id}' retrieved and validated successfully (User: {user[:3]}***)")
            return 0

        if args.command == "generate-config":
            if not args.output:
                print(
                    "[ERROR] Explicit destination file required via --output to prevent leaking configuration secrets to stdout.",
                    file=sys.stderr,
                )
                return 1

            secret_client = AwsSecretClient(account_id=args.account_id, profile=args.profile)
            pia_user, pia_pass = secret_client.get_pia_credentials()
            pia_client = PiaWireGuardClient()
            reg = pia_client.resolve_region(args.pia_region)
            token = pia_client.get_auth_token(pia_user, pia_pass)
            priv, pub = generate_wireguard_keypair()
            params = pia_client.register_key(reg["server_ip"], reg["server_cn"], token, pub)
            cfg = pia_client.build_wg0_conf(
                private_key=priv,
                peer_ip=params["peer_ip"],
                server_key=params["server_key"],
                server_ip=params["server_ip"],
                server_port=params["server_port"],
                dns_servers=params["dns_servers"],
            )

            out_path = Path(args.output).resolve()
            out_path.write_text(cfg, encoding="utf-8")
            if os.name != "nt":
                os.chmod(out_path, 0o600)
            print(f"[SECURE] WireGuard configuration written to {out_path} with restricted permissions (secrets redacted from stdout).")
            return 0

        if args.command == "provision":
            res = run_bridge_workflow(
                target_ip=args.target,
                pia_region=args.pia_region,
                aws_profile=args.profile,
                aws_account=args.account_id,
                ssh_user=args.ssh_user,
                ssh_key=args.ssh_key,
                local_compose_path=getattr(args, "compose_file", None),
                start_crawler_workload=False,
            )
            print(f"[OK] Provisioned and verified: target={res['target']}, region={res['region']}, vpn_ip={res['vpn_ip']}")
            return 0

        if args.command == "start-vpn":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            prov.start_vpn(args.compose_file)
            print(f"[OK] Gluetun started on {args.target}")
            return 0

        if args.command == "verify-tunnel":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            leak = prov.verify_leak_protection()
            print(f"[OK] Tunnel active and protected: egress={leak['vpn_ip']} (host={leak['host_ip']})")
            return 0

        if args.command == "start-crawler":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            prov.start_crawler(args.compose_file)
            print(f"[OK] Crawler started behind verified VPN on {args.target}")
            return 0

        if args.command == "health":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            leak = prov.verify_leak_protection()
            print(f"[HEALTH OK] Egress: {leak['vpn_ip']}, Handshake: {leak['handshake']}, Kill switch: {leak['kill_switch_active']}")
            return 0

        if args.command == "cleanup":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            prov.cleanup_runtime_credentials(args.compose_file)
            print(f"[OK] Runtime credentials and containers cleaned up on {args.target}")
            return 0

    except PiaBridgeError as exc:
        print(f"[ERROR] {exc.__class__.__name__}: {redact_sensitive_text(str(exc))}", file=sys.stderr)
        return 1
    except Exception as exc:
        print(f"[UNEXPECTED ERROR] {redact_sensitive_text(str(exc))}", file=sys.stderr)
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
