#!/usr/bin/env python3
"""The logging check (architecture §12.1, B-log1).

First-party code logs only through `log_event!`, whose fields are typed
and declared (the macro refuses undeclared fields and wrong kinds at
compile time). Every `crates/*/src/**/*.rs` file is checked, outside
tests. It refuses:

- plain `tracing` macros (`trace!` … `error!`, `event!`, span macros used
  as log lines, `warn !(` included) and the imports and renames that get
  around that rule (`use tracing::{warn, …}`, `use tracing as t`,
  `extern crate tracing as t`, a dependency renamed from `tracing`);
- `#[instrument]`, however it's spelled (`::tracing::instrument`, inside
  `cfg_attr`, imported under another name), without `skip_all`, or with
  `err` or `ret`, which record the error or the return value;
- any `%` / `?` error field (`%e`, `?err`, `error = %e`, `reason = %e`), by
  syntax, whatever the type: errors reach a line only through
  `lodestar_log::Error`.

NOT_YET_SWEPT names the files whose plain macros the follow-up sweep
(B-log1b) converts; the other rules apply to them already. It may only
shrink: a listed file that no longer has a plain macro, or no longer
exists, fails the check, so converting a file means taking it off.

Usage: python3 .github/scripts/log_lint.py [--base REF] [repo root]

`--base REF` also checks that NOT_YET_SWEPT and EXEMPT name nothing REF's
copy doesn't (CI passes the pull request's base branch), so neither can
grow in a pull request. A base whose copy can't be read fails the check.

Manifests are parsed with `tomllib`: every dependency table outside
dev-dependencies (`[target.<cfg>]` and inline tables included) is walked
for the `log` crate and a renamed `tracing`; one that doesn't parse fails.
"""

from __future__ import annotations

import ast
import re
import subprocess
import sys
import tomllib
from pathlib import Path

# Files whose plain `tracing` lines B-log1b converts (open PRs touched them
# when B-log1 ran). Remove a file when it's converted; never add one.
NOT_YET_SWEPT = [
    "crates/filter/src/lists.rs",
    "crates/lodestar/src/fixture.rs",
    "crates/lodestar/src/insights.rs",
    "crates/lodestar/src/main.rs",
    "crates/lodestar/src/services.rs",
    "crates/lodestar/src/system/address.rs",
    "crates/lodestar/src/system/restart.rs",
    "crates/lodestar/src/windows/ports.rs",
    "crates/lodestar/src/windows/priority.rs",
    "crates/lodestar/src/windows/service.rs",
    "crates/pipeline/src/control.rs",
    "crates/pipeline/src/dht.rs",
    "crates/pipeline/src/lib.rs",
    "crates/pipeline/src/worker.rs",
    "crates/pipeline/src/writer.rs",
    "crates/search/src/index.rs",
    "crates/search/src/live.rs",
    "crates/search/src/rebuild.rs",
]

# Narrow, named exemptions: (path, rule) and why.
EXEMPT = {
    ("crates/log/src/dispatch.rs", "tracing"): "the fallback without a sink forwards to tracing",
}

MACRO_NAMES = r"trace|debug|info|warn|error|event|span|trace_span|debug_span|info_span|warn_span|error_span"
# Bare, or through any path with a `tracing` segment: `tracing::warn!`,
# `crate::tracing::warn!` (a re-export), `tracing::log::warn!`.
LOG_MACROS = re.compile(
    r"(?:(?<![\w:.])(?:::)?(?:\w+\s*::\s*)*?tracing\s*::\s*(?:\w+\s*::\s*)*?|(?<![\w:.]))"
    rf"({MACRO_NAMES})"
    r"\s*!\s*[(\[{]"
)
EXTERN_RENAME = re.compile(r"\bextern\s+crate\s+tracing\s+as\b")
# What a `use` may not reach through `tracing`: a macro, `instrument`,
# `log` (its `log` macros) or everything.
IMPORTED = set(MACRO_NAMES.split("|")) | {"instrument", "log", "*"}
USE_TOKEN = re.compile(r"::|[{},*]|\w+")


def flatten(tokens: list[str], i: int = 0, prefix: tuple = ()) -> tuple[list[tuple[list[str], str | None]], int]:
    """A `use` tree's paths, each with its `as` name: `{tracing::{self as
    t, warn}}` is `[(["tracing"], "t"), (["tracing", "warn"], None)]`."""
    out, segs = [], list(prefix)
    while i < len(tokens):
        t = tokens[i]
        if t == "::":
            i += 1
        elif t == "{":
            i += 1
            while i < len(tokens) and tokens[i] != "}":
                if tokens[i] == ",":
                    i += 1
                    continue
                sub, i = flatten(tokens, i, tuple(segs))
                out += sub
            return out, i + 1
        elif t in (",", "}"):
            break
        elif t == "as":
            return [(segs, tokens[i + 1] if i + 1 < len(tokens) else "_")], i + 2
        elif t == "self" and segs:
            i += 1
        else:
            segs.append(t)
            i += 1
    return [(segs, None)], i


def gets_around(segs: list[str], alias: str | None) -> bool:
    """A `use` path that renames or re-exports `tracing`, or imports its
    macros (from the crate root or a re-export: `crate::tracing::warn`)."""
    for k, seg in enumerate(segs):
        if seg == "tracing":
            break
        if seg not in ("crate", "self", "super"):
            return False
    else:
        return False
    rest = segs[k + 1 :]
    return not rest or rest[0] in IMPORTED
# `#[instrument]`, `#[tracing::instrument]`, `#[::tracing::instrument]`,
# `#[cfg_attr(…, instrument(…))]`.
INSTRUMENT = re.compile(r"(?:^|[\[(,\s:])(?:::)?(?:tracing\s*::\s*)?instrument\b")
INSTRUMENT_RECORDS = re.compile(r"(?:^|[(,\s])(?:err|ret)\b")
ERROR_FIELD = re.compile(
    r"(?:"
    # `error = %x`, `err = ?x`, `cause = %e`
    r"\b(?:error|err|cause|source)\s*=\s*[%?]"
    r"|"
    # `reason = %e`, `detail = ?err`: any name, an error's value
    r"\b\w+\s*=\s*[%?]\s*&?(?:e|err|error|\w+_err|\w+_error|err_\w+|error_\w+)\b"
    r"|"
    # `%e`, `?err`, `%error`, `?io_err`, `%e.kind()` as a bare field
    r"(?:^|[(,{])\s*[%?]\s*&?(?:e|err|error|\w+_err|\w+_error|err_\w+|error_\w+)\b"
    r")"
)


def strip(code: str) -> str:
    """`code` with comments, strings and char literals blanked (newlines
    kept, so line numbers stay right)."""
    out = []
    i, n = 0, len(code)
    while i < n:
        c = code[i]
        if code.startswith("//", i):
            j = code.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif code.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if code.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif code.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(re.sub(r"[^\n]", " ", code[i:j]))
            i = j
        elif c == "r" and re.match(r'r#*"', code[i:]) and (i == 0 or not (code[i - 1].isalnum() or code[i - 1] == "_")):
            hashes = re.match(r"r(#*)\"", code[i:]).group(1)
            end = code.find('"' + hashes, i + 2 + len(hashes))
            j = n if end < 0 else end + 1 + len(hashes)
            out.append(re.sub(r"[^\n]", " ", code[i:j]))
            i = j
        elif c == '"':
            j = i + 1
            while j < n and code[j] != '"':
                j += 2 if code[j] == "\\" else 1
            j = min(j + 1, n)
            out.append(re.sub(r"[^\n]", " ", code[i:j]))
            i = j
        elif c == "'" and re.match(r"'(\\.|[^\\'\n])'", code[i:]):
            m = re.match(r"'(\\.|[^\\'\n])'", code[i:])
            out.append(" " * len(m.group(0)))
            i += len(m.group(0))
        else:
            out.append(c)
            i += 1
    return "".join(out)


def without_tests(code: str) -> str:
    """`code` (already stripped) with every `#[cfg(test)]` item blanked:
    the attribute, then the item up to its closing brace or semicolon."""
    out = list(code)
    for m in re.finditer(r"#\[\s*cfg\s*\(\s*test\s*\)\s*\]", code):
        j = m.end()
        depth = 0
        while j < len(code):
            ch = code[j]
            if ch == ";" and depth == 0:
                j += 1
                break
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    j += 1
                    break
            j += 1
        for k in range(m.start(), min(j, len(code))):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def attributes(code: str):
    """Every `#[…]` / `#![…]` attribute: (start, text)."""
    for m in re.finditer(r"#!?\s*\[", code):
        depth, j = 0, m.end() - 1
        while j < len(code):
            if code[j] == "[":
                depth += 1
            elif code[j] == "]":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        yield m.start(), code[m.start() : j + 1]


def is_test_file(path: Path) -> bool:
    return path.name == "tests.rs" or path.name.endswith("_tests.rs") or "tests" in path.parts


def plain_macros(rel: str, clean: str) -> list[str]:
    line_of = lambda pos: clean.count("\n", 0, pos) + 1
    found = []
    for m in LOG_MACROS.finditer(clean):
        found.append(f"{rel}:{line_of(m.start())}: plain `{m.group(1)}!`: use `log_event!` with a catalogued event")
    for m in re.finditer(r"\buse\b", clean):
        end = clean.find(";", m.end())
        paths, _ = flatten(USE_TOKEN.findall(clean[m.end() : end if end >= 0 else len(clean)]))
        if any(gets_around(segs, alias) for segs, alias in paths):
            found.append(f"{rel}:{line_of(m.start())}: a `tracing` macro import: use `log_event!`")
    for m in EXTERN_RENAME.finditer(clean):
        found.append(f"{rel}:{line_of(m.start())}: a `tracing` macro import: use `log_event!`")
    return found


def check_source(rel: str, code: str, swept: bool = True) -> list[str]:
    """Every finding in one file's source, as `path:line: message`.
    `swept`: the plain-macro rules apply (the file isn't in NOT_YET_SWEPT)."""
    findings = []
    clean = without_tests(strip(code))
    line_of = lambda pos: clean.count("\n", 0, pos) + 1
    if swept and (rel, "tracing") not in EXEMPT:
        findings.extend(plain_macros(rel, clean))
    for start, attr in attributes(clean):
        if not INSTRUMENT.search(attr[attr.index("[") + 1 :]):
            continue
        if "skip_all" not in attr:
            findings.append(f"{rel}:{line_of(start)}: `#[instrument]` without `skip_all` records every argument")
        if INSTRUMENT_RECORDS.search(attr[attr.index("[") + 1 :]):
            findings.append(f"{rel}:{line_of(start)}: `#[instrument(err)]` / `ret` records an error or a return value")
    for m in ERROR_FIELD.finditer(clean):
        findings.append(f"{rel}:{line_of(m.start())}: an untyped error field: use `lodestar_log::Error`")
    return findings


# The dependency tables a manifest can hold outside dev-dependencies, at
# the top or under `[target.<cfg>]` (Cargo also takes the underscored
# spellings).
DEP_TABLES = ("dependencies", "build-dependencies", "build_dependencies")
DEV_TABLES = ("dev-dependencies", "dev_dependencies")


def manifest_paths(root: Path) -> list[Path]:
    """Every first-party manifest: the crates and the bench (linked into
    `serve --fixture`)."""
    return sorted(root.glob("crates/*/Cargo.toml")) + sorted(root.glob("bench/Cargo.toml"))


def workspace_deps(root: Path) -> tuple[dict, list[str]]:
    """The root manifest's `[workspace.dependencies]`, for `x.workspace =
    true` entries, and any problem reading it."""
    p = root / "Cargo.toml"
    if not p.is_file():
        return {}, []
    try:
        doc = tomllib.loads(p.read_text(encoding="utf-8"))
    except (tomllib.TOMLDecodeError, UnicodeDecodeError) as e:
        return {}, [f"Cargo.toml: doesn't parse ({e}): the logging check can't read it"]
    deps = doc.get("workspace", {}).get("dependencies", {})
    return (deps if isinstance(deps, dict) else {}), []


def dep_tables(doc: dict) -> list[tuple[str, dict]]:
    """Every non-dev dependency table in a parsed manifest, named as a
    finding shows it: `[dependencies]`, `[target.'cfg(…)'.build-dependencies]`."""
    out = []
    for name in DEP_TABLES:
        if isinstance(doc.get(name), dict):
            out.append((name, doc[name]))
    targets = doc.get("target", {})
    if isinstance(targets, dict):
        for cfg, table in targets.items():
            if not isinstance(table, dict):
                continue
            for name in DEP_TABLES:
                if isinstance(table.get(name), dict):
                    out.append((f"target.'{cfg}'.{name}", table[name]))
    return out


def package_of(name: str, spec, workspace: dict) -> str:
    """The crate a dependency entry really names: its `package`, or the
    workspace entry's for `x.workspace = true`, else its key."""
    if isinstance(spec, dict):
        if isinstance(spec.get("package"), str):
            return spec["package"]
        if spec.get("workspace") is True:
            return package_of(name, workspace.get(name), {})
    return name


def manifests(root: Path) -> list[str]:
    """A dependency on the `log` crate (its macros are another way round
    `log_event!`), or one renamed from `tracing`, in any first-party
    manifest's dependency tables, `[target.<cfg>]` ones and inline tables
    included. Dev-dependencies serve tests only. A manifest that doesn't
    parse is a finding: the check fails closed."""
    workspace, out = workspace_deps(root)
    for p in manifest_paths(root):
        rel = p.relative_to(root).as_posix()
        try:
            doc = tomllib.loads(p.read_text(encoding="utf-8"))
        except (tomllib.TOMLDecodeError, UnicodeDecodeError) as e:
            out.append(f"{rel}: doesn't parse ({e}): the logging check can't read it")
            continue
        for table, deps in dep_tables(doc):
            for name, spec in deps.items():
                package = package_of(name, spec, workspace)
                if package == "log":
                    out.append(f"{rel}: [{table}] {name}: the `log` crate: use `log_event!`")
                elif package == "tracing" and name != "tracing":
                    out.append(f"{rel}: [{table}] {name}: `tracing` renamed: use `log_event!`")
    return out


def sources(root: Path) -> list[Path]:
    return sorted(
        p
        for p in root.glob("crates/*/src/**/*.rs")
        if not is_test_file(p.relative_to(root))
    )


def run(root: Path, not_yet: list[str] = NOT_YET_SWEPT) -> list[str]:
    problems = manifests(root)
    listed = set(not_yet)
    for entry in not_yet:
        p = root / entry
        if not p.is_file():
            problems.append(f"{entry}: in NOT_YET_SWEPT but missing: take it off the list")
        elif not plain_macros(entry, without_tests(strip(p.read_text(encoding="utf-8")))):
            problems.append(f"{entry}: in NOT_YET_SWEPT but has no plain macro left: take it off the list")
    for p in sources(root):
        rel = p.relative_to(root).as_posix()
        problems.extend(check_source(rel, p.read_text(encoding="utf-8"), swept=rel not in listed))
    return problems


def assigned(source: str, name: str):
    """The literal assigned to `name` at the top of a copy of this script,
    or None when there's no such assignment."""
    for node in ast.parse(source).body:
        if (
            isinstance(node, ast.Assign)
            and any(isinstance(t, ast.Name) and t.id == name for t in node.targets)
        ):
            return ast.literal_eval(node.value)
    return None


def listed_in(source: str) -> list[str] | None:
    """The NOT_YET_SWEPT list in a copy of this script, or None."""
    value = assigned(source, "NOT_YET_SWEPT")
    return None if value is None else list(value)


def exempt_in(source: str) -> set | None:
    """The EXEMPT keys in a copy of this script, or None."""
    value = assigned(source, "EXEMPT")
    return None if not isinstance(value, dict) else set(value)


def grown(base: list[str] | None, now: list[str]) -> list[str]:
    """Entries `now` adds to `base`'s list (none when base has no list)."""
    if base is None:
        return []
    return [f"{e}: added to NOT_YET_SWEPT: the list may only shrink" for e in now if e not in base]


def grown_exempt(base: set | None, now: dict) -> list[str]:
    """Exemptions `now` adds to `base`'s (none when base has no script)."""
    if base is None:
        return []
    return [
        f"{path} ({rule}): added to EXEMPT: exemptions may only shrink"
        for path, rule in sorted(now)
        if (path, rule) not in base
    ]


class BaseUnreadable(Exception):
    """The base has the script, but its lists can't be read."""


def against_base(source: str | None, not_yet: list[str], exempt: dict) -> list[str]:
    """What the lists add to the base's copy of this script (`source`,
    None when the base has none). Fails closed: a copy whose lists can't
    be read is a finding, never a pass."""
    if source is None:
        return []
    try:
        base_not_yet = listed_in(source)
        base_exempt = exempt_in(source)
    except (SyntaxError, ValueError, TypeError) as e:
        return [f"--base: the base's log_lint.py can't be read ({e}): the lists can't be compared"]
    problems = []
    if base_not_yet is None:
        problems.append("--base: the base's log_lint.py has no readable NOT_YET_SWEPT list")
    if base_exempt is None:
        problems.append("--base: the base's log_lint.py has no readable EXEMPT table")
    problems += grown(base_not_yet, not_yet)
    problems += grown_exempt(base_exempt, exempt)
    return problems


def base_source(root: Path, ref: str) -> str | None:
    """The base's copy of this script, or None when it has none. A ref
    that doesn't resolve fails the check rather than passing it."""
    if subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"],
        capture_output=True,
    ).returncode != 0:
        raise BaseUnreadable(f"--base {ref}: no such commit")
    path = f"{ref}:.github/scripts/log_lint.py"
    if subprocess.run(["git", "-C", str(root), "cat-file", "-e", path], capture_output=True).returncode != 0:
        # No such file at the base yet: nothing to compare with.
        return None
    shown = subprocess.run(["git", "-C", str(root), "show", path], capture_output=True, text=True)
    if shown.returncode != 0:
        raise BaseUnreadable(f"--base {ref}: its log_lint.py can't be read")
    return shown.stdout


def main() -> int:
    args = sys.argv[1:]
    base = None
    if args[:1] == ["--base"]:
        base, args = args[1], args[2:]
    root = Path(args[0]) if args else Path(__file__).resolve().parents[2]
    problems = run(root)
    if base is not None:
        try:
            problems.extend(against_base(base_source(root, base), NOT_YET_SWEPT, EXEMPT))
        except BaseUnreadable as e:
            problems.append(str(e))
    for p in problems:
        print(p)
    if problems:
        print(f"log_lint: {len(problems)} problem(s)")
        return 1
    n = len(sources(root))
    print(f"log_lint: ok ({n} files, {len(NOT_YET_SWEPT)} not yet swept)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
