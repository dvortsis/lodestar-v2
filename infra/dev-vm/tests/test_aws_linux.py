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

    def test_lodestar_gets_no_root_equivalent_rights_on_aws(self):
        # The instance role reads the owner's PIA login: only root may reach it.
        text = SCRIPT.read_text()
        self.assertNotIn("NOPASSWD", text)
        self.assertNotIn("-G sudo", text)
        self.assertIn("rm -f /etc/sudoers.d/90-lodestar", text)
        self.assertIn("gpasswd -d lodestar docker", text)
        # The check runs after the shared bootstrap that adds the docker group.
        self.assertLess(text.index("bash /usr/local/sbin/devvm-bootstrap.sh"),
                        text.index("lodestar still has a root-equivalent group or sudo"))

    def test_metadata_service_is_root_only_and_no_key_is_installed(self):
        text = SCRIPT.read_text()
        self.assertIn("-d 169.254.169.254 -m owner ! --uid-owner 0 -j REJECT", text)
        self.assertNotIn("authorized_keys", text)


class PiaAwsModeTest(unittest.TestCase):
    def test_aws_mode_writes_only_into_a_root_owned_run_folder(self):
        # Root runs the stack from here; lodestar must not own any part of it.
        text = (HERE / "pia-login.sh").read_text()
        self.assertIn('if [[ $mode == aws ]]; then run_dir=/opt/lodestar-run;', text)
        self.assertIn('require_root_only "$run_dir"', text)
        self.assertIn('[[ $run_dir == /home/lodestar/lodestar && $mode != aws ]]', text)
        runbook = (HERE / "linux-aws.md").read_text()
        self.assertIn("everything root runs lives in `/opt/lodestar-run`, never under `~lodestar`", runbook)

    def test_aws_mode_runs_only_a_root_only_step_and_ca(self):
        # O2: the step and CA must be root-only, checked before the secret is read.
        text = (HERE / "pia-login.sh").read_text()
        check = text.index('if [[ $mode == aws ]]; then\n  require_root_only "$step"\n  require_root_only "$ca"\nfi')
        self.assertLess(check, text.index("curl --fail"))
        self.assertLess(check, text.index("aws secretsmanager get-secret-value"))

    def test_require_root_only_refuses_a_path_others_can_write(self):
        text = (HERE / "pia-login.sh").read_text()
        fn = re.search(r"^require_root_only\(\) \{\n.*?^\}\n", text, re.S | re.M).group(0)

        def run(path):
            return subprocess.run(["bash", "-c", fn + 'require_root_only "$1"; echo ok', "_", path],
                                  capture_output=True, text=True)

        with tempfile.TemporaryDirectory() as d:
            f = pathlib.Path(d) / "pia-wireguard.sh"
            f.write_text("")
            r = run(str(f))  # under /tmp (sticky, world-writable) and, as a non-root user, not root's
            self.assertEqual(r.returncode, 2, r)
            self.assertIn("must be owned by root and writable by root only", r.stderr)
            self.assertNotIn("ok", r.stdout)
            link = pathlib.Path(d) / "link"
            link.symlink_to("/bin/sh")
            self.assertEqual(run(str(link)).returncode, 2)
        root_owned = next((p for p in ("/usr/bin/env", "/bin/sh") if pathlib.Path(p).stat().st_uid == 0
                           and not pathlib.Path(p).is_symlink()), None)
        if root_owned:
            r = run(root_owned)
            self.assertEqual((r.returncode, r.stdout.strip()), (0, "ok"), r)

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
