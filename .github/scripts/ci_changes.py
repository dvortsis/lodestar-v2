#!/usr/bin/env python3
"""Decide which CI jobs a pull request needs, from the files it changes.

Pushes to main, the nightly schedule and manual runs get everything. A pull
request gets:

- the Rust jobs (fmt, clippy, openapi drift, Linux and Windows tests) only if
  it touches Rust: a crate, bench/, fuzz/, a Cargo manifest or lockfile, the
  toolchain file, this workflow, web/src/lib/api/openapi.json (the drift
  job checks it), or one of the files listed in RUST_ALSO below (the
  companion templates, the Windows launcher, main.wxs, the packaging
  PowerShell scripts, the public keys, the docs that Rust tests read, the
  licence notices and the scripts the Rust jobs run);
- macOS tests only with the `full-ci` label (they run on every push to main
  and nightly);
- a benchmark gate, the importer test or a fuzz target only if it touches a
  crate that job builds, following the workspace's path dependencies (the
  logging bench: `lodestar-log` or the binary's `logging/` runtime);
- the soak tools' Python tests only if it touches `docs/soak/tools/`;
- the Unraid template checks only if it touches `packaging/unraid/` (UNRAID below);
- the companion pin tool's tests only if it touches `tools/companion-pins/`, its
  workflow, the catalogue or the Rust constants the tool mirrors (COMPANION_PINS);
- the cloud infrastructure checks (Lambda tests, template rebuilds, Terraform
  fmt/validate, PowerShell parse) only if it touches `infra/`, the fleet,
  fleet-emergency-down and operator workflows, the gluetun post-rules file,
  `docs/user/03-keep-lodestar-safe.md` or `docs/operations.md` (INFRA below);
- the update-manifest round trip only if it touches the manifest builder,
  its tests, release.yml or `crates/update/` (UPDATE_MANIFEST below);
- the RustSec advisory check only if it touches Cargo.lock, deny.toml, this
  workflow or this file (ADVISORIES below). Main and the nightly always run it,
  so an advisory published against a crate already locked turns main red the
  next night;
- `web` and `e2e` for web.yml / e2e.yml (see WEB_INPUTS below). Those two
  workflows always run on a pull request and use these outputs to skip their
  heavy jobs, so their `web-ok` / `e2e-ok` aggregates can be required.

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
    # templates.rs checks this folder is exactly what the crate writes, and its embedded post-rules copy.
    r"|^packaging/companions/(gluetun-post-rules\.txt$|compose/|unraid/)"
    # Windows packaging that Rust reads: the portable launcher (portable.rs include_str!),
    # main.wxs and every packaging/**/*.ps1 (legal_facts.rs), and the public keys that
    # crates/update/src/keys.rs compares with its embedded copies.
    r"|^packaging/windows/(main\.wxs$|portable/Start Lodestar\.cmd$)"
    r"|^packaging/.*\.ps1$"
    r"|^packaging/keys/[^/]+\.pub$"
    # Docs that Rust tests and includes read: the metrics table (metrics.rs, serve.rs),
    # the query-language examples (doc_examples.rs), the legal notice (api/src/legal.rs,
    # legal_facts.rs), the user guide (sources-plugins safety.rs) and the bitmagnet
    # reader-role SQL (bitmagnet_check.rs).
    r"|^docs/(metrics|query-language)\.md$"
    r"|^docs/legal/legal-notice\.md$"
    r"|^docs/user/"
    r"|^docs/operations/imports/bitmagnet-restore/reader-role\.sql$"
)

# Web and E2E applicability. Both workflows run on every pull request; these
# patterns decide whether their heavy jobs do. Files the web tests read from
# outside web/ are listed one by one (found by grepping web/ for `../crates`,
# `../docs`, `../brand`).
WEB_INPUTS = (
    r"web/"  # the app, its tests and web/src/lib/api/openapi.json (api:types is checked against it)
    r"|brand/source/"  # assets.ts builds the UI's brand assets from it
    r"|crates/lodestar/src/settings/golden/"  # settings/logs golden files the UI tests compare against
    r"|crates/lodestar/src/settings/preferences\.rs$|crates/search/src/query\.rs$"  # parsed by UI tests
    r"|docs/(design-tokens|themes)\.md$"  # UI tests read the token spec and the theme fixture
    r"|\.github/scripts/ci_changes\.py$"  # this file decides applicability
)
WEB = re.compile(rf"^({WEB_INPUTS}|\.github/workflows/web\.yml$)")
# E2E drives the built UI, so it needs everything web does, plus crates/api/:
# the API contract that openapi.json is generated from. Docs, other crates and
# unrelated workflows never apply.
E2E = re.compile(rf"^({WEB_INPUTS}|crates/api/|\.github/workflows/e2e\.yml$)")

# The cloud infrastructure job (X-cloud-infra): Lambda tests, template rebuilds,
# Terraform fmt/validate and a PowerShell parse check. Never touches a cloud.
# The fleet and operator workflows too, whose structure and public-log leak
# tests live in infra/fleet/tests (OP-1) and infra/operator/tests (OP-4).
# The rules file and the docs that quote it: the fleet tests that read them run in this job.
INFRA = re.compile(
    r"^(infra/"
    r"|packaging/companions/gluetun-post-rules\.txt$|docs/user/03-keep-lodestar-safe\.md$|docs/operations\.md$"
    r"|\.github/workflows/(fleet|fleet-emergency-down|operator-wake|operator-apply|operator-injection|operator-tripwire[\w-]*)\.yml$)"
)
# The update manifest's builder, the workflow that signs with it, and the
# verifier it round-trips through.
UPDATE_MANIFEST = re.compile(
    r"^(\.github/scripts/(test_)?update_manifest\.py$|\.github/workflows/release\.yml$|crates/update/)"
)

# The pin tool (CMP-13), and what its tests read: the committed catalogue's
# layout and the Rust constants it mirrors.
COMPANION_PINS = re.compile(
    r"^(tools/companion-pins/|\.github/workflows/companion-pins\.yml$|crates/companions/data/catalogue\.json$"
    r"|crates/companions/src/catalogue\.rs$|crates/lodestar/src/update/check\.rs$)"
)

# The RustSec advisory check (X-ci-advisories): what it reads (the lockfile and
# deny.toml), and what decides how it runs. A Cargo.toml change that moves a
# version also changes Cargo.lock.
ADVISORIES = re.compile(r"^(Cargo\.lock|deny\.toml|\.github/workflows/ci\.yml|\.github/scripts/ci_changes\.py)$")

# The Unraid templates and their validator. The validator and its tests live beside
# the templates, so one prefix covers all of them.
UNRAID = re.compile(r"^packaging/unraid/")

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


def parse_changed(raw: str) -> list[str]:
    """The changed paths from `git diff --name-only -z`: split on NUL, never on spaces."""
    return [f for f in raw.split("\0") if f]


def main() -> None:
    event, base, head = sys.argv[1], sys.argv[2], sys.argv[3]
    try:
        labels = {str(l).lower() for l in json.loads(sys.argv[4]) or []} if len(sys.argv) > 4 else set()
    except (ValueError, TypeError):
        labels = set()
    out: dict[str, str] = {}

    if event != "pull_request" or "full-ci" in labels:
        out.update(
            rust="true",
            bench="true",
            filter="true",
            importer="true",
            log_bench="true",
            mmap_retention="true",
            soak_tools="true",
            companion_pins="true",
            infra="true",
            unraid="true",
            update_manifest="true",
            advisories="true",
            web="true",
            e2e="true",
        )
        out["fuzz"] = json.dumps(FUZZ)
        out["test_os"] = json.dumps(["ubuntu-latest", "windows-latest", "macos-latest"])
        emit(out)
        return

    changed = parse_changed(
        subprocess.run(
            # --no-renames: a file moved out of a crate must list its old path too.
            # -z: NUL-separated and unquoted, so a path with a space stays one path.
            ["git", "diff", "--no-renames", "--name-only", "-z", f"{base}...{head}"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )

    out["web"] = str(any(WEB.match(f) for f in changed)).lower()
    out["e2e"] = str(any(E2E.match(f) for f in changed)).lower()

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
    out["companion_pins"] = str(everything or any(COMPANION_PINS.match(f) for f in changed)).lower()
    out["unraid"] = str(everything or any(UNRAID.match(f) for f in changed)).lower()
    out["infra"] = str(everything or any(INFRA.match(f) for f in changed)).lower()
    out["update_manifest"] = str(everything or any(UPDATE_MANIFEST.match(f) for f in changed)).lower()
    out["advisories"] = str(any(ADVISORIES.match(f) for f in changed)).lower()
    # The mmap-cache retention test (X-mmap-cache-prune): the bench crate
    # that holds it, or anything it builds (the search crate among them).
    out["mmap_retention"] = str(needs({"lodestar-bench"}, tested=True)).lower()
    # A change under fuzz/ (a target, the harness, its lockfile) runs them all.
    fuzz_all = "lodestar-fuzz" in touched
    out["fuzz"] = json.dumps([t for t in FUZZ if fuzz_all or needs(fuzz_roots(t))])
    emit(out)


def emit(out: dict[str, str]) -> None:
    for key, value in out.items():
        print(f"{key}={value}")


if __name__ == "__main__":
    main()
