#!/usr/bin/env python3
"""Decide which CI jobs a pull request needs, from the files it changes.

Pushes to main, the nightly schedule and manual runs get everything. A pull
request gets:

- the Rust jobs (fmt, clippy, openapi drift, Linux and Windows tests) only if
  it touches Rust: a crate, bench/, fuzz/, a Cargo manifest or lockfile, the
  toolchain file, this workflow, or web/src/lib/api/openapi.json (the drift
  job checks it);
- macOS tests only with the `full-ci` label (they run on every push to main
  and nightly);
- a benchmark gate, the importer test or a fuzz target only if it touches a
  crate that job builds, following the workspace's path dependencies (the
  logging bench: `lodestar-log` or the binary's `logging/` runtime);
- the soak tools' Python tests only if it touches `docs/soak/tools/`.

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
# Outside the crates but checked by a Rust job: the drift job regenerates these.
# The logging check runs in the lint job.
RUST_ALSO = re.compile(
    r"^web/src/lib/api/openapi\.json$|^web/licenses/"
    r"|^\.github/scripts/(test_)?licenses\.py$|^\.github/scripts/fixtures/licenses/"
    r"|^\.github/scripts/(test_)?log_lint\.py$"
    # rust.json embeds these notices; licenses.py --check runs with the Rust job.
    r"|^LICENSE$|^brand/LICENSE\.md$"
)

EVERYTHING = re.compile(
    r"^(Cargo\.(toml|lock)|rust-toolchain(\.toml)?|\.cargo/|\.github/workflows/ci\.yml"
    r"|\.github/scripts/ci_changes\.py)"
)


def packages() -> dict[str, tuple[Path, set[str], set[str]]]:
    """Workspace packages: name -> (directory, path dependencies, path dev-dependencies)."""
    root = tomllib.loads((ROOT / "Cargo.toml").read_text())
    inherited = root.get("workspace", {}).get("dependencies", {})
    dirs = [d for d in (ROOT / "crates").iterdir() if (d / "Cargo.toml").is_file()]
    dirs += [ROOT / "bench", ROOT / "fuzz"]

    def path_deps(table: dict) -> set[str]:
        deps = set()
        for dep, spec in table.items():
            if not isinstance(spec, dict):
                continue
            if spec.get("workspace"):  # `foo.workspace = true`: the root says where it lives
                root_spec = inherited.get(dep)
                if isinstance(root_spec, dict) and "path" in root_spec:
                    deps.add(root_spec.get("package", dep))
            elif "path" in spec:
                deps.add(spec.get("package", dep))
        return deps

    found: dict[str, tuple[Path, set[str], set[str]]] = {}
    for d in dirs:
        manifest = tomllib.loads((d / "Cargo.toml").read_text())
        name = manifest["package"]["name"]
        tables = [manifest, *manifest.get("target", {}).values()]
        deps: set[str] = set()
        dev: set[str] = set()
        for t in tables:
            deps |= path_deps(t.get("dependencies", {})) | path_deps(t.get("build-dependencies", {}))
            dev |= path_deps(t.get("dev-dependencies", {}))
        found[name] = (d, deps, dev)
    return found


def closure(
    pkgs: dict[str, tuple[Path, set[str], set[str]]], roots: set[str], tested: bool = False
) -> set[str]:
    """`roots` and every workspace package they build, transitively. With
    `tested`, the roots' own dev-dependencies too (`cargo test -p` builds
    those, but never a dependency's dev-dependencies)."""
    start = set(roots)
    if tested:
        for r in roots:
            if r in pkgs:
                start |= pkgs[r][2]
    seen, todo = set(), list(start)
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
    try:
        labels = {str(l).lower() for l in json.loads(sys.argv[4]) or []} if len(sys.argv) > 4 else set()
    except (ValueError, TypeError):
        labels = set()
    out: dict[str, str] = {}

    if event != "pull_request" or "full-ci" in labels:
        out.update(
            rust="true", bench="true", filter="true", importer="true", log_bench="true", soak_tools="true"
        )
        out["fuzz"] = json.dumps(FUZZ)
        out["test_os"] = json.dumps(["ubuntu-latest", "windows-latest", "macos-latest"])
        emit(out)
        return

    changed = subprocess.run(
        # --no-renames: a file moved out of a crate must list its old path too.
        ["git", "diff", "--no-renames", "--name-only", f"{base}...{head}"],
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
        for name, (d, _, _) in pkgs.items():
            if path == d or d in path.parents:
                touched.add(name)

    def needs(roots: set[str], tested: bool = False) -> bool:
        return everything or bool(closure(pkgs, roots, tested) & touched)

    rust = everything or bool(touched) or any(RUST_ALSO.match(f) for f in changed)
    out["rust"] = str(rust).lower()
    out["test_os"] = json.dumps(["ubuntu-latest", "windows-latest"] if rust else [])
    out["bench"] = str(needs({"lodestar-bench"})).lower()
    out["filter"] = str(needs({"lodestar-filter"}, tested=True)).lower()
    out["importer"] = str(needs({"lodestar-sources-importers"}, tested=True)).lower()
    # The logging hot-path bench: the log crate, or the binary's runtime.
    out["log_bench"] = str(
        needs({"lodestar-log"})
        or any(
            f.startswith("crates/lodestar/src/logging/") or f == "crates/lodestar/src/settings/logging.rs"
            for f in changed
        )
    ).lower()
    out["soak_tools"] = str(everything or any(f.startswith("docs/soak/tools/") for f in changed)).lower()
    # A change under fuzz/ (a target, the harness, its lockfile) runs them all.
    fuzz_all = "lodestar-fuzz" in touched
    out["fuzz"] = json.dumps([t for t in FUZZ if fuzz_all or needs(fuzz_roots(t))])
    emit(out)


def emit(out: dict[str, str]) -> None:
    for key, value in out.items():
        print(f"{key}={value}")


if __name__ == "__main__":
    main()
