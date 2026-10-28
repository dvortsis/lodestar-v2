"""Build a private CR-0 candidate from clean source; never launch a public crawl.

Use Python 3.10+ on Windows with existing offline Rust/npm dependencies. Output
must be a NEW direct child of this checkout's target directory. Failed outputs
are preserved for diagnosis. A package is valid only after its final manifest.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from datetime import datetime, timezone

GIB = 1024**3
TOOLS = ("cr0-soak.ps1", "cr0-common.ps1", "cr0-signal.ps1")


def digest(path: Path) -> str:
    with path.open("rb") as source:
        result = hashlib.sha256()
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
        return result.hexdigest().upper()


def git(repo: Path, *args: str) -> str:
    return subprocess.check_output(
        ["git", "-c", f"safe.directory={repo.as_posix()}", "-C", str(repo), *args],
        text=True, encoding="utf-8",
    ).strip()


def source_identity(repo: Path) -> dict:
    if git(repo, "status", "--porcelain=v1", "--untracked-files=all"):
        raise ValueError("Commit or isolate all source changes before preparation.")
    return {
        "source_commit": git(repo, "rev-parse", "HEAD"),
        "source_tree": git(repo, "rev-parse", "HEAD^{tree}"),
        "source_date_epoch": git(repo, "show", "-s", "--format=%ct", "HEAD"),
    }


def check_identity(repo: Path, expected: dict) -> None:
    if source_identity(repo) != expected:
        raise ValueError("Source identity changed during preparation.")


def output_path(repo: Path, requested: Path) -> Path:
    # Resolving before containment catches directory junctions as well as '..'.
    target = repo / "target"
    if target.resolve() != target or target.is_symlink():
        raise ValueError("The build target must not redirect outside the checkout.")
    candidate = requested if requested.is_absolute() else repo / requested
    candidate = candidate.resolve()
    if candidate.parent != target or candidate.exists():
        raise ValueError("Output must be a new direct child of checkout/target.")
    if candidate.name.casefold() in {"cargo", "npm-cache", "debug", "release"}:
        raise ValueError("Output must not reuse a generated cache directory.")
    return candidate


def files_below(root: Path) -> list[Path]:
    files = []
    def failed(error: OSError) -> None:
        raise error
    for directory, dirs, names in os.walk(root, followlinks=False, onerror=failed):
        parent = Path(directory)
        for name in dirs + names:
            item = parent / name
            if item.is_symlink() or item.resolve() != item.absolute():
                raise ValueError("Package inputs must not contain links or junctions.")
        files.extend(parent / name for name in names)
    return sorted(files)


def check_storage(repo: Path) -> None:
    import tempfile

    for location in (repo, Path(tempfile.gettempdir())):
        if shutil.disk_usage(location).free < 50 * GIB:
            raise ValueError("Preparation requires at least 50 GiB free on each used volume.")
    cargo = repo / "target" / "cargo"
    if cargo.resolve() != cargo:
        raise ValueError("Cargo cache must not be a junction.")
    if cargo.exists() and sum(path.stat().st_size for path in files_below(cargo)) > 35 * GIB:
        raise ValueError("Cargo output exceeds the 35 GiB inspection ceiling; no cleanup performed.")


def smoke_passed(record: dict, exe_hash: str, index_hash: str) -> bool:
    shutdown = record.get("shutdown") or {}
    return (
        record.get("checks_passed") is True
        and record.get("failure") is None
        and record.get("exe_sha256") == exe_hash
        and record.get("index_sha256") == index_hash
        and shutdown.get("signal_delivered") is True
        and shutdown.get("exit_confirmed") is True
        and shutdown.get("exit_code") == 0
        and shutdown.get("stop_mode") == "exited_0"
        and shutdown.get("errors") == []
    )


def write_json(path: Path, value: dict) -> None:
    # Exclusive creation: no old build record, manifest or evidence is replaced.
    with path.open("x", encoding="utf-8", newline="\n") as output:
        json.dump(value, output, indent=2)
        output.write("\n")


def verify_package(root: Path) -> None:
    manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1 or manifest.get("state") != "packaging_checks_passed_awaiting_launch_environment_review":
        raise ValueError("Unrecognized or incomplete package manifest.")
    entries = manifest.get("files", {})
    required = {"lodestar.exe", "build-record.json", "ui/index.html", *TOOLS}
    if not required.issubset(entries):
        raise ValueError("Package manifest is missing required files.")
    for name, expected in entries.items():
        path = root / name
        if Path(name).is_absolute() or ".." in Path(name).parts or not path.resolve().is_relative_to(root.resolve()):
            raise ValueError("Manifest file escapes the package.")
        if digest(path) != expected:
            raise ValueError("Package file hash mismatch.")
    build = json.loads((root / "build-record.json").read_text(encoding="utf-8"))
    if (build.get("source_commit") != manifest.get("source_commit")
            or build.get("source_tree") != manifest.get("source_tree")
            or build.get("exe_sha256") != entries["lodestar.exe"]
            or build.get("index_sha256") != entries["ui/index.html"]):
        raise ValueError("Build record and package identity differ.")


def prepare(repo: Path, output: Path) -> None:
    identity = source_identity(repo)
    output = output_path(repo, output)
    check_storage(repo)
    npm = shutil.which("npm.cmd")
    if not npm:
        raise ValueError("Installed Windows npm is required; no dependencies will be installed.")
    output.mkdir(parents=True, exist_ok=False)
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("LODESTAR_")}
    env.update({
        "CARGO_TARGET_DIR": str(repo / "target" / "cargo"),
        "CARGO_INCREMENTAL": "0", "CARGO_BUILD_JOBS": "4",
        "LODESTAR_COMMIT": identity["source_commit"],
        "LODESTAR_CHANNEL": "dev", "SOURCE_DATE_EPOCH": identity["source_date_epoch"],
        "npm_config_offline": "true",
        "npm_config_cache": str(repo / "target" / "npm-cache"),
    })

    def run(name: str, args: list[str], cwd: Path = repo) -> None:
        print(f"{name}: started", flush=True)
        with (output / f"{name}.log").open("xb") as log:
            result = subprocess.run(args, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT)
        if result.returncode:
            raise ValueError(f"{name} failed ({result.returncode}); private log preserved.")
        print(f"{name}: passed", flush=True)

    run("frontend-build", [npm, "run", "build"], repo / "web")
    run("frontend-size", [npm, "run", "size"], repo / "web")
    check_identity(repo, identity)
    ui = repo / "web" / "build"
    files_below(ui)  # Reject redirected inputs before copying.
    shutil.copytree(ui, output / "ui")
    env["LODESTAR_WEB_BUILD"] = str(output / "ui")
    run("release-build", ["cargo", "build", "--release", "--locked", "--offline", "-p", "lodestar", "--bin", "lodestar"])
    check_identity(repo, identity)
    exe = output / "lodestar.exe"
    shutil.copyfile(repo / "target" / "cargo" / "release" / "lodestar.exe", exe)
    for name in TOOLS:
        shutil.copyfile(repo / "docs" / "soak" / "tools" / name, output / name)
    index_hash = digest(output / "ui" / "index.html")
    exe_hash = digest(exe)
    run("offline-smoke", [
        "pwsh", "-NoProfile", "-File", str(repo / "docs" / "soak" / "tools" / "smoke_windows.ps1"),
        "-Exe", str(exe), "-Index", str(output / "ui" / "index.html"), "-Root", str(output / "smoke"),
    ])
    smoke = json.loads((output / "smoke" / "smoke.json").read_text(encoding="utf-8-sig"))
    if not smoke_passed(smoke, exe_hash, index_hash):
        raise ValueError("Smoke evidence is incomplete or does not match the packaged files.")
    check_identity(repo, identity)
    check_storage(repo)
    rustc = subprocess.check_output(["rustc", "-vV"], text=True)
    host = next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    build = {
        "schema_version": 1, **identity, "exe_sha256": exe_hash,
        "index_sha256": index_hash, "ui": "embedded",
        "rustc": rustc.strip(), "cargo": subprocess.check_output(["cargo", "-V"], text=True).strip(),
        "target": host, "node": subprocess.check_output(["node", "--version"], text=True).strip(),
        "prepared_utc": datetime.now(timezone.utc).isoformat(),
    }
    write_json(output / "build-record.json", build)
    # No raw logs, setup tokens, database files, or absolute paths in this manifest.
    package_files = [exe, output / "build-record.json", *(output / name for name in TOOLS), *files_below(output / "ui")]
    write_json(output / "manifest.json", {
        "schema_version": 1, **identity,
        "state": "packaging_checks_passed_awaiting_launch_environment_review",
        "files": {path.relative_to(output).as_posix(): digest(path) for path in package_files},
        "guardrails": {"start_free_gib": 50, "warn_free_gib": 20, "stop_free_gib": 10,
                       "max_redirected_log_gib": 1, "disk_check_seconds": 10,
                       "warmup_minutes": 30, "baseline_hours": 24, "suppression_seconds": 21604},
    })
    verify_package(output)
    print("Private package prepared; no public crawler launched. Recheck launch gates before use.", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--verify", action="store_true", help="Only verify an existing package; no build or process launch.")
    args = parser.parse_args()
    if sys.platform != "win32":
        parser.error("Native Windows preparation only.")
    repo = Path(__file__).resolve().parents[3]
    try:
        if args.verify:
            verify_package(args.output.resolve())
            print("Package hashes and build identity verified; launch environment still requires review.")
        else:
            prepare(repo, args.output)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"Preparation stopped: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
