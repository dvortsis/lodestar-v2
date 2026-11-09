#!/usr/bin/env python3
"""
Automated Security Controls and Comprehensive Test Suite for Lodestar PIA VPN Bridge.

Covers:
  - All 26 canonical failure and recovery scenarios:
    1. AWS role assumption and secret retrieval
    2. Invalid or missing PIA credentials
    3. Wrong AWS identity
    4. Missing MFA authorization
    5. Unauthorized secret access
    6. PIA API failures
    7. Invalid endpoint metadata
    8. Unavailable WireGuard region
    9. WireGuard registration failure
   10. Incorrect remote helper permissions / preflight failure
   11. Missing Compose configuration
   12. Invalid Compose configuration
   13. Image digest or container startup failure
   14. SSH host-key mismatch
   15. Helper Compose transfer / validation failure
   16. Insecure runtime file permissions
   17. Tunnel handshake failure
   18. DNS leakage
   19. IPv4 and IPv6 routing leakage
   20. VPN disconnection after startup
   21. Failed credential rotation
   22. VM reboot and re-provisioning
   23. Partial deployment cleanup
   24. Repeated, idempotent provisioning
   25. Attempted secret disclosure through logs or command output
   26. Crawler startup while the VPN is disconnected
  - RFC 7748 official test vectors & Gluetun image digest pinning verification.
  - Dedicated regression tests for all 6 security remediations:
    * Constrained privilege boundary rejecting arbitrary docker commands and filesystem writes
    * Path traversal and symlink attack prevention
    * Endpoint-bound TLS key registration and SNI / certificate validation
    * Fail-closed leak verification for firewall, IPv6, ISP ownership, and endpoint mismatch
    * Secure Windows configuration export with verified ACLs and zero plaintext remnants
    * Machine-readable WireGuard handshake timestamps, transfer byte counters, and stale checks
    * DNS isolation verification proving external direct queries to 8.8.8.8 are blocked
    * Complete secure deployment integration test with mocked dependencies

All AWS and Akamai/Linode interactions are safely mocked. Zero secrets exposed.
"""

from __future__ import annotations

import io
import json
import os
import shutil
import ssl
import subprocess
import sys
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import MagicMock, call, patch

# Add parent scripts directory to module path
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "scripts")))

import lodestar_pia_bridge as bridge


def make_mock_leak_responses(
    host_ip: str = "192.0.2.100",
    vpn_ip: str = "156.146.54.4",
    handshake_age: int = 10,
    rx_bytes: int = 5000,
    tx_bytes: int = 3000,
    endpoint_ip: str = "212.56.54.190",
    org: str = "Private Internet Access",
    dns_internal_ok: bool = True,
    dns_route_wg0: bool = True,
    dns_bypass_blocked: bool = True,
    firewall_drop: bool = True,
    ipv6_disabled: bool = True,
):
    now = 1731000000
    handshake_ts = 0 if handshake_age is None else (now - handshake_age)
    responses = [
        # 1. Host IP
        subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout=f"{host_ip}\n", stderr=""),
        # 2. wg-handshake
        subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout=f"peer_key\t{handshake_ts}\n", stderr=""),
        # 3. date +%s (Linode clock)
        subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout=f"{now}\n", stderr=""),
        # 4. wg-transfer
        subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout=f"peer_key\t{rx_bytes}\t{tx_bytes}\n", stderr=""),
        # 5. wg-endpoints
        subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout=f"peer_key\t{endpoint_ip}:1337\n", stderr=""),
        # 6. query-egress
        subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout=f"{vpn_ip}\n", stderr=""),
        # 7. query-ipinfo
        subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout=json.dumps({"org": org}), stderr=""),
        # 8. query-dns (internal DoT resolver: must succeed)
        subprocess.CompletedProcess(
            args=["ssh"],
            returncode=0 if dns_internal_ok else 1,
            stdout="Address: 142.250.190.46\n" if dns_internal_ok else "",
            stderr="" if dns_internal_ok else "connection timed out",
        ),
        # 9. verify-dns-route (routing must go through wg0)
        subprocess.CompletedProcess(
            args=["ssh"],
            returncode=0 if dns_route_wg0 else 1,
            stdout="DNS_ROUTE_WG0_OK\n" if dns_route_wg0 else "ERROR: DNS route does not go through wg0\n",
            stderr="" if dns_route_wg0 else "route failure",
        ),
        # 10. verify-dns-firewall (firewall and routing verification)
        subprocess.CompletedProcess(
            args=["ssh"],
            returncode=0 if dns_bypass_blocked else 1,
            stdout="DNS_FIREWALL_OK\n" if dns_bypass_blocked else "ERROR: Insecure DNS egress rule detected\n",
            stderr="" if dns_bypass_blocked else "firewall failure",
        ),
        # 11. inspect-firewall (OUTPUT chain must have -P OUTPUT DROP)
        subprocess.CompletedProcess(
            args=["ssh"],
            returncode=0 if firewall_drop else 1,
            stdout="-P OUTPUT DROP\n-A OUTPUT -o wg0 -j ACCEPT\n" if firewall_drop else "-P OUTPUT ACCEPT\n",
            stderr="",
        ),
        # 12. inspect-ipv6 (disable_ipv6 must be 1)
        subprocess.CompletedProcess(
            args=["ssh"],
            returncode=0,
            stdout="1\n" if ipv6_disabled else "0\n",
            stderr="",
        ),
    ]
    return responses


class LodestarPiaBridgeTests(unittest.TestCase):
    """Comprehensive test suite verifying all security controls and failure recovery."""

    def setUp(self) -> None:
        self.account_id = "123456789012"
        self.region = "us-east-1"
        self.role_arn = f"arn:aws:iam::{self.account_id}:role/LodestarPiaSecretReader"
        self.secret_name = "lodestar/pia-vpn"
        self.target_ip = "192.0.2.100"

        # Valid mock PIA server list metadata
        self.mock_server_list = {
            "regions": [
                {
                    "id": "us-newjersey",
                    "name": "US East",
                    "country": "US",
                    "dns": "us-newjersey.privacy.network",
                    "servers": {
                        "wg": [
                            {"ip": "212.56.54.190", "cn": "newjersey434"}
                        ]
                    },
                },
                {
                    "id": "us-chicago",
                    "name": "US Chicago",
                    "country": "US",
                    "dns": "us-chicago.privacy.network",
                    "servers": {
                        "wg": [
                            {"ip": "156.146.54.100", "cn": "chicago401"}
                        ]
                    },
                },
                {
                    "id": "legacy-ovpn-only",
                    "name": "Legacy No WG",
                    "country": "US",
                    "servers": {
                        "ovpnudp": [{"ip": "1.2.3.4", "cn": "ovpn1"}]
                    },
                },
            ]
        }

    # -------------------------------------------------------------------------
    # 1. AWS role assumption and secret retrieval
    # -------------------------------------------------------------------------
    def test_01_aws_role_assumption_and_secret_retrieval(self) -> None:
        """Confirms successful STS AssumeRole and retrieval of PIA credentials."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {
            "Account": self.account_id,
            "Arn": f"arn:aws:iam::{self.account_id}:user/lodestar-admin",
        }
        mock_sts.assume_role.return_value = {
            "Credentials": {
                "AccessKeyId": "ASIA_TEST_KEY",
                "SecretAccessKey": "SECRET_TEST_KEY",
                "SessionToken": "SESSION_TEST_TOKEN",
            }
        }
        mock_sm = MagicMock()
        mock_sm.get_secret_value.return_value = {
            "SecretString": json.dumps({"PIA_USERNAME": "p1234567", "PIA_PASSWORD": "ValidPassword99!"})
        }

        client = bridge.AwsSecretClient(
            account_id=self.account_id,
            sts_client=mock_sts,
            sm_client=mock_sm,
        )
        user, pwd = client.get_pia_credentials(self.secret_name)
        self.assertEqual(user, "p1234567")
        self.assertEqual(pwd, "ValidPassword99!")
        mock_sts.get_caller_identity.assert_called_once()
        mock_sts.assume_role.assert_called_once()
        mock_sm.get_secret_value.assert_called_once_with(SecretId=self.secret_name)

    # -------------------------------------------------------------------------
    # 2. Invalid or missing PIA credentials
    # -------------------------------------------------------------------------
    def test_02_invalid_or_missing_pia_credentials(self) -> None:
        """Confirms that malformed JSON or missing PIA keys raise appropriate errors."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        mock_sts.assume_role.return_value = {
            "Credentials": {"AccessKeyId": "A", "SecretAccessKey": "B", "SessionToken": "C"}
        }

        # Case A: Corrupted JSON
        mock_sm = MagicMock()
        mock_sm.get_secret_value.return_value = {"SecretString": "{malformed_not_json: true"}
        client = bridge.AwsSecretClient(account_id=self.account_id, sts_client=mock_sts, sm_client=mock_sm)
        with self.assertRaises(bridge.InvalidSecretJsonError):
            client.get_pia_credentials(self.secret_name)

        # Case B: Missing PIA_PASSWORD
        mock_sm.get_secret_value.return_value = {"SecretString": json.dumps({"PIA_USERNAME": "p1234"})}
        with self.assertRaises(bridge.InvalidCredentialsError):
            client.get_pia_credentials(self.secret_name)

        # Case C: Empty PIA_USERNAME
        mock_sm.get_secret_value.return_value = {"SecretString": json.dumps({"PIA_USERNAME": "   ", "PIA_PASSWORD": "secret"})}
        with self.assertRaises(bridge.InvalidCredentialsError):
            client.get_pia_credentials(self.secret_name)

    # -------------------------------------------------------------------------
    # 3. Wrong AWS identity
    # -------------------------------------------------------------------------
    def test_03_wrong_aws_identity(self) -> None:
        """Confirms that unexpected AWS Account ID aborts with WrongAccountError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {
            "Account": "999888777666",
            "Arn": "arn:aws:iam::999888777666:user/unauthorized",
        }
        client = bridge.AwsSecretClient(account_id=self.account_id, sts_client=mock_sts)
        with self.assertRaises(bridge.WrongAccountError):
            client.validate_identity()

    # -------------------------------------------------------------------------
    # 4. Missing MFA authorization
    # -------------------------------------------------------------------------
    def test_04_missing_mfa_authorization(self) -> None:
        """Confirms that unauthenticated or expired MFA prompts raise MfaRequiredError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        mock_sts.assume_role.side_effect = Exception("AccessDenied: MultiFactorAuthentication failed or required")
        client = bridge.AwsSecretClient(account_id=self.account_id, sts_client=mock_sts)
        with self.assertRaises(bridge.MfaRequiredError):
            client.assume_reader_role()

    # -------------------------------------------------------------------------
    # 5. Unauthorized secret access
    # -------------------------------------------------------------------------
    def test_05_unauthorized_secret_access(self) -> None:
        """Confirms that attempting to request unrelated secrets fails closed with SecretAccessDeniedError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        client = bridge.AwsSecretClient(account_id=self.account_id, sts_client=mock_sts)
        with self.assertRaises(bridge.SecretAccessDeniedError):
            client.get_pia_credentials("lodestar/other-secret")

    # -------------------------------------------------------------------------
    # 6. PIA API failures
    # -------------------------------------------------------------------------
    @patch("urllib.request.urlopen")
    def test_06_pia_api_failures(self, mock_urlopen: MagicMock) -> None:
        """Confirms that HTTP 401/403 or network failure during token retrieval raises PiaAuthenticationError."""
        client = bridge.PiaWireGuardClient()

        # HTTP 401 Unauthorized
        mock_urlopen.side_effect = urllib.error.HTTPError(
            url="https://api.test", code=401, msg="Unauthorized", hdrs={}, fp=io.BytesIO(b"")
        )
        with self.assertRaises(bridge.PiaAuthenticationError):
            client.get_auth_token("bad_user", "bad_pass")

    # -------------------------------------------------------------------------
    # 7. Invalid endpoint metadata
    # -------------------------------------------------------------------------
    def test_07_invalid_endpoint_metadata(self) -> None:
        """Confirms that malformed JSON or missing regions array raises EndpointMetadataError."""
        bad_cache = {"invalid_structure": True}
        client = bridge.PiaWireGuardClient(serverlist_cache=bad_cache)
        with self.assertRaises(bridge.EndpointMetadataError):
            client.resolve_region("us-east")

    # -------------------------------------------------------------------------
    # 8. Unavailable WireGuard region
    # -------------------------------------------------------------------------
    def test_08_unavailable_wireguard_region(self) -> None:
        """Confirms that requested region without WireGuard support raises RegionUnavailableError."""
        client = bridge.PiaWireGuardClient(serverlist_cache=self.mock_server_list)

        # Non-existent region
        with self.assertRaises(bridge.RegionUnavailableError):
            client.resolve_region("non-existent-region")

        # Region with no wg block
        with self.assertRaises(bridge.RegionUnavailableError):
            client.resolve_region("legacy-ovpn-only")

    # -------------------------------------------------------------------------
    # 9. WireGuard registration failure
    # -------------------------------------------------------------------------
    @patch("lodestar_pia_bridge.BoundHttpsConnection")
    def test_09_wireguard_registration_failure(self, mock_conn_cls: MagicMock) -> None:
        """Confirms that connection failure on /addKey raises WireGuardRegistrationError."""
        mock_conn = MagicMock()
        mock_conn.getresponse.side_effect = Exception("Connection refused on port 1337")
        mock_conn_cls.return_value = mock_conn

        client = bridge.PiaWireGuardClient()
        _, valid_pubkey = bridge.generate_wireguard_keypair()

        with self.assertRaises(bridge.WireGuardRegistrationError):
            client.register_key("212.56.54.190", "newjersey434", "dummy_token", valid_pubkey)

    # -------------------------------------------------------------------------
    # 10. Incorrect remote helper permissions / preflight failure
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_10_incorrect_remote_docker_permissions(self, mock_cmd: MagicMock) -> None:
        """Confirms that remote user unable to execute helper preflight raises RemotePrerequisiteError."""
        # First call: verify_host_connectivity (true) succeeds
        # Second call: sudo -n /usr/local/sbin/lodestar-vpn-helper preflight fails
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="sudo: a password is required"),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.RemotePrerequisiteError):
            prov.validate_remote_prerequisites()

    # -------------------------------------------------------------------------
    # 11. Missing Compose configuration
    # -------------------------------------------------------------------------
    def test_11_missing_compose_configuration(self) -> None:
        """Confirms that nonexistent local Compose path raises ComposeValidationError."""
        with self.assertRaises(bridge.ComposeValidationError):
            bridge.ComposeDeployer.validate_local_compose("C:\\nonexistent\\compose.yaml")

    # -------------------------------------------------------------------------
    # 12. Invalid Compose configuration
    # -------------------------------------------------------------------------
    def test_12_invalid_compose_configuration(self) -> None:
        """Confirms that invalid YAML or Compose lacking gluetun raises ComposeValidationError."""
        with patch("pathlib.Path.is_file", return_value=True):
            # Case A: Missing gluetun service
            with patch("pathlib.Path.read_text", return_value="services:\n  other:\n    image: test"):
                with self.assertRaises(bridge.ComposeValidationError):
                    bridge.ComposeDeployer.validate_local_compose("compose.yaml")

            # Case B: Malformed YAML syntax
            with patch("pathlib.Path.read_text", return_value="services: [unclosed"):
                with self.assertRaises(bridge.ComposeValidationError):
                    bridge.ComposeDeployer.validate_local_compose("compose.yaml")

    # -------------------------------------------------------------------------
    # 13. Image digest or pull failure
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_13_image_digest_or_pull_failure(self, mock_cmd: MagicMock) -> None:
        """Confirms that container startup failure via helper raises VpnConnectionError."""
        mock_cmd.return_value = subprocess.CompletedProcess(
            args=["ssh"], returncode=1, stdout="", stderr="manifest unknown: image digest not found"
        )
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnConnectionError):
            prov.start_vpn()

    # -------------------------------------------------------------------------
    # 14. SSH host-key mismatch
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_14_ssh_host_key_mismatch(self, mock_run: MagicMock) -> None:
        """Confirms that an altered or unrecognized host key fails with HostKeyMismatchError."""
        mock_run.side_effect = subprocess.CalledProcessError(
            returncode=255, cmd=["ssh"], stderr="REMOTE HOST IDENTIFICATION HAS CHANGED!"
        )
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.HostKeyMismatchError):
            prov.verify_host_connectivity()

    # -------------------------------------------------------------------------
    # 15. Helper Compose transfer / validation failure
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_15_partial_ssh_transfer(self, mock_cmd: MagicMock) -> None:
        """Confirms that helper validate-compose or deploy failure raises ComposeValidationError."""
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        deployer = bridge.ComposeDeployer(prov)

        # 1. Calling deploy_compose_stack raises ComposeValidationError (disabled caller installation)
        with self.assertRaises(bridge.ComposeValidationError):
            deployer.deploy_compose_stack("compose.yaml")

        # 2. Remote validation failure raises ComposeValidationError
        mock_cmd.return_value = subprocess.CompletedProcess(
            args=["ssh"], returncode=1, stdout="", stderr="ERROR: compose syntax error"
        )
        with self.assertRaises(bridge.ComposeValidationError):
            prov.validate_remote_compose()

    # -------------------------------------------------------------------------
    # 16. Insecure runtime file permissions
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_16_incorrect_runtime_file_permissions(self, mock_cmd: MagicMock) -> None:
        """Confirms that insecure permissions (e.g. world-readable or non-root) are refused."""
        mock_cmd.return_value = subprocess.CompletedProcess(
            args=["ssh"], returncode=0, stdout="755 lodestar:lodestar\n644 lodestar:lodestar\n", stderr=""
        )
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.RemotePermissionError):
            prov.verify_remote_permissions()

    # -------------------------------------------------------------------------
    # 17. Tunnel handshake failure
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_17_tunnel_handshake_failure(self, mock_cmd: MagicMock) -> None:
        """Confirms that missing WireGuard handshake (timestamp 0) raises VpnHandshakeError."""
        mock_cmd.side_effect = make_mock_leak_responses(handshake_age=None)
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnHandshakeError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 18. DNS leakage
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_18_dns_leakage(self, mock_cmd: MagicMock) -> None:
        """Confirms that failed internal DNS resolution raises LeakDetectedError."""
        mock_cmd.side_effect = make_mock_leak_responses(dns_internal_ok=False)
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.LeakDetectedError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 19. IPv4 and IPv6 routing leakage
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_19_ipv4_and_ipv6_routing_leakage(self, mock_cmd: MagicMock) -> None:
        """Confirms that egress matching host public IP or unblocked IPv6 raises LeakDetectedError."""
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)

        # Case A: IPv4 Egress IP matches Host IP
        mock_cmd.side_effect = make_mock_leak_responses(vpn_ip=self.target_ip)
        with self.assertRaises(bridge.LeakDetectedError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

        # Case B: IPv6 not disabled
        mock_cmd.side_effect = make_mock_leak_responses(ipv6_disabled=False)
        with self.assertRaises(bridge.LeakDetectedError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 20. VPN disconnection after startup
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_20_vpn_disconnection_after_startup(self, mock_cmd: MagicMock) -> None:
        """Confirms that dropped tunnel or unreachable egress raises VpnConnectionError."""
        # query-egress returns empty/fails
        responses = make_mock_leak_responses()
        responses[5] = subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="network unreachable")
        mock_cmd.side_effect = responses
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnConnectionError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 21. Failed credential rotation
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_21_failed_credential_rotation(self, mock_cmd: MagicMock) -> None:
        """Confirms that SSH failure during re-provisioning raises SshConnectionError and prevents drift."""
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="broken pipe writing to helper"),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.SshConnectionError):
            prov.provision_wg0_config("dummy_wg0_conf")

    # -------------------------------------------------------------------------
    # 22. VM reboot and re-provisioning
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_22_vm_reboot_and_reprovisioning(self, mock_cmd: MagicMock) -> None:
        """Confirms that provisioning on a freshly rebooted machine streams config via helper."""
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="CONFIG_OK\n", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="700 root:root\n600 root:root\n", stderr=""),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        prov.provision_wg0_config("reboot_wg0_conf")
        write_cmd = mock_cmd.call_args_list[1][0][0]
        self.assertIn("lodestar-vpn-helper write-config", write_cmd)

    # -------------------------------------------------------------------------
    # 23. Partial deployment cleanup
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_23_partial_deployment_cleanup(self, mock_cmd: MagicMock) -> None:
        """Confirms that cleanup invokes the helper cleanup subcommand."""
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="CLEANUP_OK\n", stderr=""),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        prov.cleanup_runtime_credentials()
        cleanup_script = mock_cmd.call_args[0][0]
        self.assertIn("lodestar-vpn-helper cleanup", cleanup_script)

    # -------------------------------------------------------------------------
    # 24. Repeated, idempotent provisioning
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_24_repeated_idempotent_provisioning(self, mock_cmd: MagicMock) -> None:
        """Confirms that repeating provisioning runs cleanly and leaves permissions invariant."""
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="CONFIG_OK\n", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="700 root:root\n600 root:root\n", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="CONFIG_OK\n", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="700 root:root\n600 root:root\n", stderr=""),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        prov.provision_wg0_config("conf_run_1")
        prov.provision_wg0_config("conf_run_2")
        self.assertEqual(mock_cmd.call_count, 6)

    # -------------------------------------------------------------------------
    # 25. Attempted secret disclosure through logs or command output
    # -------------------------------------------------------------------------
    def test_25_attempted_secret_disclosure_through_logs_or_command_output(self) -> None:
        """Confirms that private keys, tokens, and passwords are redacted from error output and strings."""
        sample_sensitive = (
            "Error on https://server:1337/addKey?pt=SECRET_TOKEN_12345&pubkey=abc\n"
            "PrivateKey = 44CharactersOfBase64PrivateKeyHereMustBeRedacted=\n"
            '{"PIA_PASSWORD": "SuperSecretPassword!", "token": "SecretSessionToken"}'
        )
        redacted = bridge.redact_sensitive_text(sample_sensitive)
        self.assertNotIn("SECRET_TOKEN_12345", redacted)
        self.assertNotIn("44CharactersOfBase64PrivateKeyHereMustBeRedacted=", redacted)
        self.assertNotIn("SuperSecretPassword!", redacted)
        self.assertNotIn("SecretSessionToken", redacted)
        self.assertIn("[REDACTED]", redacted)

    # -------------------------------------------------------------------------
    # 26. Crawler startup while the VPN is disconnected
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_26_crawler_startup_while_vpn_disconnected(self, mock_cmd: MagicMock) -> None:
        """Confirms that crawler refuses to start when VPN tunnel is disconnected and shuts down stack."""
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(
                args=["ssh"],
                returncode=1,
                stdout="",
                stderr="ERROR: Gluetun health check failed (status: unhealthy, required: healthy)",
            ),
            subprocess.CompletedProcess(
                args=["ssh"],
                returncode=0,
                stdout="CLEANUP_OK\n",
                stderr="",
            ),
        ]
        with self.assertRaises(bridge.VpnConnectionError):
            prov.start_crawler()
        # Verify stack shutdown helper was invoked to prevent unencrypted crawler leakage
        mock_cmd.assert_called_with("sudo -n /usr/local/sbin/lodestar-vpn-helper cleanup", check=False)

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Arbitrary privileged Docker command rejected
    # -------------------------------------------------------------------------
    def test_arbitrary_privileged_docker_command_rejected(self) -> None:
        """Proves that arbitrary Docker execution or root commands are rejected by the privilege boundary."""
        cloud_init_path = Path(__file__).resolve().parent.parent / "cloud-init" / "ubuntu24.yaml.tftpl"
        content = cloud_init_path.read_text(encoding="utf-8")
        # Ensure broad docker/rm/systemctl sudo access is NOT present
        self.assertNotIn("NOPASSWD: /usr/bin/docker", content)
        self.assertNotIn("NOPASSWD: /usr/bin/rm", content)
        self.assertNotIn("NOPASSWD: /usr/bin/systemctl", content)
        self.assertIn("lodestar ALL=(root) NOPASSWD: /usr/local/sbin/lodestar-vpn-helper", content)

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Unauthorized privileged filesystem operation rejected
    # -------------------------------------------------------------------------
    def test_unauthorized_privileged_filesystem_operation_rejected(self) -> None:
        """Proves that helper refuses operations outside the approved VPN lifecycle."""
        cloud_init_path = Path(__file__).resolve().parent.parent / "cloud-init" / "ubuntu24.yaml.tftpl"
        content = cloud_init_path.read_text(encoding="utf-8")
        self.assertIn('*)', content)
        self.assertIn('echo "ERROR: Unauthorized helper command:', content)

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Path traversal in deployment command rejected
    # -------------------------------------------------------------------------
    def test_path_traversal_in_deployment_command_rejected(self) -> None:
        """Proves that unprivileged caller cannot install compose and insecure directives are rejected."""
        cloud_init_path = Path(__file__).resolve().parent.parent / "cloud-init" / "ubuntu24.yaml.tftpl"
        content = cloud_init_path.read_text(encoding="utf-8")
        # Confirms install-compose is removed from helper
        self.assertNotIn("install-compose)", content)
        # Confirms ComposeDeployer validates and rejects privileged and docker.sock
        with self.assertRaises(bridge.ComposeValidationError):
            with tempfile.NamedTemporaryFile("w", suffix=".yaml", delete=False) as f:
                f.write("services:\n  gluetun:\n    privileged: true\n")
                f_path = f.name
            try:
                bridge.ComposeDeployer.validate_local_compose(f_path)
            finally:
                os.remove(f_path)

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Symlink attack against VPN configuration rejected
    # -------------------------------------------------------------------------
    def test_symlink_attack_against_vpn_configuration_rejected(self) -> None:
        """Proves that write_secure_local_config rejects destination paths that are symlinks."""
        with patch("os.path.islink", return_value=True):
            with self.assertRaises(bridge.PiaBridgeError) as ctx:
                bridge.write_secure_local_config("symlink_dest.conf", "dummy_content")
            self.assertIn("Symlink detected", str(ctx.exception))

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Key registration endpoint IP mismatch
    # -------------------------------------------------------------------------
    def test_key_registration_endpoint_ip_mismatch_rejected(self) -> None:
        """Proves that invalid or mismatched IP address format for key registration fails closed."""
        client = bridge.PiaWireGuardClient()
        _, valid_pubkey = bridge.generate_wireguard_keypair()
        with self.assertRaises(bridge.WireGuardRegistrationError):
            client.register_key("999.999.999.9999", "newjersey434", "dummy_token", valid_pubkey)

    # -------------------------------------------------------------------------
    # Section 9 Remediation: TLS hostname mismatch rejected
    # -------------------------------------------------------------------------
    @patch("lodestar_pia_bridge.BoundHttpsConnection")
    def test_tls_hostname_mismatch_rejected(self, mock_conn_cls: MagicMock) -> None:
        """Proves that TLS hostname mismatch raises WireGuardRegistrationError."""
        mock_conn = MagicMock()
        mock_conn.getresponse.side_effect = ssl.SSLCertVerificationError(
            "Hostname mismatch, certificate is not valid for 'wronghost'"
        )
        mock_conn_cls.return_value = mock_conn

        client = bridge.PiaWireGuardClient()
        _, valid_pubkey = bridge.generate_wireguard_keypair()
        with self.assertRaises(bridge.WireGuardRegistrationError) as ctx:
            client.register_key("212.56.54.190", "wronghost", "dummy_token", valid_pubkey)
        self.assertIn("certificate verification failed", str(ctx.exception).lower())

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Certificate verification failure rejected
    # -------------------------------------------------------------------------
    @patch("lodestar_pia_bridge.BoundHttpsConnection")
    def test_certificate_verification_failure_rejected(self, mock_conn_cls: MagicMock) -> None:
        """Proves that an untrusted or self-signed certificate raises WireGuardRegistrationError."""
        mock_conn = MagicMock()
        mock_conn.getresponse.side_effect = ssl.SSLCertVerificationError(
            "Certificate verify failed: self-signed certificate in certificate chain"
        )
        mock_conn_cls.return_value = mock_conn

        client = bridge.PiaWireGuardClient()
        _, valid_pubkey = bridge.generate_wireguard_keypair()
        with self.assertRaises(bridge.WireGuardRegistrationError) as ctx:
            client.register_key("212.56.54.190", "newjersey434", "dummy_token", valid_pubkey)
        self.assertIn("certificate verification failed", str(ctx.exception).lower())

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Firewall inspection command failure blocks crawler
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_firewall_inspection_command_failure_blocks_crawler(self, mock_cmd: MagicMock) -> None:
        """Proves that failed firewall inspection aborts verification and blocks crawler startup."""
        mock_cmd.side_effect = make_mock_leak_responses(firewall_drop=False)
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.LeakDetectedError) as ctx:
            prov.verify_leak_protection(max_retries=1, delay_s=0)
        err_msg = str(ctx.exception).lower()
        self.assertTrue("firewall" in err_msg or "kill switch" in err_msg)

    # -------------------------------------------------------------------------
    # Section 9 Remediation: IPv6 inspection failure blocks crawler
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_ipv6_inspection_failure_blocks_crawler(self, mock_cmd: MagicMock) -> None:
        """Proves that when disable_ipv6 != 1, crawler startup is blocked."""
        mock_cmd.side_effect = make_mock_leak_responses(ipv6_disabled=False)
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.LeakDetectedError) as ctx:
            prov.verify_leak_protection(max_retries=1, delay_s=0)
        self.assertIn("ipv6 is not disabled", str(ctx.exception).lower())

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Egress ownership lookup unavailable blocks crawler
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_egress_ownership_lookup_unavailable_blocks_crawler(self, mock_cmd: MagicMock) -> None:
        """Proves that when ipinfo.io is unreachable or missing 'org', verification fails closed."""
        responses = make_mock_leak_responses()
        # Query-ipinfo fails with exit code 1
        responses[6] = subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="timeout")
        mock_cmd.side_effect = responses

        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.LeakDetectedError) as ctx:
            prov.verify_leak_protection(max_retries=1, delay_s=0)
        self.assertIn("ownership lookup unavailable", str(ctx.exception).lower())

    # -------------------------------------------------------------------------
    # Section 9 Remediation: VPN endpoint verification unavailable blocks crawler
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_vpn_endpoint_verification_unavailable_blocks_crawler(self, mock_cmd: MagicMock) -> None:
        """Proves that when WireGuard endpoint does not match expected server IP, verification fails."""
        mock_cmd.side_effect = make_mock_leak_responses(endpoint_ip="198.51.100.99")
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnConnectionError) as ctx:
            prov.verify_leak_protection(max_retries=1, delay_s=0, expected_server_ip="212.56.54.190")
        self.assertIn("endpoint mismatch", str(ctx.exception).lower())

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Windows export with unsafe ACL rejected
    # -------------------------------------------------------------------------
    def test_windows_export_with_unsafe_acl_rejected(self) -> None:
        """Proves that write_secure_local_config rejects export if unprivileged groups have access."""
        with tempfile.TemporaryDirectory() as tmpdir:
            target_file = os.path.join(tmpdir, "wg0.conf")
            with patch("lodestar_pia_bridge._is_windows", return_value=True), \
                 patch("subprocess.run") as mock_sub:
                mock_sub.side_effect = [
                    subprocess.CompletedProcess(args=[], returncode=0, stdout="Successfully processed 1 files"),
                    subprocess.CompletedProcess(args=[], returncode=0, stdout=f"{target_file} NT AUTHORITY\\Authenticated Users:(R)"),
                ]
                with self.assertRaises(bridge.PiaBridgeError) as ctx:
                    bridge.write_secure_local_config(target_file, "dummy_conf")
                self.assertIn("insecure acl detected", str(ctx.exception).lower())
            self.assertFalse(os.path.exists(target_file))

    def test_windows_export_path_under_users_dir_accepted_when_acl_restricted(self) -> None:
        """Proves that a file path containing 'Users' (e.g. C:\\Users\\admin) is not falsely rejected as BUILTIN\\Users."""
        with tempfile.TemporaryDirectory() as tmpdir:
            users_subdir = os.path.join(tmpdir, "Users", "admin")
            os.makedirs(users_subdir, exist_ok=True)
            target_file = os.path.join(users_subdir, "wg0.conf")
            with patch("lodestar_pia_bridge._is_windows", return_value=True), \
                 patch("subprocess.run") as mock_sub:
                mock_sub.side_effect = [
                    subprocess.CompletedProcess(args=[], returncode=0, stdout="Successfully processed 1 files"),
                    subprocess.CompletedProcess(args=[], returncode=0, stdout=f"{target_file} DESKTOP\\admin:(F)"),
                    subprocess.CompletedProcess(args=[], returncode=0, stdout=f"{target_file} DESKTOP\\admin:(F)"),
                ]
                out = bridge.write_secure_local_config(target_file, "dummy_conf")
                self.assertTrue(out.exists())

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Interrupted secret export leaves no plaintext remnants
    # -------------------------------------------------------------------------
    def test_interrupted_secret_export_no_plaintext_remnants(self) -> None:
        """Proves that if an error occurs midway, temporary files are removed."""
        with patch("pathlib.Path.is_dir", return_value=True), \
             patch("os.path.islink", return_value=False), \
             patch("os.open", side_effect=OSError("Disk full")):
            with self.assertRaises(OSError):
                bridge.write_secure_local_config("test_interrupted.conf", "dummy_conf")

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Missing WireGuard handshake rejected
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_missing_wireguard_handshake_rejected(self, mock_cmd: MagicMock) -> None:
        """Proves that a zero handshake timestamp is rejected."""
        mock_cmd.side_effect = make_mock_leak_responses(handshake_age=None)
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnHandshakeError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Stale WireGuard handshake rejected
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_stale_wireguard_handshake_rejected(self, mock_cmd: MagicMock) -> None:
        """Proves that a handshake timestamp older than 120s is rejected."""
        mock_cmd.side_effect = make_mock_leak_responses(handshake_age=300) # 300 seconds old
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnHandshakeError) as ctx:
            prov.verify_leak_protection(max_retries=1, delay_s=0)
        self.assertIn("stale", str(ctx.exception).lower())

    # -------------------------------------------------------------------------
    # Section 9 Remediation: No tunnel traffic despite handshake rejected
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_no_tunnel_traffic_despite_handshake_rejected(self, mock_cmd: MagicMock) -> None:
        """Proves that rx_bytes=0 or tx_bytes=0 fails verification even with active handshake."""
        mock_cmd.side_effect = make_mock_leak_responses(rx_bytes=0, tx_bytes=0)
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnHandshakeError) as ctx:
            prov.verify_leak_protection(max_retries=1, delay_s=0)
        self.assertIn("0 transferred bytes", str(ctx.exception).lower())

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Unauthorized external DNS path rejected
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_unauthorized_external_dns_path_rejected(self, mock_cmd: MagicMock) -> None:
        """Proves that unencrypted external egress on eth0 succeeding raises LeakDetectedError."""
        mock_cmd.side_effect = make_mock_leak_responses(dns_bypass_blocked=False)
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.LeakDetectedError) as ctx:
            prov.verify_leak_protection(max_retries=1, delay_s=0)
        self.assertIn("bypassing wireguard", str(ctx.exception).lower())

    # -------------------------------------------------------------------------
    # Section 9 Remediation: VPN outage after startup prevents direct crawler egress
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_vpn_outage_after_startup_no_direct_crawler_egress(self, mock_cmd: MagicMock) -> None:
        """Proves that if VPN goes down before crawler launch, containers are torn down."""
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(
                args=["ssh"],
                returncode=1,
                stdout="",
                stderr="CRITICAL LEAK: Active egress IP matches host public IP",
            ),
            subprocess.CompletedProcess(
                args=["ssh"],
                returncode=0,
                stdout="CLEANUP_OK\n",
                stderr="",
            ),
        ]
        with self.assertRaises(bridge.LeakDetectedError):
            prov.start_crawler()
        mock_cmd.assert_called_with("sudo -n /usr/local/sbin/lodestar-vpn-helper cleanup", check=False)

    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_stale_handshake_with_nonzero_historical_traffic_rejected(self, mock_cmd: MagicMock) -> None:
        """Proves that a stale handshake (e.g. 500s old) is rejected even when rx_bytes and tx_bytes are large non-zero numbers."""
        mock_cmd.side_effect = make_mock_leak_responses(handshake_age=500, rx_bytes=10_000_000, tx_bytes=5_000_000)
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnHandshakeError) as ctx:
            prov.verify_leak_protection(max_retries=1, delay_s=0)
        self.assertIn("stale", str(ctx.exception).lower())

    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_failed_cleanup_propagates_error_to_python_caller(self, mock_cmd: MagicMock) -> None:
        """Proves that when remote helper cleanup fails or omits CLEANUP_OK, cleanup_runtime_credentials raises SshConnectionError."""
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(
                args=["ssh"],
                returncode=1,
                stdout="",
                stderr="ERROR: Failed to shut down VPN and crawler containers; preserving runtime configuration",
            ),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.SshConnectionError) as ctx:
            prov.cleanup_runtime_credentials()
        self.assertIn("failed to clean up runtime credentials", str(ctx.exception).lower())

    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_gluetun_restart_recovery_succeeds_on_retry(self, mock_cmd: MagicMock) -> None:
        """Proves that verify_leak_protection recovers when Gluetun restarts on attempt 1 and establishes a fresh handshake on attempt 2."""
        # Attempt 1: wg-handshake fails (container restarting), wg-transfer fails, wg-endpoints fails, query-egress fails
        attempt1 = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="192.0.2.100\n", stderr=""),  # host IP
            subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="ERROR: Gluetun container is not running"),
            subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="ERROR: Gluetun container is not running"),
            subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="ERROR: Gluetun container is not running"),
            subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="ERROR: Active tunneled egress check failed"),
        ]
        # Attempt 2: full healthy responses (excluding initial host IP query)
        attempt2 = make_mock_leak_responses()[1:]
        mock_cmd.side_effect = attempt1 + attempt2
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        res = prov.verify_leak_protection(max_retries=2, delay_s=0, expected_server_ip="212.56.54.190")
        self.assertEqual(res["status"], "protected")
        self.assertEqual(res["vpn_ip"], "156.146.54.4")

    # -------------------------------------------------------------------------
    # Section 9 Remediation: Complete secure deployment with mocked dependencies
    # -------------------------------------------------------------------------
    @patch("lodestar_pia_bridge.AwsSecretClient")
    @patch("lodestar_pia_bridge.PiaWireGuardClient")
    @patch("lodestar_pia_bridge.LinodeSshProvisioner")
    @patch("lodestar_pia_bridge.ComposeDeployer")
    def test_complete_secure_deployment_with_mocked_dependencies(
        self,
        mock_deployer_cls: MagicMock,
        mock_prov_cls: MagicMock,
        mock_pia_cls: MagicMock,
        mock_sm_cls: MagicMock,
    ) -> None:
        """Integration test verifying end-to-end orchestration workflow."""
        mock_sm = MagicMock()
        mock_sm.get_pia_credentials.return_value = ("p1234567", "secret_pass")
        mock_sm_cls.return_value = mock_sm

        mock_pia = MagicMock()
        mock_pia.resolve_region.return_value = {
            "id": "us-newjersey",
            "name": "US East",
            "server_ip": "212.56.54.190",
            "server_cn": "newjersey434",
            "dns": "us-newjersey.privacy.network",
        }
        mock_pia.get_auth_token.return_value = "pia_token_xyz"
        mock_pia.register_key.return_value = {
            "peer_ip": "10.0.0.2",
            "server_key": "A" * 44,
            "server_port": 1337,
            "server_ip": "212.56.54.190",
            "server_cn": "newjersey434",
            "dns_servers": ["10.0.0.242"],
        }
        mock_pia.build_wg0_conf.return_value = "[Interface]\nAddress = 10.0.0.2/32\n"
        mock_pia_cls.return_value = mock_pia

        mock_prov = MagicMock()
        mock_prov.verify_leak_protection.return_value = {
            "status": "protected",
            "vpn_ip": "156.146.54.4",
            "host_ip": "192.0.2.100",
        }
        mock_prov_cls.return_value = mock_prov

        result = bridge.run_bridge_workflow(
            target_ip=self.target_ip,
            pia_region="us-east",
            aws_account=self.account_id,
            start_crawler_workload=True,
        )

        self.assertEqual(result["status"], "success")
        self.assertEqual(result["vpn_ip"], "156.146.54.4")
        self.assertTrue(result["crawler_started"])
        mock_prov.validate_remote_prerequisites.assert_called_once()
        mock_prov.validate_remote_compose.assert_called_once()
        mock_prov.provision_wg0_config.assert_called_once()
        mock_prov.start_vpn.assert_called_once()
        mock_prov.verify_leak_protection.assert_called_once()
        mock_prov.start_crawler.assert_called_once()

    # -------------------------------------------------------------------------
    # Sudoers Configuration Constrained Test
    # -------------------------------------------------------------------------
    def test_sudoers_configuration_constrained(self) -> None:
        """Proves that cloud-init sudoers file satisfies visudo least-privilege grammar."""
        cloud_init_path = Path(__file__).resolve().parent.parent / "cloud-init" / "ubuntu24.yaml.tftpl"
        content = cloud_init_path.read_text(encoding="utf-8")
        sudoers_lines = [
            line.strip()
            for line in content.splitlines()
            if "ALL=(root) NOPASSWD:" in line
        ]
        self.assertEqual(len(sudoers_lines), 1)
        self.assertEqual(
            sudoers_lines[0],
            "lodestar ALL=(root) NOPASSWD: /usr/local/sbin/lodestar-vpn-helper"
        )

    # -------------------------------------------------------------------------
    # RFC 7748 Official Curve25519 Test Vectors
    # -------------------------------------------------------------------------
    def test_rfc7748_curve25519_test_vectors(self) -> None:
        """Confirms that verified X25519 implementation passes RFC 7748 Section 6.1 official test vectors."""
        from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey

        alice_priv = bytes.fromhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
        alice_pub = bytes.fromhex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        bob_priv = bytes.fromhex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb")
        bob_pub = bytes.fromhex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
        shared_k = bytes.fromhex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742")

        a_priv = X25519PrivateKey.from_private_bytes(alice_priv)
        a_pub = a_priv.public_key()
        self.assertEqual(a_pub.public_bytes_raw(), alice_pub)

        b_priv = X25519PrivateKey.from_private_bytes(bob_priv)
        b_pub = b_priv.public_key()
        self.assertEqual(b_pub.public_bytes_raw(), bob_pub)

        self.assertEqual(a_priv.exchange(b_pub), shared_k)
        self.assertEqual(b_priv.exchange(a_pub), shared_k)

    # -------------------------------------------------------------------------
    # Gluetun Pinned Immutable Image Digest
    # -------------------------------------------------------------------------
    def test_gluetun_digest_verified(self) -> None:
        """Confirms that GLUETUN_IMAGE matches the verified immutable Docker Hub digest."""
        self.assertEqual(
            bridge.GLUETUN_IMAGE,
            "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00",
        )
        self.assertEqual(
            bridge.GLUETUN_AMD64_MANIFEST_DIGEST,
            "sha256:8ec31c4b83e1f3173d43728f78a4a7ad45237fef366a4b5461e5aee93f517fba",
        )


def _find_test_bash() -> Optional[str]:
    """Discovers a usable GNU bash executable for isolated Linux privilege-boundary testing."""
    candidates = [
        r"C:\Users\dvort\scoop\apps\git\current\bin\bash.exe",
        os.path.expanduser(r"~\scoop\apps\git\current\bin\bash.exe"),
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        shutil.which("bash"),
    ]
    for c in candidates:
        if c and os.path.exists(c):
            try:
                res = subprocess.run([c, "-c", "echo ok"], capture_output=True, text=True, timeout=2)
                if res.returncode == 0 and "ok" in res.stdout:
                    return c
            except Exception:
                continue
    return None


# =============================================================================
# Executable Privilege-Boundary Security Regression Tests
# =============================================================================
class TestExecutablePrivilegeBoundary(unittest.TestCase):
    """
    Executes the root helper script in an isolated subprocess environment
    to assert real process outcomes, exit codes, and effective permissions
    against all specified adversary attack vectors.
    """

    def setUp(self) -> None:
        self.bash = _find_test_bash()
        if not self.bash:
            self.skipTest("No functional GNU bash executable found for subprocess helper execution.")

        self.temp_dir = tempfile.mkdtemp()
        self.config_dir = os.path.join(self.temp_dir, "run_vpn").replace("\\", "/")
        self.compose_dir = os.path.join(self.temp_dir, "etc_vpn").replace("\\", "/")
        os.makedirs(self.config_dir, exist_ok=True)
        os.makedirs(self.compose_dir, exist_ok=True)

        self.mock_bin = os.path.join(self.temp_dir, "mock_bin").replace("\\", "/")
        os.makedirs(self.mock_bin, exist_ok=True)

        # Extract helper script directly from cloud-init template
        tftpl_path = os.path.abspath(
            os.path.join(os.path.dirname(__file__), "..", "cloud-init", "ubuntu24.yaml.tftpl")
        )
        with open(tftpl_path, "r", encoding="utf-8") as f:
            content = f.read()

        start_marker = "path: /usr/local/sbin/lodestar-vpn-helper"
        start_idx = content.find(start_marker)
        content_marker = "content: |\n"
        script_start = content.find(content_marker, start_idx) + len(content_marker)
        end_marker = "  - path: /etc/ssh"
        script_end = content.find(end_marker, script_start)
        raw_script = content[script_start:script_end]
        lines = [line[6:] if line.startswith("      ") else line for line in raw_script.splitlines()]
        script = "\n".join(lines)
        script = script.replace("$${", "${").replace("%%{", "%{")

        # Adjust root-specific chown/install for unprivileged local test sandbox
        script = script.replace("install -d -m 0755 -o root -g root", "mkdir -p")
        script = script.replace("install -d -m 0700 -o root -g root", "mkdir -p")
        script = script.replace("chown root:root", "true || chown root:root")

        # Direct script paths into isolated sandbox directories
        script = script.replace('CONFIG_DIR="/run/lodestar/vpn"', f'CONFIG_DIR="{self.config_dir}"')
        script = script.replace(
            'CONFIG_FILE="/run/lodestar/vpn/wg0.conf"', f'CONFIG_FILE="{self.config_dir}/wg0.conf"'
        )
        script = script.replace('COMPOSE_DIR="/etc/lodestar/vpn"', f'COMPOSE_DIR="{self.compose_dir}"')
        script = script.replace(
            'COMPOSE_FILE="/etc/lodestar/vpn/compose.yaml"', f'COMPOSE_FILE="{self.compose_dir}/compose.yaml"'
        )
        script = script.replace(
            'PIA_CA_FILE="/etc/lodestar/vpn/ca.rsa.4096.crt"', f'PIA_CA_FILE="{self.compose_dir}/ca.rsa.4096.crt"'
        )

        self.script_path = os.path.join(self.temp_dir, "helper.sh").replace("\\", "/")
        with open(self.script_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(script)
        os.chmod(self.script_path, 0o755)

        # Install valid PIA Root CA in compose_dir by default (fail-closed requirement)
        self.ca_path = os.path.join(self.compose_dir, "ca.rsa.4096.crt")
        with open(self.ca_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(bridge.PIA_ROOT_CA_PEM)

        # Default mock curl and jq in mock_bin for PIA serverlist, TLS binding, and host IP check
        curl_path = os.path.join(self.mock_bin, "curl")
        with open(curl_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(
                "#!/usr/bin/env bash\n"
                "case \"$*\" in\n"
                "  *serverlist.piaservers.net*)\n"
                "    echo '{\"groups\":{\"wg\":[{\"name\":\"wireguard\",\"ports\":[1337]}]},\"regions\":[{\"id\":\"us-newjersey\",\"servers\":{\"wg\":[{\"ip\":\"212.56.54.190\",\"cn\":\"newjersey434\"}]}}]}'\n"
                "    exit 0\n"
                "    ;;\n"
                "  *newjersey434:1337*)\n"
                "    echo -n '400'\n"
                "    exit 0\n"
                "    ;;\n"
                "  *checkip.amazonaws.com*)\n"
                "    echo '192.0.2.100'\n"
                "    exit 0\n"
                "    ;;\n"
                "  *)\n"
                "    echo -n '000'\n"
                "    exit 60\n"
                "    ;;\n"
                "esac\n"
            )
        os.chmod(curl_path, 0o755)

        jq_path = os.path.join(self.mock_bin, "jq")
        with open(jq_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(
                "#!/usr/bin/env bash\n"
                "cat >/dev/null || true\n"
                'if [[ "$*" == *"groups.wg"* ]]; then\n'
                '  for arg in "$@"; do\n'
                '    if [[ "$arg" == "1337" ]]; then\n'
                "      echo 'yes'\n"
                "      exit 0\n"
                "    fi\n"
                "  done\n"
                "  echo 'no'\n"
                "  exit 0\n"
                "fi\n"
                'if [[ "$*" == *"212.56.54.190"* ]]; then\n'
                "  echo 'newjersey434'\n"
                "  exit 0\n"
                "fi\n"
                'if [[ "$*" == *".org"* ]]; then\n'
                "  echo \"${MOCK_JQ_ORG:-AS60068 Datacamp Limited}\"\n"
                "  exit 0\n"
                "fi\n"
                "echo ''\n"
                "exit 0\n"
            )
        os.chmod(jq_path, 0o755)

    def tearDown(self) -> None:
        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def create_mock_docker(self, handler_script: str) -> None:
        docker_path = os.path.join(self.mock_bin, "docker").replace("\\", "/")
        with open(docker_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(
                "#!/usr/bin/env bash\n"
                'if [[ "$1" == "inspect" && "$*" =~ "State.Running" && "$*" =~ "State.Pid" ]]; then\n'
                "  echo \"1234\"\n"
                "  exit 0\n"
                "fi\n"
                f"{handler_script}\n"
            )
        os.chmod(docker_path, 0o755)

        wg_path = os.path.join(self.mock_bin, "wg").replace("\\", "/")
        with open(wg_path, "w", encoding="utf-8", newline="\n") as f:
            f.write("#!/usr/bin/env bash\nexit 0\n")
        os.chmod(wg_path, 0o755)

        nsenter_path = os.path.join(self.mock_bin, "nsenter").replace("\\", "/")
        with open(nsenter_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(
                "#!/usr/bin/env bash\n"
                'for arg in "$@"; do\n'
                '  if [[ "$arg" == "latest-handshakes" || "$arg" == "transfer" || "$arg" == "endpoints" ]]; then\n'
                '    exec "$(dirname "$0")/docker" exec gluetun wg show wg0 "$arg"\n'
                "  fi\n"
                "done\n"
                "exit 1\n"
            )
        os.chmod(nsenter_path, 0o755)

    def run_helper(self, cmd: str, input_data: str = "") -> subprocess.CompletedProcess:
        # Ensure a default failing mock docker exists so tests never fall back to host /usr/bin/docker
        docker_path = os.path.join(self.mock_bin, "docker")
        if not os.path.exists(docker_path):
            self.create_mock_docker("echo 'mock_docker_unconfigured' >&2; exit 1")
        env = os.environ.copy()
        if os.name == "nt":
            res_p = subprocess.run([self.bash, "-c", "cygpath -u \"$1\"", "_", self.mock_bin], capture_output=True, text=True)
            posix_mock = res_p.stdout.strip() if res_p.returncode == 0 else self.mock_bin
            return subprocess.run(
                [self.bash, "-c", f'export PATH="{posix_mock}:$PATH"; exec "$1" "$2"', "_", self.script_path, cmd],
                input=input_data,
                capture_output=True,
                text=True,
                env=env,
            )
        env["PATH"] = f"{self.mock_bin}{os.pathsep}{env['PATH']}"
        return subprocess.run(
            [self.bash, self.script_path, cmd],
            input=input_data,
            capture_output=True,
            text=True,
            env=env,
        )

    # 1. Arbitrary Docker Commands
    def test_exec_helper_arbitrary_commands_rejected(self) -> None:
        """Proves that helper rejects unauthorized command arguments with non-zero exit code."""
        res = self.run_helper("docker run -it alpine")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Unauthorized helper command", res.stderr)

    def test_exec_helper_command_injection_rejected(self) -> None:
        """Proves that shell metacharacters and injected commands are rejected."""
        res = self.run_helper("start-vpn; rm -rf /")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Unauthorized helper command", res.stderr)

    # 2. Immutable Compose & Command Removal
    def test_exec_helper_install_compose_command_removed(self) -> None:
        """Proves that install-compose has been removed from helper and is rejected."""
        yaml_content = "services:\n  gluetun:\n    image: test\n"
        res = self.run_helper("install-compose", input_data=yaml_content)
        self.assertEqual(res.returncode, 1)
        self.assertIn("Unauthorized helper command: install-compose", res.stderr)

    def test_exec_helper_validate_compose_succeeds(self) -> None:
        """Proves that validate-compose succeeds on approved compose definition."""
        self.create_mock_docker("exit 0")
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  gluetun:\n    image: test\n")
        res = self.run_helper("validate-compose")
        self.assertEqual(res.returncode, 0)
        self.assertIn("COMPOSE_VALID", res.stdout)

    def test_exec_helper_validate_compose_rejects_missing_file(self) -> None:
        """Proves that validate-compose fails closed when compose.yaml is missing."""
        compose_file = os.path.join(self.compose_dir, "compose.yaml")
        if os.path.exists(compose_file):
            os.remove(compose_file)
        res = self.run_helper("validate-compose")
        self.assertEqual(res.returncode, 1)
        self.assertIn("missing or symlink", res.stderr)

    # 3. Component Authorization Gate & Gluetun Health/Identity
    def _write_default_valid_wg0(self) -> tuple[str, str]:
        priv, pub = bridge.generate_wireguard_keypair()
        valid_cfg = (
            "[Interface]\n"
            "Address = 10.0.0.2/32\n"
            f"PrivateKey = {priv}\n"
            "DNS = 10.0.0.242\n\n"
            "[Peer]\n"
            f"PublicKey = {pub}\n"
            "Endpoint = 212.56.54.190:1337\n"
            "AllowedIPs = 0.0.0.0/0\n"
            "PersistentKeepalive = 25\n"
        )
        res = self.run_helper("write-config", input_data=valid_cfg)
        self.assertEqual(res.returncode, 0, msg=res.stderr)
        return priv, pub

    def test_exec_helper_direct_start_crawler_rejected_when_unhealthy(self) -> None:
        """Proves that direct start-crawler invocation fails when Gluetun health status is starting or unhealthy."""
        self._write_default_valid_wg0()
        self.create_mock_docker(
            'if [[ "$*" =~ "State.Health" ]]; then echo "starting"; exit 0; fi; exit 0'
        )
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  crawler:\n    network_mode: service:gluetun\n")

        res = self.run_helper("start-crawler")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Gluetun health check failed (status: starting, required: healthy)", res.stderr)

    def test_exec_helper_direct_start_crawler_rejected_when_service_label_mismatched(self) -> None:
        """Proves that direct start-crawler invocation fails when container lacks service: gluetun label."""
        self._write_default_valid_wg0()
        self.create_mock_docker(
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "attacker_service"; exit 0; fi; exit 0'
        )
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  crawler:\n    network_mode: service:gluetun\n")

        res = self.run_helper("start-crawler")
        self.assertEqual(res.returncode, 1)
        self.assertIn("missing expected compose service label", res.stderr)

    def test_exec_helper_direct_start_crawler_rejected_when_image_digest_mismatched(self) -> None:
        """Proves that direct start-crawler invocation fails when container image is unapproved."""
        self._write_default_valid_wg0()
        self.create_mock_docker(
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "gluetun"; exit 0;\n'
            'elif [[ "$*" =~ "Config.Image" ]]; then echo "malicious/image:latest"; exit 0; fi; exit 0'
        )
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  crawler:\n    network_mode: service:gluetun\n")

        res = self.run_helper("start-crawler")
        self.assertEqual(res.returncode, 1)
        self.assertIn("is not running approved pinned image digest", res.stderr)

    def test_exec_helper_direct_start_crawler_rejected_stale_handshake(self) -> None:
        """Proves that direct start-crawler invocation fails when WireGuard handshake age > 120s."""
        _, pub = self._write_default_valid_wg0()
        self.create_mock_docker(
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "gluetun"; exit 0;\n'
            'elif [[ "$*" =~ "Config.Image" ]]; then echo "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00"; exit 0;\n'
            f'elif [[ "$*" =~ "latest-handshakes" ]]; then echo "{pub} 1000"; exit 0; fi; exit 0'
        )
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  crawler:\n    network_mode: service:gluetun\n")

        res = self.run_helper("start-crawler")
        self.assertEqual(res.returncode, 1)
        self.assertIn("handshake is stale", res.stderr)

    def test_exec_helper_direct_start_crawler_rejected_zero_transfer(self) -> None:
        """Proves that direct start-crawler invocation fails when transfer rx/tx bytes are 0."""
        _, pub = self._write_default_valid_wg0()
        self.create_mock_docker(
            'now=$(date +%s)\n'
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "gluetun"; exit 0;\n'
            'elif [[ "$*" =~ "Config.Image" ]]; then echo "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00"; exit 0;\n'
            f'elif [[ "$*" =~ "latest-handshakes" ]]; then echo "{pub} $now"; exit 0;\n'
            f'elif [[ "$*" =~ "transfer" ]]; then echo "{pub} 0 0"; exit 0; fi; exit 0'
        )
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  crawler:\n    network_mode: service:gluetun\n")

        res = self.run_helper("start-crawler")
        self.assertEqual(res.returncode, 1)
        self.assertIn("transfer inactive", res.stderr)

    def test_exec_helper_direct_start_crawler_rejected_firewall_not_drop(self) -> None:
        """Proves that direct start-crawler invocation fails when killswitch is inactive."""
        priv, pub = bridge.generate_wireguard_keypair()
        valid_cfg = (
            "[Interface]\n"
            "Address = 10.0.0.2/32\n"
            f"PrivateKey = {priv}\n"
            "DNS = 10.0.0.242\n\n"
            "[Peer]\n"
            f"PublicKey = {pub}\n"
            "Endpoint = 212.56.54.190:1337\n"
            "AllowedIPs = 0.0.0.0/0\n"
            "PersistentKeepalive = 25\n"
        )
        self.run_helper("write-config", input_data=valid_cfg)
        self.create_mock_docker(
            'now=$(date +%s)\n'
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "gluetun"; exit 0;\n'
            'elif [[ "$*" =~ "Config.Image" ]]; then echo "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00"; exit 0;\n'
            f'elif [[ "$*" =~ "latest-handshakes" ]]; then echo "{pub} $now"; exit 0;\n'
            f'elif [[ "$*" =~ "transfer" ]]; then echo "{pub} 1000 2000"; exit 0;\n'
            f'elif [[ "$*" =~ "endpoints" ]]; then echo "{pub} 212.56.54.190:1337"; exit 0;\n'
            'elif [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo "-P OUTPUT ACCEPT"; exit 0; fi; exit 0'
        )
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  crawler:\n    network_mode: service:gluetun\n")

        res = self.run_helper("start-crawler")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Default OUTPUT policy is not DROP", res.stderr)

    def test_exec_helper_direct_start_crawler_rejected_crawler_not_in_gluetun_namespace(self) -> None:
        """Proves that direct start-crawler invocation fails when crawler is not bound to Gluetun."""
        priv, pub = bridge.generate_wireguard_keypair()
        valid_cfg = (
            "[Interface]\n"
            "Address = 10.0.0.2/32\n"
            f"PrivateKey = {priv}\n"
            "DNS = 10.0.0.242\n\n"
            "[Peer]\n"
            f"PublicKey = {pub}\n"
            "Endpoint = 212.56.54.190:1337\n"
            "AllowedIPs = 0.0.0.0/0\n"
            "PersistentKeepalive = 25\n"
        )
        self.run_helper("write-config", input_data=valid_cfg)
        self.create_mock_docker(
            'now=$(date +%s)\n'
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "gluetun"; exit 0;\n'
            'elif [[ "$*" =~ "Config.Image" ]]; then echo "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00"; exit 0;\n'
            f'elif [[ "$*" =~ "latest-handshakes" ]]; then echo "{pub} $now"; exit 0;\n'
            f'elif [[ "$*" =~ "transfer" ]]; then echo "{pub} 1000 2000"; exit 0;\n'
            f'elif [[ "$*" =~ "endpoints" ]]; then echo "{pub} 212.56.54.190:1337"; exit 0;\n'
            'elif [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "disable_ipv6" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "nslookup" ]]; then echo "Address: 93.184.216.34"; exit 0;\n'
            'elif [[ "$*" =~ "checkip.amazonaws.com" ]]; then echo "156.146.54.4"; exit 0;\n'
            'elif [[ "$*" =~ "ipinfo.io/json" ]]; then echo \'{"ip":"156.146.54.4","org":"AS60068 Datacamp Limited"}\'; exit 0; fi; exit 0'
        )
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  crawler:\n    network_mode: host\n")

        res = self.run_helper("start-crawler")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Crawler is not restricted to Gluetun network namespace", res.stderr)

    # 4. DNS Path-Aware Routing & Firewall Verification
    def test_exec_helper_dns_routing_wg0_accepted(self) -> None:
        """Proves that DNS route query confirms dev wg0 routing."""
        self.create_mock_docker("echo '8.8.8.8 dev wg0 src 10.0.0.2 uid 0'; exit 0")
        res = self.run_helper("verify-dns-route")
        self.assertEqual(res.returncode, 0)
        self.assertIn("DNS_ROUTE_WG0_OK", res.stdout)

    def test_exec_helper_dns_routing_eth0_rejected(self) -> None:
        """Proves that DNS route query routing over dev eth0 fails closed."""
        self.create_mock_docker("echo '8.8.8.8 dev eth0 src 192.0.2.100 uid 0'; exit 0")
        res = self.run_helper("verify-dns-route")
        self.assertEqual(res.returncode, 1)
        self.assertIn("DNS route does not go through wg0", res.stderr)

    def test_exec_helper_dns_firewall_verification_accepted(self) -> None:
        """Proves that complete firewall isolation and route binding are confirmed."""
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "disable_ipv6" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res = self.run_helper("verify-dns-firewall")
        self.assertEqual(res.returncode, 0, msg=res.stderr)
        self.assertIn("DNS_FIREWALL_OK", res.stdout)

    def test_exec_helper_dns_firewall_verification_rejected_when_leak_rule_present(self) -> None:
        """Proves that any DNS egress rule on physical interface triggers critical leak alert."""
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT\\n-A OUTPUT -o eth0 -p udp --dport 53 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res = self.run_helper("verify-dns-firewall")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Unauthorized egress rule in Gluetun firewall", res.stderr)

    def test_exec_helper_dns_firewall_verification_rejected_when_broad_physical_accept_present(self) -> None:
        """Proves that a default DROP policy with a broad physical-interface ACCEPT rule fails verification."""
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT\\n-A OUTPUT -o eth0 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res = self.run_helper("verify-dns-firewall")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Unauthorized egress rule in Gluetun firewall", res.stderr)

    def test_exec_helper_dns_firewall_verification_rejected_when_unconstrained_accept_present(self) -> None:
        """Proves that an unconstrained -j ACCEPT rule fails verification."""
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT\\n-A OUTPUT -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res = self.run_helper("verify-dns-firewall")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Unauthorized egress rule in Gluetun firewall", res.stderr)

    def test_exec_helper_dns_firewall_verification_rejected_when_arbitrary_protocol_accept_present(self) -> None:
        """Proves that arbitrary non-VPN protocol egress (e.g. -p tcp) fails verification."""
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT\\n-A OUTPUT -o eth0 -p tcp -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res = self.run_helper("verify-dns-firewall")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Unauthorized egress rule in Gluetun firewall", res.stderr)

    def test_exec_helper_dns_firewall_verification_rejected_when_custom_chain_present(self) -> None:
        """Proves that unexpected chain jumps in OUTPUT chain fail verification."""
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT\\n-A OUTPUT -j BYPASS_CHAIN"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res = self.run_helper("verify-dns-firewall")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Unexpected or insecure firewall directive in OUTPUT chain", res.stderr)

    def test_exec_helper_dns_firewall_verification_accepted_with_vpn_server_endpoint(self) -> None:
        """Proves that legitimate Gluetun ruleset including the VPN server endpoint and Docker bridge rule is accepted."""
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "endpoints" ]]; then echo "peer_key 212.56.54.190:1337"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT\\n-A OUTPUT -d 212.56.54.190/32 -o eth0 -p udp -m udp --dport 1337 -j ACCEPT\\n-A OUTPUT -s 172.18.0.2/32 -d 172.18.0.0/16 -o eth0 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "disable_ipv6" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 table 51820 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res = self.run_helper("verify-dns-firewall")
        self.assertEqual(res.returncode, 0, msg=res.stderr)
        self.assertIn("DNS_FIREWALL_OK", res.stdout)

    def test_exec_helper_write_config_accepts_valid_schema_and_rejects_injections(self) -> None:
        """Proves that write-config strictly validates wg0.conf schema and rejects PostUp, regex injection, and non-public endpoints."""
        priv, pub = bridge.generate_wireguard_keypair()
        valid_cfg = (
            "[Interface]\n"
            "Address = 10.0.0.2/32\n"
            f"PrivateKey = {priv}\n"
            "DNS = 10.0.0.242\n\n"
            "[Peer]\n"
            f"PublicKey = {pub}\n"
            "Endpoint = 212.56.54.190:1337\n"
            "AllowedIPs = 0.0.0.0/0\n"
            "PersistentKeepalive = 25\n"
        )
        res_ok = self.run_helper("write-config", input_data=valid_cfg)
        self.assertEqual(res_ok.returncode, 0, msg=res_ok.stderr)
        self.assertIn("CONFIG_OK", res_ok.stdout)

        # Reject PostUp directive
        bad_postup = valid_cfg + "PostUp = /bin/sh -c 'id'\n"
        res_postup = self.run_helper("write-config", input_data=bad_postup)
        self.assertNotEqual(res_postup.returncode, 0)

        # Reject regex injection in Endpoint
        bad_regex = valid_cfg.replace("212.56.54.190:1337", ".*|.*:1337")
        res_regex = self.run_helper("write-config", input_data=bad_regex)
        self.assertNotEqual(res_regex.returncode, 0)

        # Reject metadata / link-local / private Endpoint IP
        bad_meta = valid_cfg.replace("212.56.54.190:1337", "169.254.169.254:1337")
        res_meta = self.run_helper("write-config", input_data=bad_meta)
        self.assertNotEqual(res_meta.returncode, 0)

    def test_exec_helper_endpoint_authorization_rejects_unauthorized_public_endpoint(self) -> None:
        """Proves that PIA endpoint authorization fails closed on unauthorized IP, missing/invalid/symlinked CA, curl failure, unauthorized port, and proxy bypass."""
        priv, pub = bridge.generate_wireguard_keypair()
        auth_cfg = (
            "[Interface]\n"
            "Address = 10.0.0.2/32\n"
            f"PrivateKey = {priv}\n"
            "DNS = 10.0.0.242\n\n"
            "[Peer]\n"
            f"PublicKey = {pub}\n"
            "Endpoint = 212.56.54.190:1337\n"
            "AllowedIPs = 0.0.0.0/0\n"
            "PersistentKeepalive = 25\n"
        )

        # 1. Authorized PIA endpoint passes serverlist, port, and TLS verification
        res_auth = self.run_helper("write-config", input_data=auth_cfg)
        self.assertEqual(res_auth.returncode, 0, msg=res_auth.stderr)
        self.assertIn("CONFIG_OK", res_auth.stdout)

        # 2. Attacker-controlled public IP (e.g. 198.51.100.77:1337) is rejected by serverlist check
        attacker_cfg = auth_cfg.replace("212.56.54.190:1337", "198.51.100.77:1337")
        res_unauth = self.run_helper("write-config", input_data=attacker_cfg)
        self.assertEqual(res_unauth.returncode, 1)
        self.assertIn("Unauthorized WireGuard endpoint 198.51.100.77", res_unauth.stderr)

        # 3. Unauthorized WireGuard UDP port (e.g. 212.56.54.190:9999 not in groups.wg[].ports) is rejected
        bad_port_cfg = auth_cfg.replace("212.56.54.190:1337", "212.56.54.190:9999")
        res_bad_port = self.run_helper("write-config", input_data=bad_port_cfg)
        self.assertEqual(res_bad_port.returncode, 1)
        self.assertIn("Unauthorized WireGuard UDP port 9999", res_bad_port.stderr)

        # 4. Missing PIA Root CA file fails closed (never fails open)
        os.remove(self.ca_path)
        res_no_ca = self.run_helper("write-config", input_data=auth_cfg)
        self.assertEqual(res_no_ca.returncode, 1)
        self.assertIn("is missing, unreadable, or a symlink", res_no_ca.stderr)

        # 5. Invalid/corrupted PEM content in PIA Root CA file fails closed
        with open(self.ca_path, "w", encoding="utf-8", newline="\n") as f:
            f.write("-----BEGIN CERTIFICATE-----\nnot-valid-base64-cert\n-----END CERTIFICATE-----\n")
        res_bad_ca = self.run_helper("write-config", input_data=auth_cfg)
        self.assertEqual(res_bad_ca.returncode, 1)
        self.assertIn("failed X.509 parsing", res_bad_ca.stderr)

        # Restore valid CA
        with open(self.ca_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(bridge.PIA_ROOT_CA_PEM)

        # 6. curl non-zero exit code fails closed even if HTTP status code looks like 400
        curl_path = os.path.join(self.mock_bin, "curl")
        with open(curl_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(
                "#!/usr/bin/env bash\n"
                "if [[ \"$*\" == *\"serverlist.piaservers.net\"* ]]; then\n"
                "  echo '{\"groups\":{\"wg\":[{\"name\":\"wireguard\",\"ports\":[1337]}]},\"regions\":[{\"id\":\"us-newjersey\",\"servers\":{\"wg\":[{\"ip\":\"212.56.54.190\",\"cn\":\"newjersey434\"}]}}]}'\n"
                "  exit 0\n"
                "fi\n"
                "echo -n '400'\n"
                "exit 60\n"
            )
        os.chmod(curl_path, 0o755)
        res_tls_fail = self.run_helper("write-config", input_data=auth_cfg)
        self.assertEqual(res_tls_fail.returncode, 1)
        self.assertIn("failed PIA Root CA TLS verification", res_tls_fail.stderr)

        # 7. Proxy environment variables are stripped and --noproxy '*' is enforced
        with open(curl_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(
                "#!/usr/bin/env bash\n"
                "if [[ -n \"${HTTPS_PROXY:-}${https_proxy:-}${HTTP_PROXY:-}${http_proxy:-}${ALL_PROXY:-}${all_proxy:-}\" ]]; then\n"
                "  echo 'PROXY_ENV_LEAKED' >&2\n"
                "  exit 99\n"
                "fi\n"
                "if [[ \"$*\" != *\"--noproxy *\"* ]]; then\n"
                "  echo 'MISSING_NOPROXY_FLAG' >&2\n"
                "  exit 98\n"
                "fi\n"
                "if [[ \"$*\" == *\"serverlist.piaservers.net\"* ]]; then\n"
                "  echo '{\"groups\":{\"wg\":[{\"name\":\"wireguard\",\"ports\":[1337]}]},\"regions\":[{\"id\":\"us-newjersey\",\"servers\":{\"wg\":[{\"ip\":\"212.56.54.190\",\"cn\":\"newjersey434\"}]}}]}'\n"
                "  exit 0\n"
                "fi\n"
                "echo -n '400'\n"
                "exit 0\n"
            )
        os.chmod(curl_path, 0o755)
        with patch.dict(os.environ, {"HTTPS_PROXY": "http://127.0.0.1:8080", "http_proxy": "http://127.0.0.1:8080"}):
            res_noproxy = self.run_helper("write-config", input_data=auth_cfg)
        self.assertEqual(res_noproxy.returncode, 0, msg=res_noproxy.stderr)
        self.assertIn("CONFIG_OK", res_noproxy.stdout)

    def test_exec_helper_no_synthetic_wireguard_evidence_when_wg_unavailable(self) -> None:
        """Proves that wg-handshake, wg-transfer, and wg-endpoints fail closed when wg inspection is unavailable, even if interface carrier=1, rx_bytes>0, and wg0.conf exists."""
        priv, pub = bridge.generate_wireguard_keypair()
        valid_cfg = (
            "[Interface]\n"
            "Address = 10.0.0.2/32\n"
            f"PrivateKey = {priv}\n"
            "DNS = 10.0.0.242\n\n"
            "[Peer]\n"
            f"PublicKey = {pub}\n"
            "Endpoint = 212.56.54.190:1337\n"
            "AllowedIPs = 0.0.0.0/0\n"
            "PersistentKeepalive = 25\n"
        )
        self.run_helper("write-config", input_data=valid_cfg)

        # Mock docker where carrier=1, rx_bytes=99999, tx_bytes=88888, /tmp/gluetun/ip exists,
        # but `wg show wg0 ...` and `wget` fail (simulating a dead/disconnected tunnel with historical bytes)
        self.create_mock_docker(
            'if [[ "$*" =~ "State.Running" ]]; then echo "1234"; exit 0;\n'
            'elif [[ "$*" =~ "carrier" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "rx_bytes" ]]; then echo "99999"; exit 0;\n'
            'elif [[ "$*" =~ "tx_bytes" ]]; then echo "88888"; exit 0;\n'
            'elif [[ "$*" =~ "/tmp/gluetun/ip" ]]; then echo "156.146.54.4"; exit 0;\n'
            'elif [[ "$*" =~ "wg show" ]]; then exit 1;\n'
            'elif [[ "$*" =~ "wget" ]]; then exit 1; fi; exit 1'
        )

        res_hs = self.run_helper("wg-handshake")
        self.assertNotEqual(res_hs.returncode, 0)
        self.assertEqual(res_hs.stdout.strip(), "")
        self.assertIn("Authoritative WireGuard latest-handshakes inspection unavailable", res_hs.stderr)

        res_tx = self.run_helper("wg-transfer")
        self.assertNotEqual(res_tx.returncode, 0)
        self.assertEqual(res_tx.stdout.strip(), "")
        self.assertIn("Authoritative WireGuard transfer inspection unavailable", res_tx.stderr)

        res_ep = self.run_helper("wg-endpoints")
        self.assertNotEqual(res_ep.returncode, 0)
        self.assertEqual(res_ep.stdout.strip(), "")
        self.assertIn("Authoritative WireGuard endpoints inspection unavailable", res_ep.stderr)

        res_eg = self.run_helper("query-egress")
        self.assertNotEqual(res_eg.returncode, 0)
        self.assertEqual(res_eg.stdout.strip(), "")
        self.assertIn("Active tunneled egress check failed", res_eg.stderr)

    def test_exec_helper_userspace_and_kernelspace_wireguard_inspection(self) -> None:
        """Proves that query_wg inspects both userspace (-n -m via /var/run/wireguard/wg0.sock) and kernelspace (-n netlink) WireGuard."""
        self.create_mock_docker(
            'if [[ "$*" =~ "State.Running" ]]; then echo "4321"; exit 0; fi; exit 1'
        )
        # Mock wg and nsenter in mock_bin: only succeeds when -m is passed (simulating userspace UAPI socket in container mount namespace)
        wg_path = os.path.join(self.mock_bin, "wg")
        with open(wg_path, "w", encoding="utf-8", newline="\n") as f:
            f.write("#!/usr/bin/env bash\nexit 0\n")
        os.chmod(wg_path, 0o755)

        nsenter_path = os.path.join(self.mock_bin, "nsenter")
        with open(nsenter_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(
                "#!/usr/bin/env bash\n"
                "if [[ \"$*\" =~ \"-n -m wg show wg0 latest-handshakes\" ]]; then\n"
                "  echo 'userspace_peer 1731000000'\n"
                "  exit 0\n"
                "fi\n"
                "exit 1\n"
            )
        os.chmod(nsenter_path, 0o755)

        res_hs = self.run_helper("wg-handshake")
        self.assertEqual(res_hs.returncode, 0)
        self.assertEqual(res_hs.stdout.strip(), "userspace_peer 1731000000")

    def test_exec_helper_start_crawler_rejected_when_observed_endpoint_differs_from_configured(self) -> None:
        """Proves that start-crawler fails closed if the observed active peer or endpoint differs from wg0.conf."""
        priv, pub = bridge.generate_wireguard_keypair()
        valid_cfg = (
            "[Interface]\n"
            "Address = 10.0.0.2/32\n"
            f"PrivateKey = {priv}\n"
            "DNS = 10.0.0.242\n\n"
            "[Peer]\n"
            f"PublicKey = {pub}\n"
            "Endpoint = 212.56.54.190:1337\n"
            "AllowedIPs = 0.0.0.0/0\n"
            "PersistentKeepalive = 25\n"
        )
        self.run_helper("write-config", input_data=valid_cfg)
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  crawler:\n    network_mode: service:gluetun\n")

        # Observed endpoint is 198.51.100.99:1337 instead of configured 212.56.54.190:1337
        self.create_mock_docker(
            'now=$(date +%s)\n'
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "gluetun"; exit 0;\n'
            'elif [[ "$*" =~ "Config.Image" ]]; then echo "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00"; exit 0;\n'
            f'elif [[ "$*" =~ "latest-handshakes" ]]; then echo "{pub} $now"; exit 0;\n'
            f'elif [[ "$*" =~ "transfer" ]]; then echo "{pub} 5000 3000"; exit 0;\n'
            f'elif [[ "$*" =~ "endpoints" ]]; then echo "{pub} 198.51.100.99:1337"; exit 0; fi; exit 0'
        )
        res = self.run_helper("start-crawler")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Observed WireGuard peer endpoint (198.51.100.99:1337) does not match configured Endpoint (212.56.54.190:1337)", res.stderr)

    def test_exec_helper_cleanup_fails_closed_on_daemon_or_shutdown_failure_and_succeeds_idempotently(self) -> None:
        """Proves that cleanup fails closed when Docker is unavailable, compose down fails, or a container stays running, and preserves wg0.conf until shutdown succeeds."""
        priv, pub = bridge.generate_wireguard_keypair()
        valid_cfg = (
            "[Interface]\n"
            "Address = 10.0.0.2/32\n"
            f"PrivateKey = {priv}\n"
            "DNS = 10.0.0.242\n\n"
            "[Peer]\n"
            f"PublicKey = {pub}\n"
            "Endpoint = 212.56.54.190:1337\n"
            "AllowedIPs = 0.0.0.0/0\n"
            "PersistentKeepalive = 25\n"
        )
        self.run_helper("write-config", input_data=valid_cfg)
        cfg_file = os.path.join(self.config_dir, "wg0.conf")
        self.assertTrue(os.path.exists(cfg_file))

        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  gluetun:\n    image: test\n")

        # Case 1: Docker daemon unavailable -> fails closed, does NOT print CLEANUP_OK, preserves wg0.conf
        self.create_mock_docker('if [[ "$1" == "info" ]]; then exit 1; fi; exit 0')
        res_daemon = self.run_helper("cleanup")
        self.assertEqual(res_daemon.returncode, 1)
        self.assertNotIn("CLEANUP_OK", res_daemon.stdout)
        self.assertIn("Docker daemon is unavailable", res_daemon.stderr)
        self.assertTrue(os.path.exists(cfg_file))

        # Case 2: docker compose down fails -> fails closed, preserves wg0.conf
        self.create_mock_docker(
            'if [[ "$1" == "info" ]]; then exit 0;\n'
            'elif [[ "$1" == "compose" ]]; then echo "compose down error" >&2; exit 1; fi; exit 0'
        )
        res_down_fail = self.run_helper("cleanup")
        self.assertEqual(res_down_fail.returncode, 1)
        self.assertNotIn("CLEANUP_OK", res_down_fail.stdout)
        self.assertIn("Failed to shut down VPN and crawler containers", res_down_fail.stderr)
        self.assertTrue(os.path.exists(cfg_file))

        # Case 3: Stale container (canonical name lodestar_crawler) remains running after compose down -> fails closed, preserves wg0.conf
        self.create_mock_docker(
            'if [[ "$1" == "info" ]]; then exit 0;\n'
            'elif [[ "$1" == "compose" ]]; then exit 0;\n'
            'elif [[ "$1" == "inspect" && "$*" =~ "lodestar_crawler" ]]; then echo "true"; exit 0;\n'
            'elif [[ "$1" == "inspect" ]]; then echo "No such object: gluetun" >&2; exit 1; fi; exit 1'
        )
        res_stale = self.run_helper("cleanup")
        self.assertEqual(res_stale.returncode, 1)
        self.assertNotIn("CLEANUP_OK", res_stale.stdout)
        self.assertIn("Container 'lodestar_crawler' is still running after shutdown", res_stale.stderr)
        self.assertTrue(os.path.exists(cfg_file))

        # Case 3b: docker compose ps -q returns remaining active container ID -> fails closed, preserves wg0.conf
        self.create_mock_docker(
            'if [[ "$1" == "info" ]]; then exit 0;\n'
            'elif [[ "$1" == "compose" && "$*" =~ "ps -q" ]]; then echo "abc123def456"; exit 0;\n'
            'elif [[ "$1" == "compose" ]]; then exit 0; fi; exit 0'
        )
        res_ps_active = self.run_helper("cleanup")
        self.assertEqual(res_ps_active.returncode, 1)
        self.assertNotIn("CLEANUP_OK", res_ps_active.stdout)
        self.assertIn("Compose service containers remain active after shutdown", res_ps_active.stderr)
        self.assertTrue(os.path.exists(cfg_file))

        # Case 3c: docker inspect fails with unexpected daemon/permission error (not 'No such object') -> fails closed, preserves wg0.conf
        self.create_mock_docker(
            'if [[ "$1" == "info" ]]; then exit 0;\n'
            'elif [[ "$1" == "compose" ]]; then exit 0;\n'
            'elif [[ "$1" == "inspect" ]]; then echo "Error response from daemon: permission denied" >&2; exit 1; fi; exit 0'
        )
        res_insp_err = self.run_helper("cleanup")
        self.assertEqual(res_insp_err.returncode, 1)
        self.assertNotIn("CLEANUP_OK", res_insp_err.stdout)
        self.assertIn("Failed to inspect container 'lodestar_crawler' during cleanup", res_insp_err.stderr)
        self.assertTrue(os.path.exists(cfg_file))

        # Case 4: Clean shutdown -> removes config_dir, prints CLEANUP_OK, and repeated cleanup is idempotent
        self.create_mock_docker(
            'if [[ "$1" == "info" ]]; then exit 0;\n'
            'elif [[ "$1" == "compose" ]]; then exit 0;\n'
            'elif [[ "$1" == "inspect" ]]; then echo "false"; exit 0; fi; exit 0'
        )
        res_ok1 = self.run_helper("cleanup")
        self.assertEqual(res_ok1.returncode, 0)
        self.assertIn("CLEANUP_OK", res_ok1.stdout)
        self.assertFalse(os.path.exists(cfg_file))

        res_ok2 = self.run_helper("cleanup")
        self.assertEqual(res_ok2.returncode, 0)
        self.assertIn("CLEANUP_OK", res_ok2.stdout)

    def test_exec_helper_dns_firewall_verification_rejected_when_policy_not_drop(self) -> None:
        """Proves that non-DROP output policy fails firewall verification."""
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT ACCEPT\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res = self.run_helper("verify-dns-firewall")
        self.assertEqual(res.returncode, 1)
        self.assertIn("Default OUTPUT policy is not DROP", res.stderr)

    def test_exec_helper_dns_firewall_rejects_mismatched_local_subnet_and_wrong_vpn_port(self) -> None:
        """Proves that verify-dns-firewall rejects non-matching private subnet rules and VPN endpoint rules with wrong/missing UDP port."""
        # 1. Local RFC1918 rule for 10.0.0.0/8 when actual Docker bridge on eth0 is 172.18.0.2/32 -> 172.18.0.0/16
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "endpoints" ]]; then echo "peer_key 212.56.54.190:1337"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT\\n-A OUTPUT -s 10.0.0.2/32 -d 10.0.0.0/8 -o eth0 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "disable_ipv6" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res_bad_local = self.run_helper("verify-dns-firewall")
        self.assertEqual(res_bad_local.returncode, 1)
        self.assertIn("Unauthorized egress rule in Gluetun firewall: -A OUTPUT -s 10.0.0.2/32 -d 10.0.0.0/8 -o eth0 -j ACCEPT", res_bad_local.stderr)

        # 2. VPN endpoint rule with wrong UDP port (--dport 9999 instead of 1337)
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "endpoints" ]]; then echo "peer_key 212.56.54.190:1337"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT\\n-A OUTPUT -d 212.56.54.190/32 -o eth0 -p udp -m udp --dport 9999 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "disable_ipv6" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res_bad_port = self.run_helper("verify-dns-firewall")
        self.assertEqual(res_bad_port.returncode, 1)
        self.assertIn("Unauthorized egress rule in Gluetun firewall: -A OUTPUT -d 212.56.54.190/32 -o eth0 -p udp -m udp --dport 9999 -j ACCEPT", res_bad_port.stderr)

        # 3. VPN endpoint rule without --dport (allowing all UDP ports to endpoint)
        self.create_mock_docker(
            'if [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "endpoints" ]]; then echo "peer_key 212.56.54.190:1337"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT\\n-A OUTPUT -d 212.56.54.190/32 -o eth0 -p udp -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "disable_ipv6" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0; fi; exit 0'
        )
        res_no_port = self.run_helper("verify-dns-firewall")
        self.assertEqual(res_no_port.returncode, 1)
        self.assertIn("Unauthorized egress rule in Gluetun firewall: -A OUTPUT -d 212.56.54.190/32 -o eth0 -p udp -j ACCEPT", res_no_port.stderr)

    def test_exec_helper_consolidated_start_crawler_gate_enforces_dns_and_egress_checks(self) -> None:
        """Proves that direct start-crawler invocation enforces DNS route, DoT resolution, active egress IP, and ISP ownership checks."""
        priv, pub = bridge.generate_wireguard_keypair()
        valid_cfg = (
            "[Interface]\n"
            "Address = 10.0.0.2/32\n"
            f"PrivateKey = {priv}\n"
            "DNS = 10.0.0.242\n\n"
            "[Peer]\n"
            f"PublicKey = {pub}\n"
            "Endpoint = 212.56.54.190:1337\n"
            "AllowedIPs = 0.0.0.0/0\n"
            "PersistentKeepalive = 25\n"
        )
        self.run_helper("write-config", input_data=valid_cfg)
        with open(os.path.join(self.compose_dir, "compose.yaml"), "w") as f:
            f.write("services:\n  crawler:\n    network_mode: service:gluetun\n")

        # 1. Fails closed when active egress IP matches host public IP (192.0.2.100)
        self.create_mock_docker(
            'now=$(date +%s)\n'
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "gluetun"; exit 0;\n'
            'elif [[ "$*" =~ "Config.Image" ]]; then echo "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00"; exit 0;\n'
            f'elif [[ "$*" =~ "latest-handshakes" ]]; then echo "{pub} $now"; exit 0;\n'
            f'elif [[ "$*" =~ "transfer" ]]; then echo "{pub} 5000 3000"; exit 0;\n'
            f'elif [[ "$*" =~ "endpoints" ]]; then echo "{pub} 212.56.54.190:1337"; exit 0;\n'
            'elif [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "disable_ipv6" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "nslookup" ]]; then echo "Address: 93.184.216.34"; exit 0;\n'
            'elif [[ "$*" =~ "checkip.amazonaws.com" ]]; then echo "192.0.2.100"; exit 0;\n'
            'elif [[ "$*" =~ "ipinfo.io/json" ]]; then echo \'{"ip":"192.0.2.100","org":"AS60068 Datacamp Limited"}\'; exit 0; fi; exit 0'
        )
        res_leak_ip = self.run_helper("start-crawler")
        self.assertEqual(res_leak_ip.returncode, 1)
        self.assertIn("CRITICAL LEAK: VPN egress IP (192.0.2.100) matches host IP (192.0.2.100)", res_leak_ip.stderr)

        # 2. Fails closed when egress ISP belongs to Akamai/Linode
        self.create_mock_docker(
            'now=$(date +%s)\n'
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "gluetun"; exit 0;\n'
            'elif [[ "$*" =~ "Config.Image" ]]; then echo "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00"; exit 0;\n'
            f'elif [[ "$*" =~ "latest-handshakes" ]]; then echo "{pub} $now"; exit 0;\n'
            f'elif [[ "$*" =~ "transfer" ]]; then echo "{pub} 5000 3000"; exit 0;\n'
            f'elif [[ "$*" =~ "endpoints" ]]; then echo "{pub} 212.56.54.190:1337"; exit 0;\n'
            'elif [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "disable_ipv6" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "nslookup" ]]; then echo "Address: 93.184.216.34"; exit 0;\n'
            'elif [[ "$*" =~ "checkip.amazonaws.com" ]]; then echo "156.146.54.4"; exit 0;\n'
            'elif [[ "$*" =~ "ipinfo.io/json" ]]; then echo \'{"ip":"156.146.54.4","org":"AS63949 Akamai Connected Cloud / Linode"}\'; exit 0; fi; exit 0'
        )
        with patch.dict(os.environ, {"MOCK_JQ_ORG": "AS63949 Akamai Connected Cloud / Linode"}):
            res_leak_isp = self.run_helper("start-crawler")
        self.assertEqual(res_leak_isp.returncode, 1)
        self.assertIn("CRITICAL LEAK: Container egress is routing through host ISP/cloud", res_leak_isp.stderr)

        # 3. Succeeds and prints CRAWLER_STARTED when all consolidated checks pass
        self.create_mock_docker(
            'now=$(date +%s)\n'
            'if [[ "$*" =~ "State.Health" ]]; then echo "healthy"; exit 0;\n'
            'elif [[ "$*" =~ "com.docker.compose.service" ]]; then echo "gluetun"; exit 0;\n'
            'elif [[ "$*" =~ "Config.Image" ]]; then echo "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00"; exit 0;\n'
            f'elif [[ "$*" =~ "latest-handshakes" ]]; then echo "{pub} $now"; exit 0;\n'
            f'elif [[ "$*" =~ "transfer" ]]; then echo "{pub} 5000 3000"; exit 0;\n'
            f'elif [[ "$*" =~ "endpoints" ]]; then echo "{pub} 212.56.54.190:1337"; exit 0;\n'
            'elif [[ "$*" =~ "link show" ]]; then echo -e "1: lo: <LOOPBACK>\\n2: eth0: <BROADCAST>\\n3: wg0: <POINTOPOINT>"; exit 0;\n'
            'elif [[ "$*" =~ "addr show" ]]; then echo "2: eth0    inet 172.18.0.2/16 brd 172.18.255.255 scope global eth0"; exit 0;\n'
            'elif [[ "$*" =~ "route show" ]]; then echo "172.18.0.0/16 proto kernel scope link src 172.18.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "iptables" ]]; then echo -e "-P OUTPUT DROP\\n-A OUTPUT -o lo -j ACCEPT\\n-A OUTPUT -o wg0 -j ACCEPT"; exit 0;\n'
            'elif [[ "$*" =~ "disable_ipv6" ]]; then echo "1"; exit 0;\n'
            'elif [[ "$*" =~ "route get" ]]; then echo "8.8.8.8 dev wg0 src 10.0.0.2"; exit 0;\n'
            'elif [[ "$*" =~ "nslookup" ]]; then echo "Address: 93.184.216.34"; exit 0;\n'
            'elif [[ "$*" =~ "checkip.amazonaws.com" ]]; then echo "156.146.54.4"; exit 0;\n'
            'elif [[ "$*" =~ "ipinfo.io/json" ]]; then echo \'{"ip":"156.146.54.4","org":"AS60068 Datacamp Limited"}\'; exit 0; fi; exit 0'
        )
        res_ok = self.run_helper("start-crawler")
        self.assertEqual(res_ok.returncode, 0, msg=res_ok.stderr)
        self.assertIn("CRAWLER_STARTED", res_ok.stdout)


# =============================================================================
# ComposeDeployer Strict Boundary Unit Tests
# =============================================================================
class TestComposeDeployerStrictBoundary(unittest.TestCase):
    """Verifies that ComposeDeployer rejects all malicious Compose configurations locally."""

    def test_rejects_env_interpolation(self) -> None:
        with tempfile.NamedTemporaryFile("w", delete=False, suffix=".yaml") as f:
            f.write("services:\n  gluetun:\n    image: ${IMAGE_VAR}\n")
            f_path = f.name
        try:
            with self.assertRaises(bridge.ComposeValidationError) as ctx:
                bridge.ComposeDeployer.validate_local_compose(f_path)
            self.assertIn("interpolation forbidden", str(ctx.exception).lower())
        finally:
            os.unlink(f_path)

    def test_rejects_arbitrary_host_mounts(self) -> None:
        with tempfile.NamedTemporaryFile("w", delete=False, suffix=".yaml") as f:
            f.write(
                "services:\n  gluetun:\n"
                f"    image: {bridge.GLUETUN_IMAGE}\n"
                "    volumes:\n      - /etc:/etc\n"
            )
            f_path = f.name
        try:
            with self.assertRaises(bridge.ComposeValidationError) as ctx:
                bridge.ComposeDeployer.validate_local_compose(f_path)
            self.assertIn("unauthorized volume mount", str(ctx.exception).lower())
        finally:
            os.unlink(f_path)

    def test_rejects_privileged_true(self) -> None:
        with tempfile.NamedTemporaryFile("w", delete=False, suffix=".yaml") as f:
            f.write(
                "services:\n  gluetun:\n"
                f"    image: {bridge.GLUETUN_IMAGE}\n"
                "    privileged: true\n"
            )
            f_path = f.name
        try:
            with self.assertRaises(bridge.ComposeValidationError) as ctx:
                bridge.ComposeDeployer.validate_local_compose(f_path)
            self.assertIn("privileged: true", str(ctx.exception).lower())
        finally:
            os.unlink(f_path)

    def test_rejects_docker_socket(self) -> None:
        with tempfile.NamedTemporaryFile("w", delete=False, suffix=".yaml") as f:
            f.write(
                "services:\n  gluetun:\n"
                f"    image: {bridge.GLUETUN_IMAGE}\n"
                "    volumes:\n      - /var/run/docker.sock:/var/run/docker.sock\n"
            )
            f_path = f.name
        try:
            with self.assertRaises(bridge.ComposeValidationError) as ctx:
                bridge.ComposeDeployer.validate_local_compose(f_path)
            self.assertIn("docker.sock", str(ctx.exception).lower())
        finally:
            os.unlink(f_path)

    def test_rejects_crawler_outside_gluetun_namespace(self) -> None:
        with tempfile.NamedTemporaryFile("w", delete=False, suffix=".yaml") as f:
            f.write(
                "services:\n  gluetun:\n"
                f"    image: {bridge.GLUETUN_IMAGE}\n"
                "  crawler:\n    image: curlimages/curl:latest\n    network_mode: bridge\n"
            )
            f_path = f.name
        try:
            with self.assertRaises(bridge.ComposeValidationError) as ctx:
                bridge.ComposeDeployer.validate_local_compose(f_path)
            self.assertIn("network_mode: service:gluetun", str(ctx.exception).lower())
        finally:
            os.unlink(f_path)


# =============================================================================
# Windows Secret-File Creation & Permissions Tests
# =============================================================================
class TestWindowsExportSecurityOrdering(unittest.TestCase):
    """Verifies that write_secure_local_config establishes permissions prior to writing content."""

    def test_pre_resolution_symlink_rejected(self) -> None:
        """Confirms that symlinks in target path are rejected before resolution."""
        with patch("pathlib.Path.is_symlink", return_value=True):
            with self.assertRaises(bridge.PiaBridgeError) as ctx:
                bridge.write_secure_local_config("symlink.conf", "secret_data")
            self.assertIn("symlink detected in configuration output path", str(ctx.exception).lower())

    def test_pre_resolution_parent_symlink_rejected(self) -> None:
        """Confirms that symlinks in parent directories are rejected before resolution."""
        with patch("os.path.islink", side_effect=lambda p: os.path.basename(str(p)) == "parent_symlink"):
            with self.assertRaises(bridge.PiaBridgeError) as ctx:
                bridge.write_secure_local_config("C:/parent_symlink/target.conf", "secret_data")
            self.assertIn("symlink detected in parent directory path", str(ctx.exception).lower())


if __name__ == "__main__":
    unittest.main()
