#!/usr/bin/env bash
# PreToolUse guard: only the build runner merges or writes to main
# (runner/README.md, "Who merges"). The runner creates ~/.lodestar-runner
# when it starts; slice sessions and other sessions don't have it, so their
# merge, auto-merge and push-to-main calls are refused here. Exit 2 blocks
# the call and shows the message to the session. Any other failure lets the
# call through, so a broken hook can't stall the runner. (A bash syntax error
# would exit 2 and block every call; CI runs `bash -n` on this file.)
[ -f "$HOME/.lodestar-runner" ] && exit 0

input=$(cat)
tool=$(printf '%s' "$input" | jq -r '.tool_name // empty' 2>/dev/null) || exit 0

refuse() {
  echo "Blocked by .claude/hooks/merge-guard.sh: $1 Only the build runner merges or writes to main. Push to your own branch, open or update the PR, and tell the runner (send_message to runner_session in state.json on the runner-state branch) that it's ready." >&2
  exit 2
}

case "$tool" in
  mcp__github__merge_pull_request | mcp__github__enable_pr_auto_merge)
    refuse "this session may not merge pull requests." ;;
  mcp__github__push_files | mcp__github__create_or_update_file | mcp__github__delete_file)
    branch=$(printf '%s' "$input" | jq -r '.tool_input.branch // empty' 2>/dev/null)
    case "$branch" in main | refs/heads/main) refuse "this session may not write to main." ;; esac ;;
  Bash)
    cmd=$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)
    # Look only at the push itself (including `git -C <dir> push`), not at
    # other commands chained with it. A bare `git push` while on main, and raw
    # API calls, get through; branch protection on main backs those.
    segs=$(printf '%s\n' "$cmd" | sed -E 's/(&&|\|\||[;|&()])/\n/g' |
      grep -E '(^|[[:space:]])git([[:space:]]+-[^[:space:]]+([[:space:]]+[^-[:space:]][^[:space:]]*)?)*[[:space:]]+push([[:space:]]|$)')
    if printf '%s\n' "$segs" |
       grep -Eq -e '--(all|mirror)([[:space:]=]|$)' -e "([[:space:]:+'\"]|^)(refs/heads/)?main(['\"]|[[:space:]<>]|$)"; then
      refuse "this session may not push to main."
    fi
    if printf '%s\n' "$cmd" | grep -Eq '(^|[[:space:];&|(])gh[[:space:]]+(pr[[:space:]]+merge|api[[:space:]].*/merge)'; then
      refuse "this session may not merge pull requests."
    fi ;;
esac
exit 0
