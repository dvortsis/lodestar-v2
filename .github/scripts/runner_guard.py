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
  a literal matrix (`exclude:`, which can only remove entries, is the one
  part of a matrix that may be an expression);
- a module that would shadow the standard library (or PyYAML) sits where
  the changes job's Python, or this guard's, would import it first;
- a job calls a reusable workflow from another repository (its jobs pick
  their own runners, out of sight of this check);
- bench-10m.yml itself could run code from a pull request or a fork, or
  holds more than it needs (see check_bench).

Known limit: under pull_request a PR controls its own workflows and its own
copy of this guard, so the guard is a tripwire, not a wall.

It is a detector, not a barrier: a pushed branch runs its workflows at the
same time as this check (OQ1). The barriers are the isolated, ephemeral
machine, the release-line ruleset and the fork-PR approval setting
(docs/operations/bench-runner.md).

Usage: python3 .github/scripts/runner_guard.py [ROOT]
Exit status 0 when clean, 1 with one line per problem.
"""

import importlib.machinery
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
    strategy = job.get("strategy") or {}
    if not isinstance(strategy, dict):
        return [f"runs-on {value!r}: the strategy is not a mapping ({strategy!r})"]
    matrix = strategy.get("matrix")
    if isinstance(matrix, str):
        return [f"runs-on {value!r}: the matrix is an expression ({matrix})"]
    if not isinstance(matrix, dict):
        return [f"runs-on {value!r}: the job has no matrix"]
    # Expression contexts ignore case, so `OS:` could feed `matrix.os`: match
    # keys case-insensitively, and refuse keys that differ only in case.
    folded = [str(k).casefold() for k in matrix]
    if len(folded) != len(set(folded)):
        return [f"runs-on {value!r}: matrix keys differ only in case"]
    key = key.casefold()
    raw = next((v for k, v in matrix.items() if str(k).casefold() == key), None)
    if any(str(k).casefold() in ("include", "exclude") and k not in ("include", "exclude") for k in matrix):
        return [f"runs-on {value!r}: matrix include/exclude must be spelled in lower case"]
    candidates = []
    # `exclude` may be driven by expressions (ci.yml's test job drops the
    # operating systems a change doesn't need that way): it can only remove
    # entries, so it's never read here. Everything that adds one is.
    if isinstance(raw, str) and "${{" in raw:
        return [f"runs-on {value!r}: matrix.{key} is an expression ({raw.strip()})"]
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
        hits = [v for k, v in entry.items() if str(k).casefold() == key]
        if len(hits) > 1:
            return [f"runs-on {value!r}: a matrix.include entry has keys that differ only in case"]
        candidates.extend(hits)
    if not candidates:
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


# Folders a CI Python process puts first on its import path: the repository
# root (python3 -m unittest discover), the script folder (ci_changes.py, this
# guard) and the import smoke tests' folder (their discover -s).
SHADOW_DIRS = (".", ".github/scripts", "docs/operations/imports/pilots/smoke")
SHADOW_NAMES = frozenset(sys.stdlib_module_names) | {"yaml", "_yaml"}
IMPORT_SUFFIXES = tuple(importlib.machinery.all_suffixes()) + (".pyw", ".pyd")


def shadow_problems(root):
    """A module named like the standard library's (or PyYAML) in a folder
    that's first on a CI import path loads in place of the real one."""
    out = []
    for d in SHADOW_DIRS:
        folder = Path(root) / d
        if not folder.is_dir():
            continue
        for entry in sorted(folder.iterdir()):
            top = entry.name.split(".")[0]
            if top not in SHADOW_NAMES:
                continue
            module = entry.is_file() and entry.name.endswith(IMPORT_SUFFIXES)
            package = entry.is_dir() and (entry / "__init__.py").exists()
            if module or package:
                rel = entry.relative_to(root).as_posix()
                out.append(f"{rel}: shadows the module {top!r} for CI's Python")
    return out


def check(root):
    root = Path(root)
    out = shadow_problems(root)
    wf_dir = root / ".github" / "workflows"
    names = sorted(p.name for p in wf_dir.iterdir() if p.is_file() and p.suffix.lower() in {".yml", ".yaml"})
    if BENCH not in names:
        out.append(f"{BENCH} is missing")
    for name in names:
        out.extend(check_workflow(name, (wf_dir / name).read_text(encoding="utf-8")))
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
            out.append(f"{rel}: isn't UTF-8 text, so it can't be checked for the label")
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
