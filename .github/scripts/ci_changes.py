#!/usr/bin/env python3
"""Decide which CI jobs a pull request needs, from the files it changes.

Pushes to main, the nightly schedule and manual runs get everything. A pull
request gets:

- the Rust jobs (fmt, clippy, openapi drift, Linux and Windows tests) only if
  it touches Rust: a crate, bench/, fuzz/, a Cargo manifest or lockfile, the
  toolchain file, or this workflow;
- macOS tests only with the `full-ci` label (they run on every push to main
  and nightly);
- a benchmark gate, the importer test or a fuzz target only if it touches a
  crate that job builds, following the workspace's path dependencies.

Prints `key=value` lines for $GITHUB_OUTPUT. Usage:
    ci_changes.py <event> <base-sha> <head-sha> [<labels as JSON>]
"""

import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Each fuzz target: the workspace packages it builds directly (fuzz/fuzz_targets/*.rs).
FUZZ = sorted(p.stem for p in (ROOT / "fuzz/fuzz_targets").glob("*.rs"))

# Paths that change how every Rust job builds: run all of them.
EVERYTHING = re.compile(
    r"^(Cargo\.(toml|lock)|rust-toolchain(\.toml)?|\.cargo/|\.github/workflows/ci\.yml"
    r"|\.github/scripts/ci_changes\.py)"
)


def packages() -> dict[str, tuple[Path, set[str]]]:
    """Workspace packages: name -> (directory, names of its path dependencies)."""
    dirs = [d for d in (ROOT / "crates").iterdir() if (d / "Cargo.toml").is_file()]
    dirs += [ROOT / "bench", ROOT / "fuzz"]
    found: dict[str, tuple[Path, set[str]]] = {}
    for d in dirs:
        manifest = tomllib.loads((d / "Cargo.toml").read_text())
        name = manifest["package"]["name"]
        deps: set[str] = set()
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            for dep, spec in manifest.get(table, {}).items():
                if isinstance(spec, dict) and "path" in spec:
                    deps.add(spec.get("package", dep))
        for target in manifest.get("target", {}).values():
            for table in ("dependencies", "dev-dependencies"):
                for dep, spec in target.get(table, {}).items():
                    if isinstance(spec, dict) and "path" in spec:
                        deps.add(spec.get("package", dep))
        found[name] = (d, deps)
    return found


def closure(pkgs: dict[str, tuple[Path, set[str]]], roots: set[str]) -> set[str]:
    """`roots` and every workspace package they depend on, transitively."""
    seen, todo = set(), list(roots)
    while todo:
        name = todo.pop()
        if name in seen or name not in pkgs:
            continue
        seen.add(name)
        todo.extend(pkgs[name][1])
    return seen


def fuzz_roots(target: str) -> set[str]:
    source = (ROOT / "fuzz/fuzz_targets" / f"{target}.rs").read_text()
    return {m.replace("_", "-") for m in re.findall(r"\blodestar_[a-z_]+", source)}


def main() -> None:
    event, base, head = sys.argv[1], sys.argv[2], sys.argv[3]
    labels = set(json.loads(sys.argv[4])) if len(sys.argv) > 4 and sys.argv[4] else set()
    out: dict[str, str] = {}

    if event != "pull_request" or "full-ci" in labels:
        out.update(rust="true", bench="true", filter="true", importer="true")
        out["fuzz"] = json.dumps(FUZZ)
        out["test_os"] = json.dumps(["ubuntu-latest", "windows-latest", "macos-latest"])
        emit(out)
        return

    changed = subprocess.run(
        ["git", "diff", "--name-only", f"{base}...{head}"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.split()

    pkgs = packages()
    everything = any(EVERYTHING.match(f) for f in changed)
    touched: set[str] = set()
    for f in changed:
        path = ROOT / f
        for name, (d, _) in pkgs.items():
            if path == d or d in path.parents:
                touched.add(name)

    def needs(roots: set[str]) -> bool:
        return everything or bool(closure(pkgs, roots) & touched)

    rust = everything or bool(touched)
    out["rust"] = str(rust).lower()
    out["test_os"] = json.dumps(["ubuntu-latest", "windows-latest"] if rust else [])
    out["bench"] = str(needs({"lodestar-bench"})).lower()
    out["filter"] = str(needs({"lodestar-filter"})).lower()
    out["importer"] = str(needs({"lodestar-sources-importers"})).lower()
    # A change under fuzz/ (a target, the harness, its lockfile) runs them all.
    fuzz_all = "lodestar-fuzz" in touched
    out["fuzz"] = json.dumps([t for t in FUZZ if fuzz_all or needs(fuzz_roots(t))])
    emit(out)


def emit(out: dict[str, str]) -> None:
    for key, value in out.items():
        print(f"{key}={value}")


if __name__ == "__main__":
    main()
