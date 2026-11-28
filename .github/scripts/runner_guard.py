#!/usr/bin/env python3
"""runner-guard: only bench-10m.yml may use the dedicated benchmark runner.

M8-PERF-3, mitigation (c) of plan 2026-11-06-m8-hardening §5 OQ1 (owner,
docs/owner-decisions.md §2.47). The self-hosted runner carries one label,
LABEL. On a user-owned public repository GitHub can't restrict a runner to
one workflow, so any workflow that names the label can run on it. This
check fails when:

- a workflow other than bench-10m.yml names the label anywhere (any case,
  any form: a string, a list, a `group:`/`labels:` mapping, a matrix value,
  an expression, a comment), or a file under .github/ other than the guard
  itself does;
- a job in another workflow could reach a self-hosted runner: `runs-on`
  names `self-hosted`, uses a `group:`/`labels:` mapping, names anything but
  a GitHub-hosted image, or is an expression that isn't `matrix.<key>` over
  a literal matrix. The one dynamic matrix allowed is DYNAMIC_OK, ci.yml's
  test job; check_dynamic_chain pins everything that feeds it (the changes
  job's output and step, and how ci_changes.py sets test_os);
- a job calls a reusable workflow from another repository (its jobs pick
  their own runners, out of sight of this check);
- bench-10m.yml itself could run code from a pull request or a fork, or
  holds more than it needs (see check_bench).

Known limit: the chain check reads ci_changes.py's syntax, not what it does
at run time, so any diff to its test_os code, or to ci.yml's changes or test
job, is a security-review item (docs/decisions/2026-11-28-M8-PERF-3.md).

It is a detector, not a barrier: a pushed branch runs its workflows at the
same time as this check (OQ1). The barriers are the isolated, ephemeral
machine, the release-line ruleset and the fork-PR approval setting
(docs/operations/bench-runner.md).

Usage: python3 .github/scripts/runner_guard.py [ROOT]
Exit status 0 when clean, 1 with one line per problem.
"""

import ast
import re
import sys
from pathlib import Path

import yaml

LABEL = "lodestar-bench-10m"
BENCH = "bench-10m.yml"
# Files that must name the label: the bench workflow, the guard and its test.
NAMES_LABEL = {
    ".github/workflows/" + BENCH,
    ".github/scripts/runner_guard.py",
    ".github/scripts/test_runner_guard.py",
}
# GitHub-hosted images, by name. A self-hosted runner registered with
# --no-default-labels answers to none of these.
HOSTED = re.compile(r"^(ubuntu|windows|macos)-(latest|\d+(\.\d+)?(-arm)?|\d+-(large|xlarge|arm64))$")
# The one runs-on allowed to come from a job output, as (workflow, job):
# ci.yml's test matrix is ci_changes.py's test_os, which names hosted images
# only (it's scanned for the label like every file under .github/).
DYNAMIC_OK = {("ci.yml", "test", "os"): "${{ fromJSON(needs.changes.outputs.test_os) }}"}
# What feeds it, pinned exactly (check_dynamic_chain).
CHAIN_OUTPUT = "${{ steps.c.outputs.test_os }}"
CHAIN_RUN = 'python3 .github/scripts/ci_changes.py "$EVENT" "$BASE" "$HEAD" "$LABELS" | tee -a "$GITHUB_OUTPUT"'
CHAIN_SCRIPT = ".github/scripts/ci_changes.py"
# Anything else in the changes job that mentions one of these could rewrite
# the output or the script before step c runs.
CHAIN_WORDS = re.compile(r"test_os|GITHUB_OUTPUT|ci_changes", re.I)
MATRIX_REF = re.compile(r"^\$\{\{\s*matrix\.([\w-]+)\s*\}\}$")

BENCH_TRIGGERS = {"push", "schedule", "workflow_dispatch"}
BENCH_PUSH_BRANCHES = ["release/v[0-9]+.[0-9]+", "release/v[0-9]+.[0-9]+.[0-9]+"]
BENCH_INPUTS = {"smoke_only"}
# A release line: release/v1.0 or release/v0.5.1, never release/v052-rc1.
_REL = ("(startsWith(github.ref, 'refs/heads/release/v') && contains(github.ref_name, '.') "
        "&& !contains(github.ref_name, '-'))")
REF_IF = (
    "(github.event_name == 'schedule' && github.ref == 'refs/heads/main') || "
    f"(github.event_name == 'push' && {_REL}) || "
    f"(github.event_name == 'workflow_dispatch' && (github.ref == 'refs/heads/main' || {_REL}))"
)
PINNED = re.compile(r"^[\w.-]+/[\w.-]+(/[\w./-]+)?@[0-9a-f]{40}$")


def filter_matches(pattern, branch):
    """GitHub's workflow branch-filter syntax: `*` (no `/`), `**`, `?` and `+`
    (zero-or-one and one-or-more of the previous character) and `[...]`."""
    out, i = [], 0
    while i < len(pattern):
        c = pattern[i]
        if pattern.startswith("**", i):
            out.append(".*")
            i += 2
            continue
        if c == "*":
            out.append("[^/]*")
        elif c in "?+":
            out.append(c)
        elif c == "[":
            j = pattern.index("]", i)
            out.append(pattern[i:j + 1])
            i = j + 1
            continue
        else:
            out.append(re.escape(c))
        i += 1
    return re.fullmatch("".join(out), branch) is not None


def triggers(wf):
    """The workflow's `on:` as {event: config}. PyYAML reads `on` as True."""
    on = wf.get("on", wf.get(True))
    if isinstance(on, str):
        return {on: None}
    if isinstance(on, list):
        return {e: None for e in on}
    if isinstance(on, dict):
        return on
    return {}


def runs_on_problems(name, job_id, job):
    """Problems with one job's runs-on, in a workflow other than the bench's."""
    where = f"{name}: job {job_id}"
    if "uses" in job:
        uses = str(job["uses"])
        if not uses.startswith("./"):
            return [f"{where} calls a reusable workflow from elsewhere ({uses}); its runner can't be checked"]
        return []
    if "runs-on" not in job:
        return [f"{where} has no runs-on"]
    ro = job["runs-on"]
    if isinstance(ro, dict):
        return [f"{where} uses a runs-on group/labels mapping; only GitHub-hosted images are allowed"]
    values = ro if isinstance(ro, list) else [ro]
    out = []
    for v in values:
        if not isinstance(v, str):
            out.append(f"{where}: runs-on value {v!r} is not a string")
            continue
        out.extend(f"{where}: {p}" for p in label_value_problems(name, job_id, job, v.strip()))
    return out


def label_value_problems(name, job_id, job, value):
    if value.lower() == "self-hosted":
        return ["runs-on names self-hosted"]
    if "${{" not in value:
        return [] if HOSTED.match(value) else [f"runs-on {value!r} is not a GitHub-hosted image"]
    m = MATRIX_REF.match(value)
    if not m:
        return [f"runs-on {value!r} is an expression other than matrix.<key>"]
    key = m.group(1)
    matrix = (job.get("strategy") or {}).get("matrix")
    if isinstance(matrix, str):
        return [f"runs-on {value!r}: the matrix is an expression ({matrix})"]
    if not isinstance(matrix, dict):
        return [f"runs-on {value!r}: the job has no matrix"]
    raw = matrix.get(key)
    dynamic = False
    candidates = []
    if isinstance(raw, str) and "${{" in raw:
        if DYNAMIC_OK.get((name, job_id, key)) != raw.strip():
            return [f"runs-on {value!r}: matrix.{key} is an expression ({raw.strip()})"]
        dynamic = True  # written by ci_changes.py, which is scanned for the label
    elif isinstance(raw, list):
        candidates.extend(raw)
    elif raw is not None:
        return [f"runs-on {value!r}: matrix.{key} is not a list ({raw!r})"]
    include = matrix.get("include")
    if include is not None and not isinstance(include, list):
        return [f"runs-on {value!r}: matrix.include is not a list ({include!r})"]
    for entry in include or []:
        if not isinstance(entry, dict):
            return [f"runs-on {value!r}: a matrix.include entry is not a mapping ({entry!r})"]
        if key in entry:
            candidates.append(entry[key])
    if not candidates and not dynamic:
        return [f"runs-on {value!r}: matrix.{key} has no values"]
    out = []
    for c in candidates:
        for v in c if isinstance(c, list) else [c]:
            if not isinstance(v, str):
                out.append(f"runs-on {value!r}: matrix value {v!r} is not a string")
            elif v.strip().lower() == "self-hosted":
                out.append(f"runs-on {value!r}: matrix value names self-hosted")
            elif not HOSTED.match(v.strip()):
                out.append(f"runs-on {value!r}: matrix value {v!r} is not a GitHub-hosted image")
    return out


def walk_strings(node):
    """Every string in a parsed YAML tree, keys included."""
    if isinstance(node, dict):
        for k, v in node.items():
            yield from walk_strings(k)
            yield from walk_strings(v)
    elif isinstance(node, list):
        for v in node:
            yield from walk_strings(v)
    elif isinstance(node, str):
        yield node


def check_bench(wf, text):
    """bench-10m.yml: trusted refs only, nothing to steal, nothing unpinned."""
    name = BENCH
    out = []
    on = triggers(wf)
    for event in on:
        if event not in BENCH_TRIGGERS:
            out.append(f"{name}: trigger {event!r} is not allowed (only {', '.join(sorted(BENCH_TRIGGERS))})")
    if "push" in on:
        push = on["push"] or {}
        if not isinstance(push, dict) or set(push) != {"branches"} or push.get("branches") != BENCH_PUSH_BRANCHES:
            out.append(f"{name}: push must be exactly branches: {BENCH_PUSH_BRANCHES}")
    if "workflow_dispatch" in on:
        wd = on["workflow_dispatch"] or {}
        inputs = (wd.get("inputs") or {}) if isinstance(wd, dict) else {}
        extra = set(inputs) - BENCH_INPUTS
        if extra:
            out.append(f"{name}: workflow_dispatch inputs {sorted(extra)} are not allowed")
        for key, spec in inputs.items():
            if key in BENCH_INPUTS and (not isinstance(spec, dict) or spec.get("type") != "boolean"):
                out.append(f"{name}: input {key} must be a boolean")
    if wf.get("permissions") != {}:
        out.append(f"{name}: top-level permissions must be {{}}")
    if "env" in wf:
        out.append(f"{name}: no top-level env (each job states its own)")
    for s in walk_strings(wf):
        for expr in re.findall(r"\$\{\{(.*?)\}\}", s, re.S):
            if re.search(r"\b(secrets|vars)\b|\bgithub\s*(\.\s*token\b|\[\s*['\"]token['\"]\s*\])"
                         r"|\btoJSON\s*\(\s*github\s*\)", expr, re.I):
                out.append(f"{name}: an expression reads secrets, vars or the token ({expr.strip()[:60]!r})")
    if re.search(r"secrets\s*:\s*inherit", text):
        out.append(f"{name}: secrets: inherit")
    jobs = wf.get("jobs") or {}
    if not jobs:
        out.append(f"{name}: no jobs")
    for job_id, job in jobs.items():
        where = f"{name}: job {job_id}"
        if not isinstance(job, dict):
            out.append(f"{where} is not a mapping")
            continue
        if "uses" in job:
            out.append(f"{where} calls a reusable workflow")
            continue
        ro = job.get("runs-on")
        if ro != LABEL and not (isinstance(ro, str) and HOSTED.match(ro)):
            out.append(f"{where}: runs-on must be exactly {LABEL!r} or a GitHub-hosted image, not {ro!r}")
        if ro == LABEL and str(job.get("if", "")).strip() != REF_IF:
            out.append(f"{where}: needs if: {REF_IF}")
        perms = job.get("permissions")
        if perms != {"contents": "read"}:
            out.append(f"{where}: permissions must be exactly contents: read, not {perms!r}")
        if "environment" in job or "services" in job or "container" in job:
            out.append(f"{where}: no environment, services or container")
        for step in job.get("steps") or []:
            if isinstance(step, dict) and "${{" in str(step.get("run", "")):
                out.append(f"{where}: an expression inside run: (pass it through env:)")
            uses = step.get("uses") if isinstance(step, dict) else None
            if uses is None:
                continue
            if not PINNED.match(str(uses)):
                out.append(f"{where}: {uses} is not pinned to a commit SHA")
            if str(uses).startswith("actions/checkout@"):
                if (step.get("with") or {}).get("persist-credentials") is not False:
                    out.append(f"{where}: actions/checkout needs persist-credentials: false")
    return out


def check_workflow(name, text):
    """Problems in one workflow file, given its name (no folder) and text."""
    try:
        wf = yaml.safe_load(text)
    except yaml.YAMLError as e:
        return [f"{name}: does not parse ({str(e).splitlines()[0]})"]
    if not isinstance(wf, dict):
        return [f"{name}: is not a workflow mapping"]
    if name == BENCH:
        return check_bench(wf, text)
    out = []
    # The raw text catches comments; the parsed strings catch YAML escapes
    # ("lodestar\x2dbench-10m") that the raw text hides.
    if LABEL in text.lower() or any(LABEL in v.lower() for v in walk_strings(wf)):
        out.append(f"{name}: names the bench runner's label {LABEL!r}; only {BENCH} may")
    jobs = wf.get("jobs") or {}
    if not isinstance(jobs, dict):
        return out + [f"{name}: jobs is not a mapping"]
    for job_id, job in jobs.items():
        if isinstance(job, dict):
            out.extend(runs_on_problems(name, job_id, job))
    return out


def check_chain_script(source):
    """ci_changes.py may set test_os only as `X["test_os"] = json.dumps(L)`,
    L a literal list of hosted images or a conditional between two."""
    try:
        tree = ast.parse(source)
    except SyntaxError as e:
        return [f"{CHAIN_SCRIPT}: does not parse ({e.msg})"]
    out, allowed = [], set()

    def literal_images(node):
        if isinstance(node, ast.IfExp):
            return literal_images(node.body) and literal_images(node.orelse)
        return isinstance(node, ast.List) and all(
            isinstance(e, ast.Constant) and isinstance(e.value, str) and HOSTED.match(e.value) for e in node.elts)

    for node in ast.walk(tree):
        if isinstance(node, ast.Assign):
            for t in node.targets:
                if (isinstance(t, ast.Subscript) and isinstance(t.slice, ast.Constant)
                        and t.slice.value == "test_os"):
                    allowed.add(id(t.slice))
                    v = node.value
                    ok = (len(node.targets) == 1 and isinstance(v, ast.Call) and not v.keywords
                          and len(v.args) == 1 and isinstance(v.func, ast.Attribute) and v.func.attr == "dumps"
                          and isinstance(v.func.value, ast.Name) and v.func.value.id == "json"
                          and literal_images(v.args[0]))
                    if not ok:
                        out.append(f"{CHAIN_SCRIPT}:{node.lineno}: test_os must be json.dumps of a literal list "
                                   "of GitHub-hosted images (or a conditional between two)")
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and "test_os" in node.value.lower():
            if id(node) not in allowed:
                out.append(f"{CHAIN_SCRIPT}:{node.lineno}: test_os named outside a plain assignment")
        elif isinstance(node, ast.keyword) and node.arg and "test_os" in node.arg.lower():
            out.append(f"{CHAIN_SCRIPT}:{node.value.lineno}: test_os set as a keyword")
        elif isinstance(node, ast.Name) and "test_os" in node.id.lower():
            out.append(f"{CHAIN_SCRIPT}:{node.lineno}: a name containing test_os")
    if not allowed:
        out.append(f"{CHAIN_SCRIPT}: test_os is never set")
    return out


def check_dynamic_chain(ci_text, script_source):
    """The changes job's test_os output, its step c and ci_changes.py: the
    only route by which a runs-on in another workflow is computed."""
    name = "ci.yml"
    try:
        wf = yaml.safe_load(ci_text)
    except yaml.YAMLError:
        return []  # check_workflow reports it
    jobs = (wf or {}).get("jobs") or {}
    test = jobs.get("test") or {}
    matrix_os = ((test.get("strategy") or {}).get("matrix") or {}).get("os")
    if not (isinstance(matrix_os, str) and "${{" in matrix_os):
        return []  # no dynamic matrix: nothing to pin
    out = []
    changes = jobs.get("changes")
    if not isinstance(changes, dict):
        return [f"{name}: the test job's matrix comes from a changes job that isn't there"]
    if "changes" not in (test.get("needs") if isinstance(test.get("needs"), list) else [test.get("needs")]):
        out.append(f"{name}: job test must need changes")
    outputs = changes.get("outputs") or {}
    if str(outputs.get("test_os", "")).strip() != CHAIN_OUTPUT:
        out.append(f"{name}: job changes: output test_os must be exactly {CHAIN_OUTPUT}")
    for k, v in outputs.items():
        if k != "test_os" and "test_os" in str(v).lower():
            out.append(f"{name}: job changes: output {k} mentions test_os")
    steps = [st for st in changes.get("steps") or [] if isinstance(st, dict)]
    cs = [st for st in steps if st.get("id") == "c"]
    if len(cs) != 1:
        out.append(f"{name}: job changes needs exactly one step with id c")
    for st in steps:
        if st.get("id") == "c":
            if str(st.get("run", "")).strip() != CHAIN_RUN:
                out.append(f"{name}: job changes: step c must run exactly: {CHAIN_RUN}")
            if st.get("shell", "bash") != "bash" or "uses" in st:
                out.append(f"{name}: job changes: step c must be a bash run step")
            continue
        body = " ".join(walk_strings(st))
        if CHAIN_WORDS.search(body):
            out.append(f"{name}: job changes: a step other than c mentions test_os, GITHUB_OUTPUT or ci_changes")
    return out + check_chain_script(script_source)


def check(root):
    root = Path(root)
    out = []
    wf_dir = root / ".github" / "workflows"
    names = sorted(p.name for p in wf_dir.iterdir() if p.is_file() and p.suffix.lower() in {".yml", ".yaml"})
    if BENCH not in names:
        out.append(f"{BENCH} is missing")
    for name in names:
        out.extend(check_workflow(name, (wf_dir / name).read_text(encoding="utf-8")))
    if "ci.yml" in names:
        script = root / CHAIN_SCRIPT
        source = script.read_text(encoding="utf-8") if script.is_file() else ""
        out.extend(check_dynamic_chain((wf_dir / "ci.yml").read_text(encoding="utf-8"), source))
    # The rest of .github/ (scripts, actions, fixtures, any other file in
    # workflows/) mustn't name it either: a script that prints a matrix could
    # feed it to a runs-on.
    for path in sorted((root / ".github").rglob("*")):
        rel = path.relative_to(root).as_posix()
        if (not path.is_file() or rel in NAMES_LABEL or "__pycache__" in path.parts
                or (path.parent == wf_dir and path.name in names)):
            continue
        try:
            body = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        if LABEL in body.lower():
            out.append(f"{rel}: names the bench runner's label {LABEL!r}")
    return out


def main(argv):
    root = argv[1] if len(argv) > 1 else Path(__file__).resolve().parents[2]
    problems = check(root)
    for p in problems:
        print(f"runner-guard: {p}")
    if problems:
        print(f"runner-guard: {len(problems)} problem(s). See docs/operations/bench-runner.md.")
        return 1
    print("runner-guard: only bench-10m.yml can reach the bench runner.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
