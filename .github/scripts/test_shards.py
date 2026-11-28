#!/usr/bin/env python3
"""Which `cargo test --workspace` targets each shard of ci.yml's `test` job runs.

    python3 .github/scripts/test_shards.py <shard>

prints that shard's target filters, one `cargo test` invocation per line,
for example `--test=alpha --test=beta`. Every shard keeps `--workspace`, so cargo
resolves features exactly as a plain `cargo test --workspace` does and builds
byte-identical test binaries; the filters only choose which ones a shard
builds and runs (X-ci-test-shard).

The shards together cover every target a plain `cargo test --workspace`
builds or runs, and each exactly once:

- `lib`: every library's unit tests (`--lib`);
- `bins`: every binary's unit tests, plus the integration tests the job's
  root-only steps run again under sudo (ROOT_STEP_TESTS), so those steps
  reuse this shard's build;
- `tests-a`, `tests-b`: every other integration test, split by WEIGHTS.
  `tests-a` also runs the doc tests (`--doc`, an invocation of its own,
  since cargo can't mix it with other filters): they need the plain
  libraries the integration tests build anyway, which `lib` never does.

Examples are built, not run, by `cargo test`; the job builds them on the
`bins` shard with its own step, and this script only checks they exist.

A target kind that plain `cargo test` would build and no shard covers (a
bench or example with `test = true`, a test with `test = false` and so on)
fails the script, so a new kind of target can't drop out of CI unnoticed.

A test with `required-features` (lodestar-bench's mmap_retention) is one
plain `cargo test` skips while those features are off, and `--test` would
refuse, so no shard names it. The script fails if anything in the
workspace could turn such a feature on, since then plain `cargo test`
might run it.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

SHARDS = ("lib", "bins", "tests-a", "tests-b")
SPLIT = ("tests-a", "tests-b")

# The integration tests the root-only steps run (ci.yml, `test` job). They
# build on the `bins` shard, with the lodestar binary's unit tests.
ROOT_STEP_TESTS = ("privdrop", "cli", "worker_join", "supervise")

# Seconds per test target on a 4-core Linux machine (2026-11-28). They only
# balance tests-a against tests-b; a target that isn't listed still runs, at
# DEFAULT_WEIGHT. Names shared by several packages (`--test watch` runs each
# package's `watch`) add up.
WEIGHTS = {
    "dataset": 142,
    "watch": 90,
    "sim": 56,
    "supervisor": 45,
    "identify": 45,
    "props": 28,
    "unverified_details": 21,
}
DEFAULT_WEIGHT = 2

NAME = re.compile(r"^[A-Za-z0-9_-]+$")


class ShardError(Exception):
    pass


def targets(metadata: dict) -> dict[str, list[tuple[str, dict]]]:
    """Workspace targets by kind: kind -> [(package, target)]."""
    members = set(metadata["workspace_members"])
    found: dict[str, list[tuple[str, dict]]] = {}
    for pkg in metadata["packages"]:
        if pkg["id"] not in members:
            continue
        for t in pkg["targets"]:
            kinds = t["kind"]
            if any(k in ("lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro") for k in kinds):
                kind = "lib"
            elif len(kinds) == 1:
                kind = kinds[0]
            else:
                raise ShardError(f"{pkg['name']}: target {t['name']} has kinds {kinds}")
            found.setdefault(kind, []).append((pkg["name"], t))
    return found


def check(found: dict[str, list[tuple[str, dict]]]) -> None:
    """Fail on any target plain `cargo test` treats in a way no shard does."""
    for kind, items in found.items():
        for pkg, t in items:
            where = f"{pkg}: {kind} target {t['name']}"
            if not NAME.match(t["name"]):
                raise ShardError(f"{where}: unexpected characters in the name")
            if kind == "lib":
                if not t.get("test", True):
                    raise ShardError(f"{where} has test = false; --lib would still test it")
            elif kind == "bin":
                if not t.get("test", True):
                    raise ShardError(f"{where} has test = false; --bins would still test it")
            elif kind == "test":
                if not t.get("test", True):
                    raise ShardError(f"{where} has test = false; plain cargo test skips it, --test wouldn't")
            elif kind == "example":
                if t.get("test", False):
                    raise ShardError(f"{where} has test = true; no shard runs example tests")
            elif kind == "bench":
                if t.get("test", False):
                    raise ShardError(f"{where} has test = true; no shard runs bench tests")
            elif kind != "custom-build":
                raise ShardError(f"{where}: no shard covers this kind")


def feature_closure(features: dict[str, list[str]], start: list[str]) -> set[str]:
    """The package's own features that `start` turns on."""
    seen: set[str] = set()
    todo = list(start)
    while todo:
        f = todo.pop()
        if f in seen or f not in features:
            continue
        seen.add(f)
        todo.extend(x for x in features[f] if "/" not in x and not x.startswith("dep:"))
    return seen


def maybe_enabled(metadata: dict, pkg_name: str, feature: str) -> bool:
    """Could a plain `cargo test --workspace` turn `feature` of a member on?
    Errs towards yes: default features, any member's dependency on it that
    names the feature, or any member feature that forwards to it."""
    members = set(metadata["workspace_members"])
    pkgs = [p for p in metadata["packages"] if p["id"] in members]
    own = next(p for p in pkgs if p["name"] == pkg_name)
    feats = own.get("features") or {}
    if feature in feature_closure(feats, ["default"]):
        return True
    for p in pkgs:
        for d in p.get("dependencies") or []:
            if d["name"] == pkg_name:
                if feature in feature_closure(feats, list(d.get("features") or []) + (["default"] if d.get("uses_default_features", True) else [])):
                    return True
        for values in (p.get("features") or {}).values():
            for v in values:
                if v in (f"{pkg_name}/{feature}", f"{pkg_name}?/{feature}"):
                    return True
    return False


def plan(metadata: dict) -> dict[str, list[list[str]]]:
    """Every shard's cargo invocations, each a list of target filters."""
    found = targets(metadata)
    check(found)
    runnable, skipped = [], set()
    for pkg, t in found.get("test", []):
        required = t.get("required-features") or []
        if not required:
            runnable.append((pkg, t))
            continue
        for f in required:
            if maybe_enabled(metadata, pkg, f):
                raise ShardError(f"{pkg}: test {t['name']} needs feature {f}, which the workspace may turn on; "
                                 "plain cargo test might run it, so test_shards.py can't place it")
        skipped.add(t["name"])
    clash = skipped & {t["name"] for _, t in runnable}
    if clash:
        raise ShardError(f"--test {', '.join(sorted(clash))} would also name a target plain cargo test skips")
    found["test"] = runnable
    tests = sorted({t["name"] for _, t in found.get("test", [])})
    missing = [n for n in ROOT_STEP_TESTS if n not in tests]
    if missing:
        raise ShardError(f"the root-only steps' tests are gone: {', '.join(missing)}")
    weight = {n: 0 for n in tests}
    for _, t in found.get("test", []):
        weight[t["name"]] += WEIGHTS.get(t["name"], DEFAULT_WEIGHT)
    split = [n for n in tests if n not in ROOT_STEP_TESTS]
    # Longest first, each to the lighter shard; ties go by name, so every
    # shard computes the same split.
    load = {s: 0 for s in SPLIT}
    chosen: dict[str, list[str]] = {s: [] for s in SPLIT}
    for n in sorted(split, key=lambda n: (-weight[n], n)):
        s = min(SPLIT, key=lambda s: (load[s], SPLIT.index(s)))
        load[s] += weight[n]
        chosen[s].append(n)
    out = {
        "lib": [["--lib"]],
        "bins": [["--bins", *[f"--test={n}" for n in ROOT_STEP_TESTS]]],
    }
    for s in SPLIT:
        if not chosen[s]:
            raise ShardError(f"{s} got no tests")
        out[s] = [[f"--test={n}" for n in sorted(chosen[s])]]
    out["tests-a"].append(["--doc"])
    return out


def metadata() -> dict:
    return json.loads(
        subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )


def main(argv: list[str]) -> int:
    if len(argv) != 2 or argv[1] not in SHARDS:
        print(f"usage: test_shards.py {{{'|'.join(SHARDS)}}}", file=sys.stderr)
        return 2
    # Bare \n even on Windows, where bash would hand a \r on to cargo.
    sys.stdout.reconfigure(newline="\n")
    try:
        for line in plan(metadata())[argv[1]]:
            print(" ".join(line))
    except ShardError as e:
        print(f"::error::test_shards.py: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
