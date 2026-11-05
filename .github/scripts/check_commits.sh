#!/usr/bin/env bash
# Checks commit subjects against Conventional Commits 1.0.0.
#   check_commits.sh <range>        e.g. origin/main..HEAD
#   check_commits.sh --message FILE one message file (used by .githooks/commit-msg)
# Merge commits, `Revert "..."`, `fixup!` and `squash!` subjects are exempt. Commits authored
# before CUTOFF are exempt: history before it is not rewritten
# (docs/decisions/2026-11-05-conventional-commits.md).
set -euo pipefail

CUTOFF="2026-11-06T00:00:00Z"
TYPES="feat|fix|perf|refactor|test|docs|build|ci|chore|revert|style"
RE="^(${TYPES})(\([a-z0-9][a-z0-9._/-]*\))?!?: [^[:space:]].*[^.]$"

fail=0
check() { # sha subject
  local sha=$1 subject=$2
  case $subject in "Merge "* | 'Revert "'* | "fixup! "* | "squash! "*) return 0 ;; esac
  if [[ ${#subject} -gt 72 ]]; then
    echo "::error::${sha}: subject is ${#subject} chars (max 72): ${subject}"; fail=1; return
  fi
  if ! [[ $subject =~ $RE ]]; then
    echo "::error::${sha}: not 'type(scope): description' (types: ${TYPES//|/, }; no trailing period): ${subject}"
    fail=1
  fi
}

if [[ ${1:-} == --message ]]; then
  check "message" "$(head -n1 "$2")"
else
  while IFS='|' read -r sha date subject; do
    [[ $date < $CUTOFF ]] && continue
    check "${sha:0:9}" "$subject"
  done < <(git log --no-merges --format='%H|%aI|%s' "$1" | while IFS='|' read -r s d rest; do
    printf '%s|%s|%s\n' "$s" "$(date -u -d "$d" +%Y-%m-%dT%H:%M:%SZ)" "$rest"; done)
fi
if [[ $fail -ne 0 ]]; then
  echo "See docs/decisions/2026-11-05-conventional-commits.md" >&2
fi
exit $fail
