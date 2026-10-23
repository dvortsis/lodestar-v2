#!/usr/bin/env python3
"""Generate and check web/licenses/rust.json, the Rust half of About's licences.

For every shipped target, `cargo metadata --filter-platform` resolves the
release feature set, and the walk from the `lodestar` package over normal
dependency edges gives the crates that end up in that target's binary.
Platform-only crates (winapi, rustix, ...) are listed only for the targets
that build them. Proc-macros count as shipped: they generate shipped code.

rust.json holds each crate's name, version, SPDX expression, repository,
targets and licence texts (read from the crate's own folder, stored once per
SHA-256), plus Lodestar's own licence and the notices overrides.json names.
It lives under web/ because the Docker web stage copies only web/.

Every shipped crate's licence must pass web/licenses/allow.json (owner's
decision §2.12). A crate without a licence field needs a reviewed entry in
web/licenses/overrides.json `missing`.

Usage:
    licenses.py            regenerate web/licenses/rust.json
    licenses.py --check    fail on drift or on a licence outside the allowlist
    licenses.py --metadata <target>=<file> ...   use recorded metadata (tests)

Python standard library only (no new dependency).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LICENSES = ROOT / "web" / "licenses"
OUT = LICENSES / "rust.json"

# The targets release.yml builds and ships. Keep in sync with the build matrix
# in .github/workflows/release.yml.
TARGETS = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
]

# The release feature set: `web` only. The default-on `fixture` feature pulls
# bench in and isn't shipped.
ROOT_PACKAGE = "lodestar"
FEATURES = ["--no-default-features", "--features", f"{ROOT_PACKAGE}/web"]

# Licence, notice and copyright files at the top of a package folder.
TEXT_FILE = re.compile(r"^(licen[cs]e|copying|notice|copyright|unlicen[cs]e)", re.IGNORECASE)
NOT_TEXT = {".rs", ".toml", ".json", ".yml", ".yaml", ".sh", ".py", ".js", ".ts", ".html"}

# prettier's settings for web/ (web/.prettierrc), so the generated file is
# already in prettier's format and `npm run check` leaves it alone.
PRINT_WIDTH = 100
TAB_WIDTH = 2


# --- SPDX ---------------------------------------------------------------------

# Deprecated identifiers and their current form (SPDX licence list, "deprecated").
DEPRECATED = {
    "GPL-1.0": "GPL-1.0-only",
    "GPL-1.0+": "GPL-1.0-or-later",
    "GPL-2.0": "GPL-2.0-only",
    "GPL-2.0+": "GPL-2.0-or-later",
    "GPL-3.0": "GPL-3.0-only",
    "GPL-3.0+": "GPL-3.0-or-later",
    "AGPL-1.0": "AGPL-1.0-only",
    "AGPL-3.0": "AGPL-3.0-only",
    "AGPL-3.0+": "AGPL-3.0-or-later",
    "LGPL-2.0": "LGPL-2.0-only",
    "LGPL-2.0+": "LGPL-2.0-or-later",
    "LGPL-2.1": "LGPL-2.1-only",
    "LGPL-2.1+": "LGPL-2.1-or-later",
    "LGPL-3.0": "LGPL-3.0-only",
    "LGPL-3.0+": "LGPL-3.0-or-later",
    "GFDL-1.3": "GFDL-1.3-only",
}


class SpdxError(ValueError):
    """A licence expression that can't be parsed."""


@dataclass(frozen=True)
class Verdict:
    ok: bool
    # Identifiers that failed (empty when ok), refused ones marked.
    reason: str


def _tokens(expr: str) -> list[str]:
    # The old crates.io form `MIT/Apache-2.0` means MIT OR Apache-2.0.
    expr = re.sub(r"\s*/\s*", " OR ", expr.strip())
    out = re.findall(r"\(|\)|[^\s()]+", expr)
    if not out:
        raise SpdxError("empty licence expression")
    return out


def parse(expr: str):
    """Parse an SPDX expression into nested tuples:
    ("or", [..]), ("and", [..]), ("with", id, exception) or ("id", id).
    Precedence, highest first: WITH, AND, OR. Operators are matched without
    regard to case (crates.io has `MIT or Apache-2.0`)."""
    toks = _tokens(expr)
    pos = 0

    def peek() -> str | None:
        return toks[pos] if pos < len(toks) else None

    def op(word: str) -> bool:
        t = peek()
        return t is not None and t.upper() == word

    def take() -> str:
        nonlocal pos
        if pos >= len(toks):
            raise SpdxError(f"unexpected end of {expr!r}")
        pos += 1
        return toks[pos - 1]

    def primary():
        t = take()
        if t == "(":
            node = disjunction()
            if peek() != ")":
                raise SpdxError(f"unbalanced parentheses in {expr!r}")
            take()
            return node
        if t == ")" or t.upper() in ("AND", "OR", "WITH"):
            raise SpdxError(f"unexpected {t!r} in {expr!r}")
        node = ("id", t)
        if op("WITH"):
            take()
            exc = peek()
            if exc is None or exc in ("(", ")") or exc.upper() in ("AND", "OR", "WITH"):
                raise SpdxError(f"WITH needs an exception in {expr!r}")
            take()
            node = ("with", t, exc)
        return node

    def conjunction():
        items = [primary()]
        while op("AND"):
            take()
            items.append(primary())
        return items[0] if len(items) == 1 else ("and", items)

    def disjunction():
        items = [conjunction()]
        while op("OR"):
            take()
            items.append(conjunction())
        return items[0] if len(items) == 1 else ("or", items)

    tree = disjunction()
    if pos != len(toks):
        raise SpdxError(f"unexpected {toks[pos]!r} in {expr!r}")
    return tree


def canonical(ident: str) -> str:
    return DEPRECATED.get(ident, ident)


@dataclass(frozen=True)
class Policy:
    allowed: frozenset[str]  # upper-cased
    refused: frozenset[str]  # upper-cased

    @staticmethod
    def load(path: Path = LICENSES / "allow.json") -> "Policy":
        data = json.loads(path.read_text(encoding="utf-8"))
        return Policy(
            frozenset(i.upper() for i in data["allowed"]),
            frozenset(i.upper() for i in data["refused"]),
        )

    def evaluate(self, expr: str) -> Verdict:
        """`OR` passes if any branch passes, `AND` needs every branch, and
        `X WITH exception` passes when X does. Identifiers match without
        regard to case, as SPDX specifies; deprecated ones are read as their
        current form first."""
        try:
            tree = parse(expr)
        except SpdxError as e:
            return Verdict(False, str(e))
        failed: list[str] = []

        def walk(node) -> bool:
            kind = node[0]
            if kind == "or":
                results = [walk(n) for n in node[1]]  # walk all, to name every failure
                return any(results)
            if kind == "and":
                results = [walk(n) for n in node[1]]
                return all(results)
            ident = canonical(node[1])
            key = ident.upper()
            if key in self.allowed:
                return True
            name = ident if ident == node[1] else f"{node[1]} (read as {ident})"
            failed.append(f"{name} (refused)" if key in self.refused else name)
            return False

        if walk(tree):
            return Verdict(True, "")
        return Verdict(False, "not allowed: " + ", ".join(dict.fromkeys(failed)))


# --- cargo metadata -----------------------------------------------------------


def run_metadata(target: str) -> dict:
    cmd = [
        "cargo",
        "metadata",
        "--locked",
        "--format-version",
        "1",
        "--filter-platform",
        target,
        *FEATURES,
    ]
    res = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    if res.returncode != 0:
        sys.exit(f"licenses.py: `{' '.join(cmd)}` failed:\n{res.stderr}")
    return json.loads(res.stdout)


def shipped(meta: dict, root: str = ROOT_PACKAGE) -> list[dict]:
    """The packages reachable from `root` over normal dependency edges: no dev
    or build edges. Proc-macros are reached through normal edges and count."""
    packages = {p["id"]: p for p in meta["packages"]}
    members = set(meta["workspace_members"])
    roots = [p["id"] for p in meta["packages"] if p["name"] == root and p["id"] in members]
    if len(roots) != 1:
        sys.exit(f"licenses.py: workspace package {root!r} not found in cargo metadata")
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    seen: set[str] = set()
    stack = roots[:]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k.get("kind") is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    return [packages[i] for i in seen]


def text_files(package: dict) -> dict[str, str]:
    """Licence texts in the package folder: file name -> text."""
    folder = Path(package["manifest_path"]).parent
    out: dict[str, str] = {}
    names = [package["license_file"]] if package.get("license_file") else []
    if folder.is_dir():
        names += sorted(
            f.name
            for f in folder.iterdir()
            if f.is_file() and TEXT_FILE.match(f.name) and f.suffix.lower() not in NOT_TEXT
        )
    for name in names:
        path = folder / name
        if name not in out and path.is_file():
            raw = path.read_bytes().decode("utf-8", errors="replace")
            out[name] = raw.replace("\r\n", "\n")
    return out


def sha256(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


class Problems(list):
    """Every failure found, so one run names them all. `warnings` don't fail."""

    def __init__(self) -> None:
        super().__init__()
        self.warnings: list[str] = []

    def add(self, msg: str) -> None:
        self.append(msg)


def load_overrides(path: Path = LICENSES / "overrides.json") -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def generate(
    metas: dict[str, dict],
    policy: Policy,
    overrides: dict,
    root: Path = ROOT,
) -> tuple[dict, Problems]:
    """Build rust.json's content from one cargo metadata per target."""
    problems = Problems()
    missing = {
        e["name"]: e for e in overrides.get("missing", []) if e.get("ecosystem") == "cargo"
    }
    donors = {e["name"]: e for e in overrides.get("texts", []) if e.get("ecosystem") == "cargo"}
    used_overrides: set[str] = set()
    used_donors: set[str] = set()
    crates: dict[tuple[str, str], dict] = {}
    first_party: dict[str, dict] = {}
    all_packages: dict[tuple[str, str], dict] = {}

    for target, meta in metas.items():
        members = set(meta["workspace_members"])
        for p in meta["packages"]:
            all_packages.setdefault((p["name"], p["version"]), p)
        for p in shipped(meta):
            key = (p["name"], p["version"])
            if p["id"] in members:
                first_party.setdefault(p["name"], p)
                continue
            entry = crates.get(key)
            if entry is None:
                entry = crates[key] = {"package": p, "targets": set()}
            entry["targets"].add(target)

    texts: dict[str, str] = {}

    def store(files: dict[str, str]) -> dict[str, str]:
        refs = {}
        for name, text in sorted(files.items()):
            h = sha256(text)
            texts[h] = text
            refs[name] = h
        return refs

    out_crates = []
    for (name, version), entry in sorted(crates.items()):
        p = entry["package"]
        expr = p.get("license")
        override = missing.get(name)
        files = text_files(p)
        record: dict = {"name": name, "version": version}
        if override is not None:
            used_overrides.add(name)
            if override.get("version") not in (None, version):
                problems.add(
                    f"{name} {version}: overrides.json `missing` was reviewed for "
                    f"{override['version']}; check the new version's licence and update it"
                )
            if override.get("license"):
                if expr:
                    problems.add(
                        f"{name} {version}: has licence {expr!r}; its override in "
                        "overrides.json is no longer needed for the licence"
                    )
                expr = override["license"]
                record["license_source"] = "override"
        donor_entry = donors.get(name)
        if donor_entry is not None:
            used_donors.add(name)
            source = donor_entry["from"]
            donor = next((q for (n, _v), q in sorted(all_packages.items()) if n == source), None)
            if donor is None:
                problems.add(
                    f"{name} {version}: overrides.json `texts` names {source!r}, not in the build"
                )
            elif files:
                problems.add(
                    f"{name} {version}: now ships its own licence text; drop its overrides.json "
                    "`texts` entry"
                )
            else:
                files = text_files(donor)
                record["texts_from"] = f"{donor['name']} {donor['version']}"
        if not expr:
            lf = p.get("license_file")
            problems.add(
                f"{name} {version}: no licence field"
                + (f" (ships {lf})" if lf else "")
                + "; add a reviewed `missing` entry to web/licenses/overrides.json"
            )
            expr = ""
        else:
            verdict = policy.evaluate(expr)
            if not verdict.ok:
                problems.add(f"{name} {version}: licence {expr!r} {verdict.reason}")
        record["license"] = expr
        record["repository"] = p.get("repository") or ""
        if any("proc-macro" in t["kind"] for t in p["targets"]):
            record["proc_macro"] = True
        record["targets"] = [t for t in TARGETS if t in entry["targets"]] + sorted(
            entry["targets"] - set(TARGETS)
        )
        record["texts"] = store(files)
        out_crates.append(record)

    # A stale entry is harmless; say so without failing.
    for name in sorted(set(missing) - used_overrides):
        problems.warnings.append(f"overrides.json `missing` names {name}, which no target builds")
    for name in sorted(set(donors) - used_donors):
        problems.warnings.append(f"overrides.json `texts` names {name}, which no target builds")

    own_licenses = sorted({p.get("license") or "" for p in first_party.values()})
    for name, p in sorted(first_party.items()):
        verdict = policy.evaluate(p.get("license") or "")
        if not verdict.ok:
            problems.add(f"{name} (workspace): licence {p.get('license')!r} {verdict.reason}")

    lodestar_files = {}
    licence = root / "LICENSE"
    if licence.is_file():
        lodestar_files["LICENSE"] = licence.read_text(encoding="utf-8").replace("\r\n", "\n")
    notices = []
    for n in overrides.get("notices", []):
        path = root / n["file"]
        if not path.is_file():
            problems.add(f"overrides.json notice {n['name']!r}: {n['file']} not found")
            continue
        notices.append(
            {
                "name": n["name"],
                "file": n["file"],
                "texts": store({path.name: path.read_text(encoding="utf-8").replace("\r\n", "\n")}),
            }
        )

    doc = {
        "generated_by": ".github/scripts/licenses.py",
        "targets": list(metas),
        "lodestar": {
            "license": " AND ".join(own_licenses),
            "crates": sorted(first_party),
            "texts": store(lodestar_files),
        },
        "notices": notices,
        "crates": out_crates,
        "texts": dict(sorted(texts.items())),
    }
    return doc, problems


# --- output -------------------------------------------------------------------


def dumps(value) -> str:
    """JSON in prettier's format for web/ (tabs, objects always broken,
    scalar arrays on one line when they fit in the print width), so
    `prettier --check` agrees with the drift check."""

    def scalar(v) -> str:
        return json.dumps(v, ensure_ascii=False)

    def width(s: str) -> int:
        return len(s)

    def fmt(v, depth: int, prefix_len: int, trailing: int) -> str:
        pad = "\t" * (depth + 1)
        end = "\t" * depth
        if isinstance(v, dict):
            if not v:
                return "{}"
            items = list(v.items())
            lines = []
            for i, (k, x) in enumerate(items):
                key = scalar(k) + ": "
                comma = "," if i < len(items) - 1 else ""
                body = fmt(x, depth + 1, (depth + 1) * TAB_WIDTH + width(key), len(comma))
                lines.append(pad + key + body + comma)
            return "{\n" + "\n".join(lines) + "\n" + end + "}"
        if isinstance(v, list):
            if not v:
                return "[]"
            if all(not isinstance(x, (dict, list)) for x in v):
                inline = "[" + ", ".join(scalar(x) for x in v) + "]"
                if prefix_len + width(inline) + trailing <= PRINT_WIDTH:
                    return inline
            lines = []
            for i, x in enumerate(v):
                comma = "," if i < len(v) - 1 else ""
                lines.append(pad + fmt(x, depth + 1, (depth + 1) * TAB_WIDTH, len(comma)) + comma)
            return "[\n" + "\n".join(lines) + "\n" + end + "]"
        return scalar(v)

    return fmt(value, 0, 0, 0) + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--check", action="store_true", help="fail on drift or a disallowed licence")
    ap.add_argument(
        "--metadata",
        nargs="+",
        metavar="TARGET=FILE",
        help="recorded cargo metadata per target instead of running cargo",
    )
    ap.add_argument("--out", type=Path, default=OUT, help=argparse.SUPPRESS)
    ap.add_argument(
        "--overrides", type=Path, default=LICENSES / "overrides.json", help=argparse.SUPPRESS
    )
    args = ap.parse_args(argv)

    if args.metadata:
        metas = {}
        for item in args.metadata:
            target, _, file = item.partition("=")
            metas[target] = json.loads(Path(file).read_text(encoding="utf-8"))
    else:
        metas = {t: run_metadata(t) for t in TARGETS}

    doc, problems = generate(metas, Policy.load(), load_overrides(args.overrides))
    text = dumps(doc)
    crates = len(doc["crates"])

    if args.check:
        current = args.out.read_text(encoding="utf-8") if args.out.is_file() else ""
        if current != text:
            problems.add(
                f"{args.out.relative_to(ROOT) if args.out.is_relative_to(ROOT) else args.out} "
                "is out of date: run `python3 .github/scripts/licenses.py` and commit it"
            )
    else:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text, encoding="utf-8", newline="\n")

    for w in problems.warnings:
        print(f"licenses.py: warning: {w}", file=sys.stderr)
    if problems:
        print(
            "licenses.py: shipped-licence check failed (owner's decision §2.12):", file=sys.stderr
        )
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1
    verb = "checked" if args.check else "wrote"
    print(f"licenses.py: {verb} {crates} crates, {len(text.encode())} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
