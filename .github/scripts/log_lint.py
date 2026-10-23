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

Usage: python3 .github/scripts/log_lint.py [repo root]
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# Files whose plain `tracing` lines B-log1b converts (open PRs touched them
# when B-log1 ran). Remove a file when it's converted; never add one.
NOT_YET_SWEPT = [
    "crates/api/src/auth.rs",
    "crates/api/src/auth/guard.rs",
    "crates/api/src/auth/http.rs",
    "crates/filter/src/lists.rs",
    "crates/lodestar/src/fixture.rs",
    "crates/lodestar/src/insights.rs",
    "crates/lodestar/src/main.rs",
    "crates/lodestar/src/services.rs",
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
LOG_MACROS = re.compile(
    r"(?:(?<![\w:.])(?:::)?tracing\s*::\s*|(?<![\w:.]))"
    rf"({MACRO_NAMES})"
    r"\s*!\s*[(\[{]"
)
# `use tracing::{warn, …}`, `use tracing::warn`, `use tracing as t`,
# `use ::tracing::…`, `use tracing::*`, `extern crate tracing as t`.
TRACING_IMPORT = re.compile(
    r"\buse\s+(?:::)?tracing\s*(?:\bas\b"
    rf"|::\s*\{{[^}}]*\b(?:{MACRO_NAMES}|instrument)\b"
    rf"|::\s*(?:{MACRO_NAMES}|instrument)\b"
    r"|::\s*\*)"
    r"|\bextern\s+crate\s+tracing\s+as\b"
)
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
RENAMED_DEP = re.compile(r"""\bpackage\s*=\s*["']tracing["']""")


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
    for m in TRACING_IMPORT.finditer(clean):
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


def manifests(root: Path) -> list[str]:
    """A dependency renamed from `tracing` in any crate's manifest."""
    out = []
    for p in sorted(root.glob("crates/*/Cargo.toml")):
        for i, line in enumerate(p.read_text(encoding="utf-8").splitlines(), 1):
            if RENAMED_DEP.search(line.split("#", 1)[0]):
                out.append(f"{p.relative_to(root).as_posix()}:{i}: `tracing` renamed: use `log_event!`")
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


def main() -> int:
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[2]
    problems = run(root)
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
