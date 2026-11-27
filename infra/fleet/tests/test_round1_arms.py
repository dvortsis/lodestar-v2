"""X-cr-round1-collect: one arm's leak test among several (docs/plan/2026-11-27-revised-path-to-100.md
§5.2 item 3; runner's grant, 2026-11-27).

leak-test.sh with LODESTAR_ARM=b runs against test_steps' stubs, rewritten for arm b's
names: gluetun-b, the lodestar-b project and bridge, the -b probe containers and rule tags,
and the endpoint block scoped to the arm's bridge. The host also holds another arm's (c's)
recreate guard, host guard and ICMP rule, and its egress counter, which rises on every read
because its tunnel is carrying traffic (the stub simulates that traffic; what the fixture
proves is that nothing removed or reset the counter rule). Any command that touched them would be one the stubs
don't know, which fails the run (badargs). The fixture: during the test and its failure
cleanup, c's guards stay, its counter keeps rising, and nothing names gluetun-c, so its
egress identity and tunnel are untouched. Docker and iptables are stubs: the box's
rehearsal runs the real ones (docs/soak/runsheets/host-round1-runsheet.md).
"""

import re
import unittest
from pathlib import Path

import yaml

from tests import test_steps as ts

VM = ts.VM
OTHER = {  # arm c's rules, as `iptables -S` prints them
    "cguard": "-A DOCKER-USER -s 198.51.100.99/32 -p udp -m udp --sport 1338 -m comment --comment lodestar-leak-guard-c -j DROP",
    "chost": "-A INPUT -s 198.51.100.99/32 -p udp -m udp --sport 1338 -m comment --comment lodestar-leak-host-c -j DROP",
    "cicmp": "-A POSTROUTING -d 198.51.100.99/32 -p icmp -m comment --comment lodestar-leak-host-c -j DROP",
    "ccounter": None,  # its egress counter (DOCKER-USER), listed by -L while it's there
}


def for_arm(text, arm="b"):
    """test_steps' stub text, with arm `arm`'s container names, tags and bridge-scoped block."""
    s = "-" + arm
    for old, new in (("lodestar-leak-flow", "lodestar-leak-flow" + s), ("lodestar-leak-drive", "lodestar-leak-drive" + s),
                     ("lodestar-leak-egress", "lodestar-leak-egress" + s), ("lodestar-leak-guard", "lodestar-leak-guard" + s),
                     ("lodestar-leak-host", "lodestar-leak-host" + s), ("container:gluetun", "container:gluetun" + s),
                     ("stop -t 10 gluetun", "stop -t 10 gluetun" + s), ('"start gluetun"', '"start gluetun' + s + '"'),
                     ("exec gluetun ", "exec gluetun" + s + " "), ("lodestar_default", "lodestar" + s + "_default"),
                     ("DOCKER-USER 1 -d 192.0.2.77/32 -p udp --dport 1337 -j DROP",
                      "DOCKER-USER 1 -i br-abc123 -d 192.0.2.77/32 -p udp --dport 1337 -j DROP"),
                     ("-D DOCKER-USER -d 192.0.2.77/32 -p udp --dport 1337 -j DROP",
                      "-D DOCKER-USER -i br-abc123 -d 192.0.2.77/32 -p udp --dport 1337 -j DROP")):
        text = text.replace(old, new)
    return text


def with_other_arm(iptables):
    """Arm c's rules in every listing, and its counter rising on every read, ahead of b's
    own line (a match on the wrong tag would read c's packets)."""
    listings = {'-S DOCKER-USER")\n    echo "-N DOCKER-USER"': "cguard", '-S INPUT")\n    echo "-P INPUT ACCEPT"': "chost",
                '-S POSTROUTING")\n    echo "-P POSTROUTING ACCEPT"': "cicmp"}
    for anchor, key in listings.items():
        assert anchor in iptables, anchor
        iptables = iptables.replace(anchor, anchor + f'\n    have {key} && echo "{OTHER[key]}"')
    anchor = '"-L DOCKER-USER -v -x -n")\n'
    rising = ('"-L DOCKER-USER -v -x -n")\n    up ccount\n'
              "    have ccounter && printf '%8d %8d            0    --  br-ccc333 eth0    0.0.0.0/0            0.0.0.0/0            "
              "/* lodestar-leak-egress-c */\\n' $((5000 + $(n ccount))) 999\n")
    assert anchor in iptables
    iptables = iptables.replace(anchor, rising)
    # c's counter is listed even while b has none: only b's own line may be read as b's.
    iptables = iptables.replace('    [ -e "$STUB/counter" ] || exit 0\n    [ -n "$COUNTER_UNREADABLE" ] && exit 1\n',
                                '    [ -n "$COUNTER_UNREADABLE" ] && exit 1\n    [ -e "$STUB/counter" ] || exit 0\n')
    return iptables


def arm_compose(docker):
    """b's stack recreates under its own project and suffix only."""
    docker = docker.replace("      *Running*lodestar)", '      *Running*lodestar-b) [ -n "$LODESTAR_B" ] && echo true || echo false ;;\n      *Running*lodestar)', 1)
    anchor = "  compose)\n"
    check = ('  compose)\n    case "$*" in "compose -p lodestar-b -f "*) ;; *) bad "not arm b\'s project: $*" ;; esac\n'
             '    [ "$LODESTAR_ARM_SUFFIX" = -b ] || bad "suffix: $LODESTAR_ARM_SUFFIX"\n'
             '    [ "$LODESTAR_UI_PORT" = 8092 ] || bad "port: $LODESTAR_UI_PORT"\n')
    assert anchor in docker
    return docker.replace(anchor, check, 1)


class OneArm(unittest.TestCase):
    run_script = ts.VMScripts.run_script
    run_script_fixtures = ts.VMScripts.run_script_fixtures
    argv = ts.VMScripts.argv
    copies = ts.VMScripts.copies
    log = ts.VMScripts.log
    signal_at = ts.VMScripts.signal_at

    def setUp(self):
        ts.VMScripts.setUp(self)
        for name, body in (("docker", arm_compose(for_arm(ts.STUB_DOCKER))),
                           ("iptables", with_other_arm(for_arm(ts.STUB_IPTABLES)))):
            (self.stub / "bin" / name).write_text(body)
        for key in OTHER:
            (self.stub / key).write_text("1")

    def env(self, **flags):
        return dict(ts.VMScripts.env(self, **flags), LODESTAR_ARM="b", LODESTAR_UI_PORT="8092")

    def c_packets(self):
        """Arm c's egress counter as the host reads it now (its tunnel keeps sending)."""
        import subprocess
        out = subprocess.run([str(self.stub / "bin" / "iptables"), "-L", "DOCKER-USER", "-v", "-x", "-n"],
                             env=self.env(), capture_output=True, text=True).stdout
        m = re.search(r"(?m)^\s*(\d+) .*/\* lodestar-leak-egress-c \*/$", out)
        return int(m[1]) if m else None

    def assert_other_arm_intact(self, before):
        for key in OTHER:
            self.assertEqual(self.copies(key), 1, key)  # c's guards and counter are all still there
        after = self.c_packets()
        self.assertIsNotNone(after)
        self.assertGreater(after, before)  # and its counter kept rising
        log = self.log()
        self.assertNotIn("gluetun-c", log)  # c's tunnel was never stopped, started or recreated
        self.assertNotRegex(log, r"-[ADI] \S+ .*lodestar-leak-\w+-c ")
        for line in log.splitlines():
            if line.startswith("docker compose"):
                self.assertIn(" -p lodestar-b ", line)

    def test_an_arms_leak_test_passes_and_touches_only_its_own_rules(self):
        before = self.c_packets()
        rc, lines, raw = self.run_script("leak-test.sh")
        self.assertEqual(rc, 0, raw)
        self.assertEqual(lines[-1], "ok leak_test")
        self.assertIn("count host_egress_packets_down 0", lines)  # b's own counter, not c's 5000-odd
        log = self.log()
        self.assertIn("iptables -I DOCKER-USER 1 -i br-abc123 -d 192.0.2.77/32 -p udp --dport 1337 -j DROP", log)
        self.assertIn("--comment lodestar-leak-guard-b", log)
        self.assertIn("--comment lodestar-leak-host-b", log)
        self.assertIn("network inspect -f {{.EnableIPv6}} lodestar-b_default", log)
        self.assertIn("--name lodestar-leak-flow-b --network container:gluetun-b", log)
        self.assertEqual((self.copies("guard"), self.copies("host"), self.copies("icmp")), (0, 0, 0))
        self.assert_other_arm_intact(before)

    def test_its_failure_cleanup_leaves_the_other_arm_intact(self):
        for flags in ({"LEAK_TCP": 1}, {"COMPOSE_FAIL": 1}, {"NO_POST_RULES": 1}, {"COUNTER_FAILS": 1},
                      {"GUARD_STUCK": 1}):
            with self.subTest(**flags):
                self.setUp()
                before = self.c_packets()
                rc, lines, _ = self.run_script("leak-test.sh", **flags)
                self.assertEqual((rc, lines[-1]), (1, "fail leak_test"))
                self.assertFalse((self.stub / "blocked").exists())
                self.assert_other_arm_intact(before)

    def test_a_signal_mid_recreate_cleans_up_only_this_arm(self):
        before = self.c_packets()
        rc, out = self.signal_at("composing", BLOCK_COMPOSE=1)
        self.assertNotEqual(rc, 0)
        self.assert_other_arm_intact(before)
        self.assertEqual((self.copies("guard"), self.copies("host"), self.copies("icmp")), (0, 0, 0))
        self.assertNotIn("gluetun-c", self.log())

    def test_an_arm_needs_its_own_port_and_its_lodestar_down(self):
        env = ts.VMScripts.env(self)
        import subprocess
        r = subprocess.run(self.argv("leak-test.sh"), env=dict(env, LODESTAR_ARM="b"), capture_output=True, text=True,
                           timeout=60)
        self.assertEqual(r.stdout.splitlines()[-2:], ["fail args_valid", "fail leak_test"])
        r = subprocess.run(self.argv("leak-test.sh"), env=dict(env, LODESTAR_ARM="B", LODESTAR_UI_PORT="8092"),
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(r.stdout.splitlines()[-2:], ["fail args_valid", "fail leak_test"])
        self.assertNotIn("iptables", self.log())
        rc, lines, _ = self.run_script("leak-test.sh", LODESTAR_B=1)  # its own Lodestar is running
        self.assertEqual((rc, lines[-2:]), (1, ["fail block_running", "fail leak_test"]))
        self.assertNotIn("iptables", self.log())

    def test_the_cloud_lanes_single_stack_is_unchanged(self):
        """No LODESTAR_ARM: the stack's defaults are today's names and port."""
        doc = yaml.safe_load((VM / "compose.yaml").read_text())

        def default(v):
            return re.sub(r"\$\{[A-Z_]+:-([^}]*)\}", r"\1", v)
        svc = doc["services"]
        self.assertEqual(default(svc["gluetun"]["container_name"]), "gluetun")
        self.assertEqual(default(svc["lodestar"]["container_name"]), "lodestar")
        self.assertEqual(default(svc["docker-proxy"]["container_name"]), "lodestar-docker-proxy")
        self.assertEqual([default(p) for p in svc["gluetun"]["ports"]], ["127.0.0.1:8090:8090"])
        self.assertEqual(doc["name"], "lodestar")
        # Round 1's coordinator sets "no", so a reboot brings no arm back untested.
        self.assertEqual(default(svc["gluetun"]["restart"]), "unless-stopped")
        for s in svc.values():  # each arm's names and port follow its suffix and port, nothing else
            for v in [s.get("container_name", ""), str(s.get("restart", ""))] + s.get("ports", []):
                self.assertEqual(set(re.findall(r"\$\{([A-Z_]+)", v))
                                 - {"LODESTAR_ARM_SUFFIX", "LODESTAR_UI_PORT", "LODESTAR_GLUETUN_RESTART"}, set())


class DeclaredSettingsReachTheContainer(unittest.TestCase):
    """The coordinator's `up` sets each arm's declared settings in Compose's environment; only
    names the lodestar service passes through reach the container (the self-review's find:
    LODESTAR_INTENSITY didn't, so A would have run Balanced)."""

    def test_every_variable_up_sets_is_passed_through(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location("round1_coordinator", VM / "round1-coordinator.py")
        co = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(co)
        env = yaml.safe_load((VM / "compose.yaml").read_text())["services"]["lodestar"]["environment"]
        bare = {e for e in env if "=" not in e}
        all_on = {s: s != "route_backoff" for s in co.collect.SWITCH_ENV if s != "safe_restart"}
        for preset in ("balanced", "aggressive"):
            sets = co.arm_switch_env({"switches": all_on, "preset": preset})
            self.assertEqual(set(sets) - bare, set(), preset)
        self.assertEqual(co.arm_switch_env({"switches": all_on, "preset": "aggressive"})["LODESTAR_INTENSITY"],
                         "aggressive")


if __name__ == "__main__":
    unittest.main()
