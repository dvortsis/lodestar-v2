"""The AWS Linux test instance's bootstrap (aws-linux-bootstrap.sh) reads the
Linode cloud-init template, so a template change that breaks it fails here."""
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent.parent
SCRIPT = HERE / "aws-linux-bootstrap.sh"


def embedded_python() -> str:
    m = re.search(r"<<'EOF'\n(import sys, yaml.*?)\nEOF\n", SCRIPT.read_text(), re.S)
    assert m, "the bootstrap's embedded Python was not found"
    return m.group(1)


class BootstrapTest(unittest.TestCase):
    def test_shell_scripts_parse(self):
        for name in ("aws-linux-bootstrap.sh", "pia-login.sh"):
            subprocess.run(["bash", "-n", str(HERE / name)], check=True)

    def test_extracts_packages_user_and_bootstrap_from_the_template(self):
        try:
            import yaml  # noqa: F401  (on the instance, cloud-init brings it)
        except ImportError:
            self.skipTest("PyYAML is not installed")
        with tempfile.TemporaryDirectory() as out:
            subprocess.run([sys.executable, "-I", "-", str(HERE / "cloud-init.yaml.tmpl"), out],
                           input=embedded_python(), text=True, check=True)
            o = pathlib.Path(out)
            pkgs = o.joinpath("packages").read_text().split()
            for p in ("wireguard-tools", "tcpdump", "jq", "git", "tmux"):
                self.assertIn(p, pkgs)
            script = o.joinpath("devvm-bootstrap.sh").read_text()
            self.assertIn('echo "ok $(date -u +%FT%TZ)" > /var/lib/devvm-bootstrap.status', script)
            self.assertIn("fallocate -l 8G /swapfile", script)
            self.assertNotIn("__", script.replace("__file__", ""))
            self.assertEqual(o.joinpath("sudo").read_text().strip(), "ALL=(ALL) NOPASSWD:ALL")

    def test_metadata_service_is_root_only_and_no_key_is_installed(self):
        text = SCRIPT.read_text()
        self.assertIn("-d 169.254.169.254 -m owner ! --uid-owner 0 -j REJECT", text)
        self.assertNotIn("authorized_keys", text)


class PiaAwsModeTest(unittest.TestCase):
    def test_aws_mode_reads_into_memory_and_keeps_nothing(self):
        text = (HERE / "pia-login.sh").read_text()
        aws = text[text.index("elif [[ $mode == aws ]]"):text.index("\nelse\n  read -r -s")]
        self.assertIn("--query SecretString --output text 2>/dev/null)", aws)
        # No redirect writes the value anywhere: only stderr notes and /dev/null.
        stripped = re.sub(r"2>/dev/null|>/dev/null|>&2", "", aws)
        self.assertNotIn(">", stripped)
        self.assertIn("unset pia_secret", aws)


if __name__ == "__main__":
    unittest.main()
