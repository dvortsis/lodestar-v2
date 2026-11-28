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
  a literal matrix (the one dynamic matrix allowed is DYNAMIC_OK);
- a job calls a reusable workflow from another repository (its jobs pick
  their own runners, out of sight of this check);
- bench-10m.yml itself could run code from a pull request or a fork, or
  holds more than it needs (see check_bench).

It is a detector, not a barrier: a pushed branch runs its workflows at the
same time as this check (OQ1). The barriers are the isolated, ephemeral
machine, the release/* ruleset and the fork-PR approval setting
(docs/operations/bench-runner.md).

Usage: python3 .github/scripts/runner_guard.py [ROOT]
Exit status 0 when clean, 1 with one line per problem.
"""

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
DYNAMIC_OK = {("ci.yml", "test")}
DYNAMIC_MATRIX = re.compile(r"^\$\{\{\s*fromJSON\(\s*needs\.[\w-]+\.outputs\.[\w-]+\s*\)\s*\}\}$")
MATRIX_REF = re.compile(r"^\$\{\{\s*matrix\.([\w-]+)\s*\}\}$")

BENCH_TRIGGERS = {"push", "schedule", "workflow_dispatch"}
BENCH_PUSH_BRANCHES = ["release/**"]
BENCH_INPUTS = {"smoke_only"}
# Triggers that run code a pull request or a fork controls, or that a
# comment can start.
UNTRUSTED = {"pull_request", "pull_request_target", "workflow_run", "issue_comment",
             "pull_request_review", "pull_request_review_comment", "discussion_comment"}
REF_IF = "github.ref == 'refs/heads/main' || startsWith(github.ref, 'refs/heads/release/')"
PINNED = re.compile(r"^[\w.-]+/[\w.-]+(/[\w./-]+)?@[0-9a-f]{40}$")


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


def names_label(value):
    """True if the label appears anywhere in a runs-on value."""
    return LABEL in str(value).lower()


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
    if isinstance(raw, str) and DYNAMIC_MATRIX.match(raw.strip()):
        if (name, job_id) in DYNAMIC_OK:
            return []
        return [f"runs-on {value!r}: matrix.{key} comes from a job output ({raw.strip()})"]
    candidates = []
    if isinstance(raw, list):
        candidates.extend(raw)
    elif raw is not None:
        return [f"runs-on {value!r}: matrix.{key} is not a list ({raw!r})"]
    for entry in matrix.get("include") or []:
        if isinstance(entry, dict) and key in entry:
            candidates.append(entry[key])
    if isinstance(matrix.get("include"), str):
        return [f"runs-on {value!r}: matrix.include is an expression"]
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
        if re.search(r"\b(secrets|vars)\s*[.\[]", s) or "secrets: inherit" in s:
            out.append(f"{name}: references secrets or vars ({s.strip()[:60]!r})")
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
    if LABEL in text.lower():
        out.append(f"{name}: names the bench runner's label {LABEL!r}; only {BENCH} may")
    jobs = wf.get("jobs") or {}
    if not isinstance(jobs, dict):
        return out + [f"{name}: jobs is not a mapping"]
    for job_id, job in jobs.items():
        if isinstance(job, dict):
            out.extend(runs_on_problems(name, job_id, job))
    return out


def check(root):
    root = Path(root)
    out = []
    wf_dir = root / ".github" / "workflows"
    names = sorted(p.name for p in wf_dir.iterdir() if p.is_file() and p.suffix in {".yml", ".yaml"})
    if BENCH not in names:
        out.append(f"{BENCH} is missing")
    for name in names:
        out.extend(check_workflow(name, (wf_dir / name).read_text(encoding="utf-8")))
    # The rest of .github/ (scripts, actions, fixtures) mustn't name it either:
    # a script that prints a matrix could feed it to a runs-on.
    for path in sorted((root / ".github").rglob("*")):
        rel = path.relative_to(root).as_posix()
        if not path.is_file() or rel.startswith(".github/workflows/") or rel in NAMES_LABEL:
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
