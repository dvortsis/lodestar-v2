"""The VM step (vm/pia-wireguard.sh) with stub curl and wg on PATH: no network.

Checks the configuration it writes, that the login never reaches a command
line, and that the work directory is gone after success, failure and a hangup.
"""

import json
import os
import signal
import stat
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]
STEP = HERE / "vm" / "pia-wireguard.sh"
CA = HERE / "vm" / "pia-ca.rsa.4096.crt"
CA_SHA256 = "32e9b1d1433ea97614f2a14c6e358e3f57c0570cc9f6b2ee812699ba696c66ab"

USER, PASSWORD, TOKEN = "p1234567", "pw-SECRET-value", "TOKEN-SECRET-value"
PRIV = "A" * 43 + "="  # stub wg genkey
PUB = "B" * 43 + "="  # stub wg pubkey
SERVER_KEY = "C" * 43 + "="
SERVERS = {"regions": [
    {"id": "other", "servers": {"wg": [{"ip": "192.0.2.9", "cn": "other401"}]}},
    {"id": "ca_test", "servers": {"wg": [{"ip": "192.0.2.10", "cn": "catest401"}]}},
]}
ADDKEY = {"status": "OK", "server_key": SERVER_KEY, "server_port": 1337, "peer_ip": "10.1.2.3"}

# Records its argv (one line per call) and answers by URL. STUB_MODE picks a failure.
CURL = r"""#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB/argv.log"
url="${@: -1}"
case "$STUB_MODE:$url" in
  hang:*token) : > "$STUB/started"; read -r _ < "$STUB/fifo"; exit 1 ;;
  tokenfail:*token) exit 22 ;;
  *:*token) cat "$STUB/token.json" ;;
  *:*servers/v6) cat "$STUB/servers.json"; printf '\nSIGNATURE\n' ;;
  addkeyfail:*addKey) printf '{"status":"ERROR"}' ;;
  *:*addKey)
    for a in "$@"; do case "$a" in pt@*) [ "$(cat "${a#pt@}")" = "$TOKEN" ] || exit 3 ;; esac; done
    cat "$STUB/addkey.json" ;;
  *) exit 99 ;;
esac
"""
WG = r"""#!/usr/bin/env bash
printf 'wg %s\n' "$*" >> "$STUB/argv.log"
case "$1" in genkey) printf '%s\n' "$PRIV" ;; pubkey) cat > /dev/null; printf '%s\n' "$PUB" ;; esac
"""


def deadline_wait(pred, seconds=10.0):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if pred():
            return True
        time.sleep(0.02)  # polling toward a deadline, not a fixed wait
    return False


class VMStep(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.tmp)])
        self.stub, self.work = self.tmp / "stub", self.tmp / "run" / "lodestar-pia"
        self.out = self.tmp / "home" / "secrets" / "wg0.conf"
        (self.stub / "bin").mkdir(parents=True)
        for name, body in (("curl", CURL), ("wg", WG)):
            p = self.stub / "bin" / name
            p.write_text(body)
            p.chmod(0o755)
        (self.stub / "token.json").write_text(json.dumps({"token": TOKEN}))
        (self.stub / "servers.json").write_text(json.dumps(SERVERS))
        (self.stub / "addkey.json").write_text(json.dumps(ADDKEY))
        self.work.mkdir(parents=True, mode=0o700)
        (self.work / "login.json").write_text(json.dumps({"username": USER, "password": PASSWORD}))
        (self.work / "region").write_text("ca_test")
        (self.work / "pia-ca.crt").write_bytes(CA.read_bytes())

    def env(self, mode=""):
        return {"PATH": f"{self.stub / 'bin'}:{os.environ.get('PATH', '/usr/bin:/bin')}", "STUB": str(self.stub), "STUB_MODE": mode,
                "TOKEN": TOKEN, "PRIV": PRIV, "PUB": PUB}

    def run_step(self, mode=""):
        return subprocess.run(["bash", str(STEP), str(self.work), str(self.out)], env=self.env(mode),
                              capture_output=True, text=True, timeout=60)

    def argv(self):
        p = self.stub / "argv.log"
        return p.read_text() if p.exists() else ""

    def test_writes_a_pinned_root_only_config_and_cleans_up(self):
        r = self.run_step()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual((r.stdout, r.stderr), ("", ""))  # nothing to discard, even so
        conf = self.out.read_text()
        self.assertEqual(conf, "[Interface]\nAddress = 10.1.2.3/32\nPrivateKey = " + PRIV + "\n\n[Peer]\n"
                         "PublicKey = " + SERVER_KEY + "\nAllowedIPs = 0.0.0.0/0\nEndpoint = 192.0.2.10:1337\n"
                         "PersistentKeepalive = 25\n")
        self.assertEqual(stat.S_IMODE(self.out.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(self.out.parent.stat().st_mode), 0o700)
        self.assertFalse(self.work.exists())  # the login, token and work files are gone
        self.assertEqual([p.name for p in self.out.parent.iterdir()], ["wg0.conf"])  # no temp file left

    def test_the_login_and_token_never_reach_a_command_line(self):
        self.run_step()
        argv = self.argv()
        for secret in (USER, PASSWORD, TOKEN, PRIV):
            self.assertNotIn(secret, argv)
        self.assertIn("--connect-to catest401::192.0.2.10:", argv)  # addKey pinned to the listed address
        self.assertIn(f"--cacert {self.work}/pia-ca.crt", argv)
        self.assertIn("https://catest401:1337/addKey", argv)

    def test_every_failure_still_deletes_the_login(self):
        cases = {"tokenfail": 12, "addkeyfail": 15}
        for mode, code in cases.items():
            with self.subTest(mode=mode):
                self.setUp()
                r = self.run_step(mode)
                self.assertEqual(r.returncode, code)
                self.assertFalse(self.work.exists())
                self.assertFalse(self.out.exists())

    def test_bad_inputs_fail_closed(self):
        for name, mutate, code in (
                ("region", lambda: (self.work / "region").write_text("../etc"), 10),
                ("login", lambda: (self.work / "login.json").write_text('{"username": ""}'), 11),
                ("unknown region", lambda: (self.work / "region").write_text("nowhere"), 13),
                ("bad server key", lambda: (self.stub / "addkey.json").write_text(
                    json.dumps(dict(ADDKEY, server_key="x\nPostUp = rm -rf /"))), 16),
                ("bad endpoint", lambda: (self.stub / "servers.json").write_text(json.dumps(
                    {"regions": [{"id": "ca_test", "servers": {"wg": [{"ip": "192.0.2.10\nx", "cn": "c"}]}}]})), 13),
                ("bad cn", lambda: (self.stub / "servers.json").write_text(json.dumps(
                    {"regions": [{"id": "ca_test", "servers": {"wg": [{"ip": "192.0.2.10", "cn": "x::1.2.3.4:"}]}}]})), 13),
                ("cn with a space", lambda: (self.stub / "servers.json").write_text(json.dumps(
                    {"regions": [{"id": "ca_test", "servers": {"wg": [{"ip": "192.0.2.10", "cn": "a b"}]}}]})), 13),
                # Bash arithmetic would run this as root, were the digits check not first.
                ("hostile port", lambda: (self.stub / "addkey.json").write_text(
                    json.dumps(dict(ADDKEY, server_port=f"a[$(touch {self.stub}/pwned)]"))), 16),
                ("port out of range", lambda: (self.stub / "addkey.json").write_text(
                    json.dumps(dict(ADDKEY, server_port=70000))), 16),
                ("hostile peer", lambda: (self.stub / "addkey.json").write_text(
                    json.dumps(dict(ADDKEY, peer_ip="10.1.2.3\nPostUp = id"))), 16)):
            with self.subTest(name):
                self.setUp()
                mutate()
                r = self.run_step()
                self.assertEqual(r.returncode, code)
                self.assertFalse(self.work.exists())
                self.assertFalse(self.out.exists())
                self.assertFalse((self.stub / "pwned").exists())

    def test_a_hangup_deletes_the_login(self):
        os.mkfifo(self.stub / "fifo")
        p = subprocess.Popen(["bash", str(STEP), str(self.work), str(self.out)], env=self.env("hang"),
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        try:
            self.assertTrue(deadline_wait((self.stub / "started").exists))
            self.assertTrue(self.work.exists())
            os.killpg(p.pid, signal.SIGHUP)  # as a cancelled job signals it (a dropped SSH session sends nothing)
            self.assertEqual(p.wait(timeout=30), 129)
        finally:
            if p.poll() is None:
                os.killpg(p.pid, signal.SIGKILL)
        self.assertFalse(self.work.exists())
        self.assertFalse(self.out.exists())

    def test_the_committed_ca_is_pias(self):
        import hashlib
        self.assertEqual(hashlib.sha256(CA.read_bytes()).hexdigest(), CA_SHA256)


if __name__ == "__main__":
    unittest.main()
