#!/usr/bin/env bash
# ==============================================================================
# test_linux_privilege_boundary.sh (X-pia-vm-helper, from #632)
#
#   --offline     (the default) No root, no network, no cloud, no terraform.
#                 Runs on stock Ubuntu 24.04 (bash, python3, python3-yaml):
#                 1. Renders cloud-init/ubuntu24.yaml.tftpl with test values
#                    and parses it as YAML.
#                 2. The sudoers tie test: every privileged command main's fleet
#                    steps (infra/fleet/steps/**) and deliver-pia
#                    (infra/pia-bridge/**) invoke must be allowed, without a
#                    password, by the template's sudoers lines. A sudo use the
#                    test can't classify fails it. A self-check proves the tie
#                    test fails when only the helper rule is left.
#                 3. The helper: bash -n, shellcheck when installed, its fixed
#                    verb list, and its paths and gluetun digest against
#                    compose.vpn.yaml and the fleet's stack.
#                 4. The helper's wg0.conf schema, CIDR and firewall checks run
#                    under every awk on the host (mawk is Ubuntu 24.04's awk).
#   --privileged  Also #632's integration test: a real unprivileged user, its
#                 sudoers rule and a network namespace. It changes the host, so
#                 it runs only as root in CI or with LODESTAR_ALLOW_PRIVILEGED_TESTS=1,
#                 in a disposable container or VM, and restores what it changed.
#
# Test addresses come from the documentation ranges (RFC 5737).
# ==============================================================================
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TF_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$TF_DIR/../../.." && pwd)"

MODE="offline"
for arg in "$@"; do
  case "$arg" in
    --offline) MODE="offline" ;;
    --privileged) MODE="privileged" ;;
    *) echo "usage: $0 [--offline|--privileged]" >&2; exit 2 ;;
  esac
done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lodestar-priv-test.XXXXXX")"
cleanup_work() { rm -rf -- "$WORK"; }
trap cleanup_work EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }

# ------------------------------------------------------------------------------
# 1-3. Render, parse, tie test, helper checks (python3 + PyYAML only)
# ------------------------------------------------------------------------------
echo "=== Offline: render the template, tie test, helper checks ==="
python3 - "$REPO_ROOT" "$WORK" <<'PY'
import base64, hashlib, os, re, shlex, shutil, subprocess, sys, fnmatch
from pathlib import Path

try:
    import yaml
except ImportError:
    sys.exit("FAIL: python3-yaml is required (Ubuntu ships it with cloud-init)")

ROOT, WORK = Path(sys.argv[1]), Path(sys.argv[2])
TF = ROOT / "infra/akamai/terraform"
TEMPLATE = TF / "cloud-init/ubuntu24.yaml.tftpl"
COMPOSE = TF / "pia-example/compose.vpn.yaml"
CA = ROOT / "infra/pia-bridge/vm/pia-ca.rsa.4096.crt"
FLEET_COMPOSE = ROOT / "infra/fleet/steps/vm/compose.yaml"
# PIA's CA from github.com/pia-foss/manual-connections (ca.rsa.4096.crt),
# the file deliver-pia already pins (docs/decisions/2026-11-09-OP-2b.md).
CA_SHA256 = "32e9b1d1433ea97614f2a14c6e358e3f57c0570cc9f6b2ee812699ba696c66ab"
HELPER = "/usr/local/sbin/lodestar-vpn-helper"
HELPER_RULE = f"lodestar ALL=(root) NOPASSWD: {HELPER}"
HELPER_VERBS = {
    "preflight", "write-config", "verify-permissions", "validate-compose", "start-vpn",
    "start-crawler", "verify-tunnel", "stop-vpn", "cleanup", "wg-handshake", "wg-transfer",
    "wg-endpoints", "query-egress", "query-ipinfo", "query-dns", "verify-dns-firewall",
    "inspect-firewall", "inspect-ipv6",
}
# Ubuntu 24.04's sudo secure_path (/etc/sudoers), where `sudo -n bash` finds bash.
SECURE_PATH = ["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin", "/sbin", "/bin", "/snap/bin"]
# The files whose privileged commands the template must allow: main's fleet
# steps and deliver-pia, plus the manual bridge once X-pia-bridge-manual adds it.
SCAN = [ROOT / "infra/fleet/steps", ROOT / "infra/pia-bridge", TF / "scripts"]
# Placeholder key material only (not real keys).
TEST_KEYS = ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestKeyOne test-one",
             "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestKeyTwo test-two"]
TEST_HOST_PRIV = "-----BEGIN OPENSSH PRIVATE KEY-----\nTESTONLYNOTAKEY\n-----END OPENSSH PRIVATE KEY-----\n"
TEST_HOST_PUB = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestHostKey"
# Linode's API takes user_data base64-encoded, at most 65535 characters.
USER_DATA_MAX = 65535


def fail(msg):
    sys.exit(f"FAIL: {msg}")


def ok(msg):
    print(f"PASS: {msg}")


# ---- A small renderer for the template's Terraform constructs ---------------
def tf_indent(n, s):
    lines = s.split("\n")
    # As Terraform's indent(): every line after the first, empty ones too.
    return "\n".join([lines[0]] + [" " * n + l for l in lines[1:]])


def render(tpl, env):
    out, stack = [], []
    lines = tpl.split("\n")
    i = 0

    def active():
        return all(stack)

    def expr(e):
        e = e.strip()
        m = re.fullmatch(r"indent\((\d+),\s*(.+)\)", e)
        if m:
            return tf_indent(int(m.group(1)), expr(m.group(2)))
        m = re.fullmatch(r"trimspace\((.+)\)", e)
        if m:
            return expr(m.group(1)).strip()
        if re.fullmatch(r"[a-z_][a-z0-9_]*", e) and e in env:
            return env[e]
        raise ValueError(f"the renderer doesn't know the expression ${{{e}}}")

    def interp(line):
        line = line.replace("$${", "\x00D").replace("%%{", "\x00P")
        if "%{" in line:
            raise ValueError(f"an inline template directive: {line!r}")
        line = re.sub(r"\$\{([^}]*)\}", lambda m: expr(m.group(1)), line)
        return line.replace("\x00D", "${").replace("\x00P", "%{")

    while i < len(lines):
        line = lines[i]
        d = re.fullmatch(r"%\{ (.*?) ~\}", line)
        if d:
            body = d.group(1)
            m = re.fullmatch(r"for (\w+) in (\w+)", body)
            if m:
                end = next(j for j in range(i + 1, len(lines)) if lines[j] == "%{ endfor ~}")
                for item in env[m.group(2)]:
                    sub = render("\n".join(lines[i + 1:end]), {**env, m.group(1): item})
                    if active():
                        out.append(sub)
                i = end + 1
                continue
            m = re.fullmatch(r'if (\w+) != ""', body)
            if m:
                stack.append(env[m.group(1)] != "")
            elif body == "endif":
                stack.pop()
            else:
                raise ValueError(f"unknown directive {body!r}")
            i += 1
            continue
        if active():
            out.append(interp(line))
        i += 1
    return "\n".join(out)


tpl = TEMPLATE.read_text()
compose_text = COMPOSE.read_text()
ca_text = CA.read_text()
if hashlib.sha256(CA.read_bytes()).hexdigest() != CA_SHA256:
    fail(f"{CA} is not the pinned PIA CA")
ok("PIA's CA is the pinned file (SHA-256 " + CA_SHA256 + ")")

base = {"ssh_public_keys": TEST_KEYS, "role_label": "worker-01", "compose_vpn_content": compose_text,
        "pia_ca_cert_content": ca_text}
variants = {
    "no host key": {**base, "host_key_private": "", "host_key_public": ""},
    "pinned host key": {**base, "role_label": "main", "host_key_private": TEST_HOST_PRIV, "host_key_public": TEST_HOST_PUB},
}
docs = {}
for name, env in variants.items():
    try:
        text = render(tpl, env)
    except (ValueError, KeyError, StopIteration) as e:
        fail(f"rendering ({name}): {e}")
    if not text.startswith("#cloud-config\n"):
        fail(f"rendered user data ({name}) doesn't start with #cloud-config")
    doc = yaml.safe_load(text)
    if not isinstance(doc, dict):
        fail(f"rendered user data ({name}) isn't a YAML mapping")
    b64 = len(base64.b64encode(text.encode()))
    if b64 > USER_DATA_MAX:
        fail(f"user data ({name}) is {b64} base64 characters, over Linode's {USER_DATA_MAX}")
    docs[name] = (text, doc)
    ok(f"template renders and parses ({name}; {b64} base64 characters of {USER_DATA_MAX})")
(WORK / "user-data.yaml").write_text(docs["pinned host key"][0])

# cloud-init's own schema check, where cloud-init is installed (stock Ubuntu
# 24.04 has it). Main's `ssh_genkeytypes: []` (OP-2: generate no other host
# key) is the one finding allowed: the schema wants a non-empty list, but
# cloud-init reads an empty one as "none".
if shutil.which("cloud-init"):
    for name, (text, _) in docs.items():
        ud = WORK / f"schema-{name.replace(' ', '-')}.yaml"
        ud.write_text(text)
        r = subprocess.run(["cloud-init", "schema", "-c", str(ud), "--annotate"], capture_output=True, text=True)
        errs = [l for l in r.stdout.splitlines() if re.search(r"\t# E\d+$", l)]
        allowed = [l for l in errs if l.startswith("ssh_genkeytypes: []")]
        if r.returncode and (errs != allowed or not errs):
            fail(f"cloud-init schema ({name}):\n{r.stdout}{r.stderr}")
        ok(f"cloud-init schema ({name})" + (" (main's ssh_genkeytypes: [] noted)" if allowed else ""))
else:
    print("SKIP: cloud-init not installed; no schema check")
text, doc = docs["no host key"]

if doc.get("hostname") != "lodestar-worker-01" or docs["pinned host key"][1].get("ssh_keys", {}).get("ed25519_public") != TEST_HOST_PUB:
    fail("hostname or the pinned host key didn't render")
if [k for k in doc if k in ("ssh_keys", "ssh_deletekeys")]:
    fail("host key settings rendered without a host key")

# ---- Files the template writes -----------------------------------------------
files = {f["path"]: f for f in doc.get("write_files", [])}
def want_file(path, perms, owner="root:root"):
    f = files.get(path)
    if not f:
        fail(f"cloud-init doesn't write {path}")
    if f.get("owner") != owner or f.get("permissions") != perms:
        fail(f"{path} is {f.get('owner')} {f.get('permissions')}, want {owner} {perms}")
    return f["content"]

sudoers_d = {p: f for p, f in files.items() if p.startswith("/etc/sudoers.d/")}
if list(sudoers_d) != ["/etc/sudoers.d/90-lodestar-vpn-helper"]:
    fail(f"sudoers.d files: {sorted(sudoers_d)}")
helper_sudoers = want_file("/etc/sudoers.d/90-lodestar-vpn-helper", "0440")
if helper_sudoers != HELPER_RULE + "\n":
    fail(f"the helper's sudoers file is {helper_sudoers!r}")
if want_file("/etc/lodestar/vpn/compose.yaml", "0644") != compose_text:
    fail("the installed compose.yaml isn't pia-example/compose.vpn.yaml byte for byte")
if want_file("/etc/lodestar/vpn/ca.rsa.4096.crt", "0644") != ca_text:
    fail("the installed CA isn't the pinned file byte for byte")
helper = want_file(HELPER, "0750")
want_file("/usr/local/sbin/lodestar-bootstrap.sh", "0700")
ok("cloud-init writes the helper, its sudoers rule, compose.yaml and the CA, root-owned")

# ---- Main's sudo model is kept -------------------------------------------------
users = [u for u in doc.get("users", []) if isinstance(u, dict)]
lod = [u for u in users if u.get("name") == "lodestar"]
if len(lod) != 1 or [u for u in users if u is not lod[0] and u.get("sudo")]:
    fail("exactly one user, lodestar, may have sudo")
lod = lod[0]
if lod.get("lock_passwd") is not True or doc.get("ssh_pwauth") is not False or doc.get("disable_root") is not True:
    fail("lodestar's password must stay locked, with password SSH and root login off")
user_sudo = lod.get("sudo")
user_sudo = [user_sudo] if isinstance(user_sudo, str) else list(user_sudo or [])
if user_sudo != ["ALL=(ALL) NOPASSWD:ALL"]:
    fail(f"main's sudo grant changed: {user_sudo} (narrowing it is X-fleet-sudo-narrow)")
ok("main's sudo model is kept: lodestar ALL=(ALL) NOPASSWD:ALL, password locked")

# Cloud-init writes users.sudo as "<user> <spec>" lines in /etc/sudoers.d/90-cloud-init-users.
sudoers_lines = [f"lodestar {s}" for s in user_sudo]
for f in sudoers_d.values():
    sudoers_lines += [l for l in f["content"].splitlines() if l.strip() and not l.lstrip().startswith("#")]
(WORK / "sudoers").write_text("\n".join(sudoers_lines) + "\n")


# ---- The tie test ------------------------------------------------------------
def parse_rule(line):
    """One sudoers user spec of the shapes the template uses:
    user host=(runas) [NOPASSWD:] cmd [args], cmd ..."""
    m = re.fullmatch(r"(\S+)\s+(\S+)\s*=\s*(?:\(([^)]*)\))?\s*(NOPASSWD:)?\s*(.+)", line.strip())
    if not m:
        fail(f"a sudoers line the tie test can't read: {line!r}")
    user, host, runas, nopasswd, cmds = m.groups()
    return {"user": user, "host": host, "runas": [r.strip() for r in (runas or "root").split(":")[0].split(",")],
            "nopasswd": bool(nopasswd), "cmds": [c.strip() for c in cmds.split(",")]}


def allows(rule, user, argv):
    if rule["user"] not in (user, "ALL") or rule["host"] != "ALL" or not rule["nopasswd"]:
        return False  # `sudo -n` can't answer a password prompt
    if not ({"ALL", "root"} & set(rule["runas"])):
        return False
    for spec in rule["cmds"]:
        if spec == "ALL":
            return True
        parts = spec.split(None, 1)
        path, args = parts[0], (parts[1] if len(parts) > 1 else None)
        cmd = argv[0]
        if "/" in cmd:
            cands = [cmd]
        else:
            cands = [f"{d}/{cmd}" for d in SECURE_PATH]
        if path not in cands:
            continue
        if args is None:
            return True  # a command with no arguments listed takes any
        if args == '""':
            if len(argv) == 1:
                return True
            continue
        if fnmatch.fnmatchcase(" ".join(argv[1:]), args):
            return True
    return False


class Unreadable(Exception):
    pass


def code_lines(p, text):
    """The file's lines with comments cut: Python's by its tokenizer (a # in a
    string stays), shell's and PowerShell's at a # that starts a word outside
    quotes and ${...}."""
    lines = text.splitlines()
    if p.suffix == ".py":
        import io, tokenize
        cut = {}
        try:
            for tok in tokenize.generate_tokens(io.StringIO(text).readline):
                if tok.type == tokenize.COMMENT:
                    cut[tok.start[0]] = tok.start[1]
        except (tokenize.TokenError, SyntaxError) as e:
            raise Unreadable(f"{p}: can't tokenize: {e}")
        return [l[:cut[i]] if i in cut else l for i, l in enumerate(lines, 1)]
    out = []
    for l in lines:
        q, brace, end = None, 0, len(l)
        for i, ch in enumerate(l):
            if q:
                if ch == q:
                    q = None
            elif ch in "'\"":
                q = ch
            elif ch == "{" and i and l[i - 1] == "$":
                brace += 1
            elif ch == "}" and brace:
                brace -= 1
            elif ch == "#" and not brace and (i == 0 or l[i - 1].isspace()):
                end = i
                break
        out.append(l[:end])
    return out


def py_constants(text):
    """Module-level NAME = "string" assignments, to resolve {NAME} in f-strings."""
    import ast
    consts = {}
    for node in ast.parse(text).body:
        if isinstance(node, ast.Assign) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
            for t in node.targets:
                if isinstance(t, ast.Name):
                    consts[t.id] = node.value.value
    return consts


def sudo_argv(where, s, consts):
    """argv after `sudo -n` in a command string. {NAME} resolves to a module
    constant; any other {...} stands as <arg>."""
    m = re.match(r"sudo(\s+-n)?\s+(.*)$", s, re.S)
    if not m:
        raise Unreadable(f"{where}: sudo with no command: {s!r}")
    if not m.group(1):
        raise Unreadable(f"{where}: sudo without -n (a prompt would hang the step)")
    rest = re.sub(r"\{([A-Za-z_][A-Za-z0-9_]*)\}", lambda x: consts.get(x.group(1), x.group(0)), m.group(2))
    rest = re.sub(r"\{[^}]*\}", "<arg>", rest).strip()
    try:
        argv = shlex.split(rest)
    except ValueError:
        raise Unreadable(f"{where}: can't split {rest!r}")
    if not argv:
        raise Unreadable(f"{where}: sudo with no command")
    if argv[0].startswith("-"):
        raise Unreadable(f"{where}: sudo option {argv[0]} (the tie test reads -n only)")
    return argv


def scan_file(p, rel):
    """Every privileged command in one file, as (where, argv). A string literal
    that starts with `sudo` is a command; in shell, so is a sudo command word.
    Elsewhere, `sudo` followed by an option is a command the test can't read
    (it fails); other mentions (prose in messages) are not commands."""
    try:
        text = p.read_text()
    except UnicodeDecodeError:
        return []
    shell = p.suffix in (".sh", "")
    consts = py_constants(text) if p.suffix == ".py" else {}
    found = []
    for n, code in enumerate(code_lines(p, text), 1):
        where = f"{rel}:{n}"
        for m in re.finditer(r"\bsudo\b", code):
            i = m.start()
            before = code[i - 1] if i else ""
            if before in "'\"":
                close = code.find(before, i)
                if close < 0:
                    raise Unreadable(f"{where}: a sudo literal the test can't read: {code.strip()}")
                found.append((where, sudo_argv(where, code[i:close], consts)))
            elif shell and (i == 0 or before.isspace() or before in ";|&(") and re.match(r"sudo\s", code[i:]):
                found.append((where, sudo_argv(where, re.split(r"[;&|)]", code[i:])[0], consts)))
            elif re.match(r"sudo\s+-", code[i:]) or code[i:i + 5] == "sudo\"" or code[i:i + 5] == "sudo'":
                raise Unreadable(f"{where}: a sudo use the tie test can't read: {code.strip()}")
    return found


def privileged_commands(tops):
    found = []
    for top in tops:
        if not top.exists():
            continue
        for p in sorted(top.rglob("*")):
            if not p.is_file() or "tests" in p.relative_to(top).parts or p.suffix not in (".py", ".sh", ".ps1", ""):
                continue
            found += scan_file(p, p.relative_to(ROOT) if p.is_relative_to(ROOT) else p)
    return found


rules = [parse_rule(l) for l in sudoers_lines]
try:
    cmds = privileged_commands(SCAN)
except Unreadable as e:
    fail(str(e))
names = sorted({a[0] for _, a in cmds})
if not cmds or "bash" not in names or "flock" not in names:
    fail(f"the tie test found too few privileged commands ({names}); its scan is broken")
for where, argv in cmds:
    if not any(allows(r, "lodestar", argv) for r in rules):
        fail(f"{where}: `sudo -n {' '.join(argv)}` isn't allowed by the template's sudoers lines")
ok(f"tie test: all {len(cmds)} privileged commands ({', '.join(names)}) are allowed by the template's sudoers lines")

# Self-check: with only the helper rule (the X-fleet-sudo-narrow end state before
# the steps move), the tie test must fail for the fleet's commands.
narrow = [parse_rule(HELPER_RULE)]
uncovered = [w for w, a in cmds if not any(allows(r, "lodestar", a) for r in narrow)]
if not uncovered:
    fail("tie self-check: the helper rule alone covered every fleet command; the matcher is broken")
if not allows(narrow[0], "lodestar", [HELPER, "preflight"]) or allows(narrow[0], "lodestar", ["bash", "-c", "id"]):
    fail("tie self-check: the matcher misreads the helper rule")
if allows(parse_rule("lodestar ALL=(ALL) ALL"), "lodestar", ["bash"]):
    fail("tie self-check: a rule that needs a password was taken for sudo -n")
ok(f"tie self-check: with the helper rule alone, {len(uncovered)} commands are uncovered, as they must be")

# Scanner self-check: comments that hide nothing, constants that resolve, prose
# that isn't a command, and forms it must refuse.
sc = WORK / "scan"
sc.mkdir()
cases = {
    "hash_in_string.py": ('X = "echo #"; Y = "sudo -n rm -f /x"\n', [["rm", "-f", "/x"]]),
    "param_expansion.sh": ('x="${a#b}"; sudo -n iptables -F\n', [["iptables", "-F"]]),
    "comment_only.sh": ("  # sudo -n reboot\n", []),
    "helper_const.py": (f'HELPER = "{HELPER}"\ncmd = f"sudo -n {{HELPER}} preflight"\n', [[HELPER, "preflight"]]),
    "prose.py": ('msg = f"Verify {user} has sudo access to the helper"\n', []),
}
for name, (body, want) in cases.items():
    (sc / name).write_text(body)
    try:
        got = [a for _, a in scan_file(sc / name, name)]
    except Unreadable as e:
        fail(f"scanner self-check {name}: {e}")
    if got != want:
        fail(f"scanner self-check {name}: found {got}, want {want}")
for name, body in {"list.py": 'X = ["sudo", "-n", "id"]\n', "no_n.py": 'X = "sudo id"\n',
                   "option.py": 'X = "sudo -n -u root id"\n', "embedded.py": 'X = "true && sudo -n id"\n',
                   "no_n.sh": "sudo reboot\n"}.items():
    (sc / name).write_text(body)
    try:
        scan_file(sc / name, name)
    except Unreadable:
        continue
    fail(f"scanner self-check {name}: an unreadable sudo use was accepted")
if not allows(narrow[0], "lodestar", cases["helper_const.py"][1][0]):
    fail("scanner self-check: a helper call doesn't tie to the helper rule")
ok("scanner self-check: comments, constants and prose handled; list, option, embedded and passworded forms refused")

if shutil.which("visudo"):
    r = subprocess.run(["visudo", "-c", "-q", "-f", str(WORK / "sudoers")], capture_output=True, text=True)
    if r.returncode != 0:
        fail(f"visudo rejects the template's sudoers lines: {r.stdout}{r.stderr}")
    ok("visudo accepts the template's sudoers lines")
else:
    print("SKIP: visudo not installed")

# ---- The helper ----------------------------------------------------------------
hp = WORK / "lodestar-vpn-helper"
hp.write_text(helper)
r = subprocess.run(["bash", "-n", str(hp)], capture_output=True, text=True)
if r.returncode:
    fail(f"bash -n on the helper: {r.stderr}")
if shutil.which("shellcheck"):
    r = subprocess.run(["shellcheck", "-S", "warning", str(hp)], capture_output=True, text=True)
    if r.returncode:
        fail(f"shellcheck on the helper:\n{r.stdout}")
    ok("helper passes bash -n and shellcheck")
else:
    ok("helper passes bash -n")
    print("SKIP: shellcheck not installed; no shellcheck pass on the helper")

main_body = helper[helper.index("\nmain() {"):]
verbs = set(re.findall(r"^    ([a-z][a-z0-9-]*)\)$", main_body, re.M))
if verbs != HELPER_VERBS:
    fail(f"helper verbs changed: added {sorted(verbs - HELPER_VERBS)}, removed {sorted(HELPER_VERBS - verbs)}")
if not re.search(r'^    \*\)\n      echo "ERROR: Unauthorized helper command', main_body, re.M):
    fail("the helper's verb switch must refuse anything else")
ok(f"helper's fixed verb list ({len(verbs)} verbs), anything else refused")

def helper_var(name):
    m = re.search(rf'^{name}="([^"]*)"$', helper, re.M)
    if not m:
        fail(f"the helper has no {name}")
    return m.group(1)

cdoc = yaml.safe_load(compose_text)
fdoc = yaml.safe_load(FLEET_COMPOSE.read_text())
g = cdoc["services"]["gluetun"]
digest = helper_var("GLUETUN_DIGEST")
if g["image"] != f"qmcgaw/gluetun@{digest}" or fdoc["services"]["gluetun"]["image"] != g["image"]:
    fail(f"gluetun's digest differs: helper {digest}, compose {g['image']}, fleet {fdoc['services']['gluetun']['image']}")
if cdoc.get("name") != helper_var("COMPOSE_PROJECT") or g.get("container_name") != "gluetun":
    fail("compose.vpn.yaml's project or container name isn't the helper's")
if {"type": "bind", "source": helper_var("CONFIG_FILE"), "target": "/gluetun/wireguard/wg0.conf", "read_only": True,
    "bind": {"create_host_path": False}} not in g["volumes"]:
    fail("compose.vpn.yaml doesn't mount the helper's wg0.conf read-only")
if helper_var("COMPOSE_FILE") not in files or helper_var("PIA_CA_FILE") not in files:
    fail("the helper's compose or CA path isn't where cloud-init writes it")
if not helper_var("CONFIG_FILE").startswith(helper_var("CONFIG_DIR") + "/") or not helper_var("CONFIG_DIR").startswith("/run/"):
    fail("wg0.conf must live in the helper's directory on /run (tmpfs)")
env = g["environment"]
if env.get("VPN_INTERFACE") != "wg0" or env.get("WIREGUARD_IMPLEMENTATION") != "kernelspace":
    fail("gluetun must name wg0 and use kernel WireGuard (the helper reads it with the host's wg)")
if g.get("configs") != fdoc["services"]["gluetun"]["configs"] or cdoc.get("configs") != fdoc.get("configs"):
    fail("compose.vpn.yaml must ship the fleet's post-rules")
if "net.ipv6.conf.all.disable_ipv6=1" not in g.get("sysctls", []) or g.get("ports"):
    fail("gluetun must have IPv6 off and publish no port")
if "crawler" in cdoc["services"]:
    fail("compose.vpn.yaml must not run the crawler (see crawler-service.example.yaml)")
ok("helper, compose.vpn.yaml and the fleet's stack agree (digest, project, paths, post-rules)")

boot = files["/usr/local/sbin/lodestar-bootstrap.sh"]["content"]
for want in ("install -d -o lodestar -g lodestar -m 0700 /home/lodestar/lodestar/secrets",
             "install -d -o root -g root -m 0755 /etc/lodestar /etc/lodestar/vpn",
             "install -d -o root -g root -m 0700 /run/lodestar/vpn"):
    if want not in boot:
        fail(f"bootstrap lacks: {want}")
ok("bootstrap keeps main's secrets directory and adds the helper's root-only directories")
PY

# ------------------------------------------------------------------------------
# 4. The helper's validators under every awk on the host
# ------------------------------------------------------------------------------
HELPER_FILE="$WORK/lodestar-vpn-helper"
AWKS=()
for a in mawk gawk original-awk; do
  if command -v "$a" >/dev/null 2>&1; then AWKS+=("$(command -v "$a")"); fi
done
if command -v busybox >/dev/null 2>&1 && busybox awk 'BEGIN{}' 2>/dev/null; then
  mkdir -p "$WORK/bb"
  printf '#!/bin/sh\nexec busybox awk "$@"\n' > "$WORK/bb/busybox-awk"
  chmod 0755 "$WORK/bb/busybox-awk"
  AWKS+=("$WORK/bb/busybox-awk")
fi
[[ ${#AWKS[@]} -gt 0 ]] || fail "no awk on this host"
if [[ -r /etc/os-release ]] && grep -q '^VERSION_ID="24.04"' /etc/os-release && grep -q '^ID=ubuntu' /etc/os-release; then
  [[ " ${AWKS[*]} " == *"/mawk "* ]] || fail "Ubuntu 24.04 without mawk: its default awk is untested"
fi

key() { python3 -c 'import base64,sys; print(base64.b64encode(bytes([int(sys.argv[1])])*32).decode())' "$1"; }
K1="$(key 1)"; K2="$(key 2)"
wg_conf() {  # endpoint, then extra lines for [Peer]
  printf '[Interface]\nAddress = 10.0.0.2/32\nPrivateKey = %s\nDNS = 10.0.0.242\n\n[Peer]\nPublicKey = %s\nEndpoint = %s\nAllowedIPs = 0.0.0.0/0\nPersistentKeepalive = 25\n%s' \
    "$K1" "$K2" "$1" "${2:-}"
}

# Rules as iptables -S OUTPUT lists them: Lodestar's post-rules (inserted at 1,
# 2 and 3, so first and in order), then gluetun's own.
POST1='-A OUTPUT -o wg0 -j CONNMARK --set-xmark 0x800000/0x800000'
POST2='-A OUTPUT ! -o wg0 -m connmark --mark 0x800000/0x800000 -j DROP'
POST3='-A OUTPUT ! -o wg0 -p icmp -m addrtype ! --dst-type LOCAL -j DROP'
GLUETUN_RULES='-A OUTPUT -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
-A OUTPUT -o lo -j ACCEPT
-A OUTPUT -s 172.18.0.2/32 -d 172.18.0.0/16 -o eth0 -j ACCEPT
-A OUTPUT -d 203.0.113.10/32 -o eth0 -p udp -m udp --dport 1337 -j ACCEPT
-A OUTPUT -o wg0 -j ACCEPT'
GOOD_RULES="-P OUTPUT DROP
$POST1
$POST2
$POST3
$GLUETUN_RULES"

for AWK_BIN in "${AWKS[@]}"; do
  shim="$WORK/shim-$(basename "$AWK_BIN")"
  mkdir -p "$shim"
  ln -sf "$AWK_BIN" "$shim/awk"
  echo "=== Helper validators under $(basename "$AWK_BIN") ==="
  # Each check sources the helper (its dispatch runs only when executed) in a subshell.
  # shellcheck source=/dev/null
  run() { (PATH="$shim:$PATH"; set +e; source "$HELPER_FILE"; "$@") ; }
  v() { local f="$WORK/wg.conf"; cat > "$f"; run validate_wg_config "$f" 2>/dev/null; }

  wg_conf 203.0.113.10:1337 | v || fail "[$AWK_BIN] a valid wg0.conf was refused"
  wg_conf 198.51.100.7:65535 | v || fail "[$AWK_BIN] a valid wg0.conf (port 65535) was refused"
  for bad_ep in 10.1.2.3:1337 192.168.1.1:1337 172.16.0.1:1337 172.31.255.1:1337 127.0.0.1:1337 \
                169.254.1.1:1337 100.64.0.1:1337 0.1.2.3:1337 224.0.0.1:1337 203.0.113.256:1337 \
                203.0.113.10:0 203.0.113.10:65536 203.0.113.10:123456 203.0.113:1337 2001:db8::1:1337 \
                203.0.113.10 1203.0.113.10:1337; do
    if wg_conf "$bad_ep" | v; then fail "[$AWK_BIN] endpoint $bad_ep was accepted"; fi
  done
  for extra in "PostUp = id" "Table = off" "SaveConfig = true" "AllowedIPs = 0.0.0.0/0" "[Peer]" "Junk"; do
    if wg_conf 203.0.113.10:1337 "$extra"$'\n' | v; then fail "[$AWK_BIN] an extra line was accepted: $extra"; fi
  done
  if wg_conf 203.0.113.10:1337 | sed 's/^PublicKey = .*/PublicKey = short=/' | v; then fail "[$AWK_BIN] a short key was accepted"; fi
  if wg_conf 203.0.113.10:1337 | sed 's#^Address = .*#Address = 192.0.2.5/32#' | v; then fail "[$AWK_BIN] a non-10/8 Address was accepted"; fi
  if wg_conf 203.0.113.10:1337 | sed 's#^Address = .*#Address = 10.0.0.300/32#' | v; then fail "[$AWK_BIN] Address octet 300 was accepted"; fi
  if wg_conf 203.0.113.10:1337 | sed 's/^DNS = .*/DNS = 10.0.0.1;id/' | v; then fail "[$AWK_BIN] a metacharacter was accepted"; fi
  if wg_conf 203.0.113.10:1337 | sed 's/^DNS = .*/DNS = 10.0.0.1\r/' | v; then fail "[$AWK_BIN] a carriage return was accepted"; fi
  if wg_conf 203.0.113.10:1337 | sed '1,4d' | v; then fail "[$AWK_BIN] a missing [Interface] was accepted"; fi
  if { wg_conf 203.0.113.10:1337; head -c 5000 /dev/zero | tr '\0' '#'; } | v; then fail "[$AWK_BIN] an oversized payload was accepted"; fi
  pass "[$(basename "$AWK_BIN")] wg0.conf schema: valid configs pass; private, reserved and malformed endpoints, hooks, extra lines, bad keys, control characters and oversize fail"

  for c in "172.18.0.2/16 172.18.0.0/16" "10.99.7.200/24 10.99.7.0/24" "192.0.2.77/32 192.0.2.77/32" \
           "203.0.113.130/25 203.0.113.128/25" "255.255.255.255/1 128.0.0.0/1" "198.51.100.9/0 0.0.0.0/0"; do
    got="$(run cidr_network "${c% *}")" || fail "[$AWK_BIN] cidr_network ${c% *} failed"
    [[ "$got" == "${c#* }" ]] || fail "[$AWK_BIN] cidr_network ${c% *}: got $got, want ${c#* }"
  done
  pass "[$(basename "$AWK_BIN")] cidr_network"

  rc() { run check_output_rules "$1" 203.0.113.10 1337 eth0 172.18.0.2 172.18.0.0/16 2>/dev/null; }
  rc "$GOOD_RULES" || fail "[$AWK_BIN] gluetun's rules with Lodestar's post-rules were refused"
  for bad in '-A OUTPUT -o eth0 -j ACCEPT' '-A OUTPUT -o eth0 -p tcp -j ACCEPT' \
             '-A OUTPUT -s 10.99.0.2/32 -d 10.99.0.0/16 -o eth0 -j ACCEPT' \
             '-A OUTPUT -d 203.0.113.10/32 -o eth0 -p udp -m udp --dport 9999 -j ACCEPT' \
             '-A OUTPUT -d 198.51.100.1/32 -o eth0 -p udp -m udp --dport 1337 -j ACCEPT' \
             '-A OUTPUT -j ACCEPT' '-A OUTPUT -j custom_chain' '-A OUTPUT -o eth0 -j CONNMARK --set-xmark 0x1/0x1' \
             '-A OUTPUT -o eth0 -j RETURN' '-A OUTPUT -o eth0 -m comment --comment "-j DROP " -j ACCEPT' \
             '-A OUTPUT -o eth0 -m comment --comment x -j DROP' '-A OUTPUT -o eth0 -g custom_chain' \
             '-A OUTPUT -o eth0 -j REJECT --reject-with x -j ACCEPT' "$POST1"; do
    if rc "$GOOD_RULES"$'\n'"$bad"; then fail "[$AWK_BIN] an unauthorized rule passed: $bad"; fi
  done
  for bad_set in "-P OUTPUT DROP"$'\n'"$POST2"$'\n'"$POST1"$'\n'"$POST3"$'\n'"$GLUETUN_RULES" \
                 "-P OUTPUT DROP"$'\n'"$POST1"$'\n'"$POST2"$'\n'"$GLUETUN_RULES" \
                 "-P OUTPUT DROP"$'\n'"$GLUETUN_RULES"$'\n'"$POST1"$'\n'"$POST2"$'\n'"$POST3" \
                 "-P OUTPUT DROP"$'\n'"$POST1"$'\n'"$POST2"; do
    if rc "$bad_set"; then fail "[$AWK_BIN] post-rules out of order or missing passed: $(echo "$bad_set" | head -4 | tr '\n' '|')"; fi
  done
  pass "[$(basename "$AWK_BIN")] firewall rule check: the expected set passes; reordered or missing post-rules, broad, wrong-port, wrong-subnet, custom-chain, goto, RETURN and comment-disguised rules fail"
done

if [[ "$MODE" == "offline" ]]; then
  echo "ALL OFFLINE CHECKS PASSED."
  exit 0
fi

# ==============================================================================
# --privileged: #632's integration test (a real user, sudoers and a netns)
# ==============================================================================
IS_CI="${CI:-false}"
[[ "${GITHUB_ACTIONS:-false}" == "true" ]] && IS_CI="true"
[[ "$(uname -s)" == "Linux" ]] || fail "--privileged needs Linux"
if [[ "$IS_CI" != "true" && "${LODESTAR_ALLOW_PRIVILEGED_TESTS:-0}" != "1" ]]; then
  echo "ERROR: --privileged changes the host (a user, sudoers, /usr/local/sbin, /etc/lodestar)." >&2
  echo "Run it only in a disposable container or VM, with LODESTAR_ALLOW_PRIVILEGED_TESTS=1." >&2
  exit 2
fi
[[ $EUID -eq 0 ]] || fail "--privileged must run as root"
for c in useradd userdel su sudo visudo ip iptables; do
  command -v "$c" >/dev/null 2>&1 || fail "--privileged needs $c"
done

TEST_PID="$$"
TEST_USER="test-ls-$TEST_PID"
SUDOERS_FILE="/etc/sudoers.d/99-test-ls-$TEST_PID"
HELPER_BIN="/usr/local/sbin/lodestar-vpn-helper"
COMPOSE_DIR="/etc/lodestar/vpn"
COMPOSE_FILE="$COMPOSE_DIR/compose.yaml"
PIA_CA_FILE="$COMPOSE_DIR/ca.rsa.4096.crt"
RUN_DIR="/run/lodestar/vpn"
TEST_NETNS="ns-ls-$TEST_PID"
BACKUP="$WORK/backup"
mkdir -p "$BACKUP"
STATE_USER=0 STATE_SUDOERS=0 STATE_NETNS=0 STATE_COMPOSE_DIR=0 STATE_RUN_DIR=0 STATE_RUN_PARENT=0 STATE_LOCK=0

for f in "$HELPER_BIN" "$COMPOSE_FILE" "$PIA_CA_FILE"; do
  if [[ -e "$f" || -L "$f" ]]; then cp -a "$f" "$BACKUP/$(basename "$f")"; fi
done
[[ -d "$COMPOSE_DIR" ]] && STATE_COMPOSE_DIR=1
[[ -e "$RUN_DIR" ]] && STATE_RUN_DIR=1
[[ -e "$(dirname "$RUN_DIR")" ]] && STATE_RUN_PARENT=1
[[ -e /run/lodestar-pia.lock ]] && STATE_LOCK=1

restore() {
  local rc=$?
  set +e
  [[ "$STATE_NETNS" -eq 1 ]] && ip netns del "$TEST_NETNS" 2>/dev/null
  [[ "$STATE_SUDOERS" -eq 1 ]] && rm -f "$SUDOERS_FILE"
  [[ "$STATE_USER" -eq 1 ]] && userdel -r -f "$TEST_USER" 2>/dev/null
  for f in "$HELPER_BIN" "$COMPOSE_FILE" "$PIA_CA_FILE"; do
    if [[ -e "$BACKUP/$(basename "$f")" || -L "$BACKUP/$(basename "$f")" ]]; then
      rm -f "$f"; cp -a "$BACKUP/$(basename "$f")" "$f"
    else
      rm -f "$f"
    fi
  done
  [[ "$STATE_COMPOSE_DIR" -eq 0 ]] && rmdir "$COMPOSE_DIR" /etc/lodestar 2>/dev/null
  [[ "$STATE_RUN_DIR" -eq 0 ]] && rm -rf -- "$RUN_DIR"
  [[ "$STATE_RUN_PARENT" -eq 0 ]] && rmdir "$(dirname "$RUN_DIR")" 2>/dev/null
  [[ "$STATE_LOCK" -eq 0 ]] && rm -f /run/lodestar-pia.lock
  rm -rf -- "$WORK"
  if [[ $rc -eq 0 ]]; then echo "ALL LINUX PRIVILEGE BOUNDARY TESTS PASSED."; else echo "LINUX PRIVILEGE BOUNDARY TEST FAILED (exit $rc)" >&2; fi
  exit "$rc"
}
trap restore EXIT

id -u "$TEST_USER" >/dev/null 2>&1 && fail "test user $TEST_USER already exists"

echo "=== Privileged: install the rendered helper, compose.yaml and CA ==="
install -d -o root -g root -m 0755 /etc/lodestar "$COMPOSE_DIR"
install -o root -g root -m 0644 "$TF_DIR/pia-example/compose.vpn.yaml" "$COMPOSE_FILE"
install -o root -g root -m 0644 "$REPO_ROOT/infra/pia-bridge/vm/pia-ca.rsa.4096.crt" "$PIA_CA_FILE"
install -o root -g root -m 0750 "$HELPER_FILE" "$HELPER_BIN"

useradd -m -s /bin/bash "$TEST_USER"
STATE_USER=1
printf '%s ALL=(root) NOPASSWD: %s\n' "$TEST_USER" "$HELPER_BIN" > "$SUDOERS_FILE"
chmod 0440 "$SUDOERS_FILE"
STATE_SUDOERS=1
visudo -cf "$SUDOERS_FILE" >/dev/null
as_user() { su - "$TEST_USER" -c "$1"; }

echo "=== Test 1: the helper rule alone gives no root shell, no other command ==="
for c in "sudo -n ls /root" "sudo -n docker ps" "sudo -n bash -c id" "sudo -n $HELPER_BIN preflight extra"; do
  if as_user "$c" >/dev/null 2>&1; then fail "the test user ran: $c"; fi
done
pass "arbitrary sudo refused; the helper takes exactly one verb"

echo "=== Test 2: unknown verbs are refused ==="
for verb in install-compose arbitrary-exec query-dns-vpn "start-vpn;id"; do
  out="$(as_user "sudo -n $HELPER_BIN '$verb'" 2>&1 || true)"
  grep -q "ERROR: Unauthorized helper command" <<<"$out" || fail "verb $verb: $out"
done
pass "unknown verbs refused"

echo "=== Test 3: the test user can't change the root-owned stack ==="
for c in "echo x >> $COMPOSE_FILE" "rm -f $COMPOSE_FILE" "ln -sf /tmp/x $COMPOSE_FILE" "cat $HELPER_BIN" "touch $COMPOSE_DIR/x"; do
  if as_user "$c" >/dev/null 2>&1; then fail "the test user could: $c"; fi
done
pass "compose.yaml, its directory and the helper are out of the test user's reach"

echo "=== Test 4: validate-compose, and its symlink refusal ==="
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
  out="$(as_user "sudo -n $HELPER_BIN validate-compose" 2>&1 || true)"
  grep -q COMPOSE_VALID <<<"$out" || fail "validate-compose: $out"
  pass "validate-compose accepts compose.vpn.yaml"
else
  echo "SKIP: docker compose not installed"
fi
mv "$COMPOSE_FILE" "$COMPOSE_FILE.real"; ln -s "$COMPOSE_FILE.real" "$COMPOSE_FILE"
out="$(as_user "sudo -n $HELPER_BIN validate-compose" 2>&1 || true)"
rm -f "$COMPOSE_FILE"; mv "$COMPOSE_FILE.real" "$COMPOSE_FILE"
grep -q "ERROR: $COMPOSE_FILE missing or symlink" <<<"$out" || fail "symlinked compose.yaml: $out"
pass "validate-compose refuses a symlink"

echo "=== Test 5: the crawler gate fails closed with no wg0.conf or gluetun ==="
for verb in start-crawler verify-tunnel; do
  out="$(as_user "sudo -n $HELPER_BIN $verb" 2>&1 || true)"
  grep -q -E "(wg0.conf missing or symlink|Gluetun health check failed)" <<<"$out" || fail "$verb: $out"
done
pass "start-crawler and verify-tunnel fail closed"

echo "=== Test 6: write-config against a stubbed PIA, the CA checks, and a namespace firewall ==="
MOCK="$WORK/mock"; mkdir -p "$MOCK"
cat > "$MOCK/curl" <<'MOCK_CURL'
#!/usr/bin/env bash
case "$*" in
  *serverlist.piaservers.net*) echo '{"groups":{"wg":[{"ports":[1337]}]},"regions":[{"servers":{"wg":[{"ip":"203.0.113.10","cn":"test-server"}]}}]}' ;;
  *test-server:1337*) printf 400 ;;
  *) printf 000; exit 60 ;;
esac
MOCK_CURL
cat > "$MOCK/docker" <<MOCK_DOCKER
#!/usr/bin/env bash
if [[ "\$1" == exec && "\$2" == gluetun ]]; then
  shift 2
  # A kernel built without IPv6 has no switch to read: IPv6 is off there.
  if [[ "\$*" == "cat /proc/sys/net/ipv6/conf/all/disable_ipv6" && ! -e /proc/sys/net/ipv6 ]]; then echo 1; exit 0; fi
  exec ip netns exec "$TEST_NETNS" "\$@"
fi
exit 1
MOCK_DOCKER
chmod 0755 "$MOCK/curl" "$MOCK/docker"
wg_conf 203.0.113.10:1337 | PATH="$MOCK:$PATH" "$HELPER_BIN" write-config | grep -q CONFIG_OK || fail "write-config refused a valid config"
[[ "$(stat -c '%a %U' "$RUN_DIR" "$RUN_DIR/wg0.conf" | tr '\n' ' ')" == "700 root 600 root " ]] || fail "wg0.conf modes: $(stat -c '%n %a %U' "$RUN_DIR" "$RUN_DIR/wg0.conf")"
if wg_conf 198.51.100.7:1337 | PATH="$MOCK:$PATH" "$HELPER_BIN" write-config >/dev/null 2>&1; then fail "an endpoint not in the server list was written"; fi
mv "$PIA_CA_FILE" "$PIA_CA_FILE.bak"
if wg_conf 203.0.113.10:1337 | PATH="$MOCK:$PATH" "$HELPER_BIN" write-config >/dev/null 2>&1; then fail "write-config passed with no CA"; fi
ln -s "$PIA_CA_FILE.bak" "$PIA_CA_FILE"
if wg_conf 203.0.113.10:1337 | PATH="$MOCK:$PATH" "$HELPER_BIN" write-config >/dev/null 2>&1; then fail "write-config passed with a symlinked CA"; fi
rm -f "$PIA_CA_FILE"; mv "$PIA_CA_FILE.bak" "$PIA_CA_FILE"
cp -a "$RUN_DIR/wg0.conf" "$WORK/wg0.conf.keep"
rm -f "$RUN_DIR/wg0.conf"
mkdir "$RUN_DIR/wg0.conf"  # what Docker's short bind syntax leaves after a reboot
out="$(wg_conf 203.0.113.10:1337 | PATH="$MOCK:$PATH" "$HELPER_BIN" write-config 2>&1 || true)"
rmdir "$RUN_DIR/wg0.conf"
cp -a "$WORK/wg0.conf.keep" "$RUN_DIR/wg0.conf"
grep -q "exists and is not a file; run cleanup first" <<<"$out" || fail "write-config over a directory: $out"
pass "write-config: a valid config is written root-only; an unlisted endpoint, a missing or symlinked CA, or a directory in its place fail"

ip netns add "$TEST_NETNS"; STATE_NETNS=1
ip -n "$TEST_NETNS" link set lo up
# veth pairs with the far ends left down on the host; deleting the namespace
# deletes both pairs. The wg0 here is not WireGuard, on purpose.
ip link add "e$TEST_PID" type veth peer name "ep$TEST_PID"; ip link set "e$TEST_PID" netns "$TEST_NETNS" name eth0
ip link add "w$TEST_PID" type veth peer name "wp$TEST_PID"; ip link set "w$TEST_PID" netns "$TEST_NETNS" name wg0
ip -n "$TEST_NETNS" link set eth0 up; ip -n "$TEST_NETNS" addr add 172.18.0.2/16 dev eth0
ip -n "$TEST_NETNS" link set wg0 up; ip -n "$TEST_NETNS" addr add 10.0.0.2/32 dev wg0
ip -n "$TEST_NETNS" route add default dev wg0
nsx() { ip netns exec "$TEST_NETNS" "$@"; }
if [[ -e /proc/sys/net/ipv6 ]]; then nsx sysctl -q -w net.ipv6.conf.all.disable_ipv6=1; fi
nsx iptables -P OUTPUT DROP
nsx iptables -A OUTPUT -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
nsx iptables -A OUTPUT -o lo -j ACCEPT
nsx iptables -A OUTPUT -s 172.18.0.2/32 -d 172.18.0.0/16 -o eth0 -j ACCEPT
nsx iptables -A OUTPUT -d 203.0.113.10/32 -o eth0 -p udp -m udp --dport 1337 -j ACCEPT
nsx iptables -A OUTPUT -o wg0 -j ACCEPT
nsx iptables -I OUTPUT 1 -o wg0 -j CONNMARK --set-xmark 0x800000/0x800000
nsx iptables -I OUTPUT 2 ! -o wg0 -m connmark --mark 0x800000/0x800000 -j DROP
nsx iptables -I OUTPUT 3 ! -o wg0 -p icmp -m addrtype ! --dst-type LOCAL -j DROP
out="$(PATH="$MOCK:$PATH" "$HELPER_BIN" verify-dns-firewall 2>&1 || true)"
grep -q DNS_FIREWALL_OK <<<"$out" || fail "verify-dns-firewall refused the expected rules: $out"
nsx iptables -D OUTPUT 2
out="$(PATH="$MOCK:$PATH" "$HELPER_BIN" verify-dns-firewall 2>&1 || true)"
grep -q "post-rules" <<<"$out" || fail "verify-dns-firewall passed without the post-rules' DROP: $out"
nsx iptables -I OUTPUT 2 ! -o wg0 -m connmark --mark 0x800000/0x800000 -j DROP
nsx iptables -A OUTPUT -o eth0 -j ACCEPT
out="$(PATH="$MOCK:$PATH" "$HELPER_BIN" verify-dns-firewall 2>&1 || true)"
grep -q "Unauthorized egress rule in Gluetun firewall: -A OUTPUT -o eth0 -j ACCEPT" <<<"$out" || fail "a broad eth0 accept passed: $out"
nsx iptables -D OUTPUT -o eth0 -j ACCEPT
pass "verify-dns-firewall on a real namespace: expected rules pass; a missing post-rule or a broad accept fails"

for verb in wg-handshake wg-transfer wg-endpoints; do
  if PATH="$MOCK:$PATH" "$HELPER_BIN" "$verb" >/dev/null 2>&1; then fail "$verb answered for a wg0 that isn't WireGuard"; fi
done
pass "a wg0 that isn't WireGuard can't fake handshake, transfer or endpoint evidence"

if PATH="$MOCK:$PATH" "$HELPER_BIN" cleanup >/dev/null 2>&1; then fail "cleanup passed although compose down failed"; fi
[[ -f "$RUN_DIR/wg0.conf" ]] || fail "cleanup deleted wg0.conf although the containers weren't verifiably down"
pass "cleanup fails closed and keeps wg0.conf when shutdown can't be verified"
