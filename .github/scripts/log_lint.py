#!/usr/bin/env python3
"""The logging check (architecture §12.1, B-log1).

First-party code logs only through `log_event!`, whose fields are typed
and declared (the macro refuses undeclared fields and wrong kinds at
compile time). In the paths listed in ENFORCED, outside tests, this
refuses:

- plain `tracing` macros (`trace!` … `error!`, `event!`) and span macros
  used as log lines;
- `#[instrument]` without `skip_all` (a `skip(..)` list, or `fields(x)`
  alone, still records every argument a later change adds);
- any `%` / `?` error field (`%e`, `?err`, `error = %e`), by syntax,
  whatever the type: errors reach a line only through `lodestar_log::Error`.

ENFORCED is explicit: the fully swept crates and the swept files of the
partly swept ones, so a PR that merges later, or a file the follow-up sweep
(B-log1b) converts, doesn't go red. B-log1b widens it to every first-party
crate. A listed path that doesn't exist fails the check, so the list can't
silently rot.

Usage: python3 .github/scripts/log_lint.py [repo root]
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# Directories hold every `.rs` file below them.
ENFORCED = [
    "crates/log/src/",
    "crates/integrations/src/",
    "crates/lodestar/src/logging/",
    "crates/lodestar/src/floor.rs",
    "crates/lodestar/src/fsdetect.rs",
    "crates/lodestar/src/privdrop.rs",
    "crates/lodestar/src/subscriptions.rs",
    "crates/lodestar/src/settings/mod.rs",
    "crates/lodestar/src/settings/crawler.rs",
    "crates/lodestar/src/settings/general.rs",
    "crates/lodestar/src/settings/log.rs",
    "crates/lodestar/src/settings/logging.rs",
    "crates/api/src/error.rs",
    "crates/api/src/filters.rs",
    "crates/api/src/log_events.rs",
    "crates/api/src/logs.rs",
    "crates/api/src/dto/logs.rs",
]

# Narrow, named exemptions: (path, rule) and why.
EXEMPT = {
    ("crates/log/src/dispatch.rs", "tracing"): "the fallback without a sink forwards to tracing",
}

LOG_MACROS = re.compile(
    r"(?:(?<![\w:.])tracing::|(?<![\w:.]))"
    r"(trace|debug|info|warn|error|event|span|trace_span|debug_span|info_span|warn_span|error_span)"
    r"!\s*[(\[{]"
)
INSTRUMENT = re.compile(r"#\[\s*(?:tracing::)?instrument\b")
ERROR_FIELD = re.compile(
    r"(?:"
    # `error = %x`, `err = ?x`, `cause = %e`
    r"\b(?:error|err|cause|source)\s*=\s*[%?]"
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
        # The item ends at its first `;` before any `{`, or at the brace
        # that closes its first `{`.
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


def is_test_file(path: Path) -> bool:
    return path.name == "tests.rs" or path.name.endswith("_tests.rs") or "tests" in path.parts


def check_source(rel: str, code: str) -> list[str]:
    """Every finding in one file's source, as `path:line: message`."""
    findings = []
    clean = without_tests(strip(code))
    line_of = lambda pos: clean.count("\n", 0, pos) + 1
    if (rel, "tracing") not in EXEMPT:
        for m in LOG_MACROS.finditer(clean):
            # `log_event!` and other `*_event!` / `*_span!` names aren't
            # tracing's: the lookbehind keeps word characters out.
            findings.append(f"{rel}:{line_of(m.start())}: plain `{m.group(1)}!`: use `log_event!` with a catalogued event")
    for m in INSTRUMENT.finditer(clean):
        end = clean.find("]", m.end())
        depth, j = 0, m.end()
        while j < len(clean):
            if clean[j] == "(":
                depth += 1
            elif clean[j] == ")":
                depth -= 1
            elif clean[j] == "]" and depth <= 0:
                end = j
                break
            j += 1
        attr = clean[m.start() : end + 1]
        if "skip_all" not in attr:
            findings.append(f"{rel}:{line_of(m.start())}: `#[instrument]` without `skip_all` records every argument")
    for m in ERROR_FIELD.finditer(clean):
        findings.append(f"{rel}:{line_of(m.start())}: an untyped error field: use `lodestar_log::Error`")
    return findings


def files(root: Path, listed: list[str]) -> tuple[list[Path], list[str]]:
    found, missing = [], []
    for entry in listed:
        p = root / entry
        if entry.endswith("/"):
            if not p.is_dir():
                missing.append(entry)
                continue
            found.extend(sorted(x for x in p.rglob("*.rs") if not is_test_file(x.relative_to(root))))
        elif p.is_file():
            found.append(p)
        else:
            missing.append(entry)
    return found, missing


def run(root: Path, listed: list[str] = ENFORCED) -> list[str]:
    paths, missing = files(root, listed)
    problems = [f"{m}: listed in log_lint.py's ENFORCED but missing" for m in missing]
    for p in paths:
        rel = p.relative_to(root).as_posix()
        problems.extend(check_source(rel, p.read_text(encoding="utf-8")))
    return problems


def main() -> int:
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[2]
    problems = run(root)
    for p in problems:
        print(p)
    if problems:
        print(f"log_lint: {len(problems)} problem(s)")
        return 1
    print(f"log_lint: ok ({len(files(root, ENFORCED)[0])} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
