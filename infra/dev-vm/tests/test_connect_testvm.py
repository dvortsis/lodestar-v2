"""Pins Connect-TestVm.ps1's SSH hardening (#864 review S1-S3). There is no
PowerShell on the CI's Linux runner, so these read the script; the remote bash
part is extracted and parsed with bash -n."""
import re
import subprocess
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "Connect-TestVm.ps1"


def text() -> str:
    return SCRIPT.read_text()


def remote_script() -> str:
    m = re.search(r"\$remote = @'\n(.*?)\n'@", text(), re.S)
    assert m, "the remote script was not found"
    return m.group(1)


def ssh_call() -> str:
    m = re.search(r"^& ssh .*?\"\$User@\$Ip\" \$cmd$", text(), re.S | re.M)
    assert m, "the ssh call was not found"
    return m.group(0)


class KeyTests(unittest.TestCase):
    def test_default_key_is_the_test_vms_own(self):
        # S1: never the Akamai fleet's key, by default or by -KeyFile.
        self.assertIn("[string]$KeyFile = (Join-Path $HOME '.ssh\\lodestar_testvm_ed25519')", text())
        code = "\n".join(ln for ln in text().splitlines() if not ln.lstrip().startswith("#"))
        self.assertNotIn("lodestar_akamai_ed25519", code.split("'@", 1)[1])
        self.assertIn("-match '^lodestar_akamai'", text())
        self.assertLess(text().index("-match '^lodestar_akamai'"), text().index("& ssh "))


class NothingLeftOnTheVmTests(unittest.TestCase):
    def test_remote_script_runs_from_a_pipe(self):
        # S2: no script file in the VM user's home for later code to read or change.
        self.assertIn("$cmd = \"bash <(printf %s '$(ConvertTo-B64 $remote)' | base64 -d)\"", text())
        self.assertNotIn(".devvm-connect.sh", text())

    def test_claude_starts_without_a_script_file(self):
        r = remote_script()
        self.assertNotIn(".devvm-claude.sh", r)
        self.assertIsNone(re.search(r">\s*\"?\$HOME", r))
        self.assertIsNone(re.search(r">\s*~/", r))

    def test_remote_script_parses(self):
        subprocess.run(["bash", "-n"], input=remote_script(), text=True, check=True)


class SshOptionTests(unittest.TestCase):
    def test_only_the_pinned_host_key_is_trusted(self):
        # S3: strict checking against the pinned file, also as the global file,
        # so no system-wide entry or user setting can vouch for another key.
        call = ssh_call()
        for opt in ('"UserKnownHostsFile=$kh"', '"GlobalKnownHostsFile=$kh"', "StrictHostKeyChecking=yes",
                    "HostKeyAlgorithms=ssh-ed25519", "UpdateHostKeys=no", "IdentitiesOnly=yes"):
            self.assertIn(opt, call)
        self.assertNotRegex(text(), r"StrictHostKeyChecking=(no|accept-new|off)")

    def test_nothing_is_forwarded_to_the_vm(self):
        call = ssh_call()
        for opt in ("ForwardAgent=no", "ForwardX11=no", "ClearAllForwardings=yes", "PermitLocalCommand=no"):
            self.assertIn(opt, call)
        self.assertNotRegex(call, r"\s-[AXY]\b")

    def test_no_secret_on_the_command_line(self):
        # The PIA login is typed on the VM into pia-login.sh's hidden prompt;
        # nothing secret is a parameter or part of the ssh command.
        params = re.search(r"^param\((.*?)^\)", text(), re.S | re.M).group(1)
        self.assertNotRegex(params, r"(?i)pass|secret|token|cred")
        self.assertIn('pia-login.sh" --keep "$REGION"', remote_script())


if __name__ == "__main__":
    unittest.main()
