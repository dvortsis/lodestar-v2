# X-operator-bash-allow decisions (#878)

This changes one OP-4 security decision (`2026-11-11-OP-4.md`: the operator's toolset runs on the `auto` permission policy). The owner decided it on 2026-11-14 at about 08:05Z, asked directly, with the evidence and the alternatives below in front of them: "Allow bash now". It replaces the "permission policy stays `auto`" lines of `2026-11-14-X-operator-mount-helpers.md` for `bash` only.

## The decision
- **`bash` runs on `always_allow`.** The agent's toolset keeps `auto` as its default, with one override for `bash`. A `bash` call now runs without the server's check.
- **Every other tool stays on `auto`,** and the web tools stay off.
- **Nothing else changes:** the bridge, the prompt, the mounted helper, the budgets, the rate limits, the lock and the token's scope are as they were.

## Why
- **On `auto` the operator could not do its job.** Its whole job is to read the channel and post to it with the helper. Three wakes in a row were refused, each with `reason_code: high_risk`:
  - **`ping-1`:** the fetch of the helper and its run, in one call.
  - **`ping-2`:** the copy of the mounted helper and its run, in one call (after `2026-11-14-X-operator-mount-helpers.md`).
  - **`ping-3`:** the runner's instruction named the three steps one by one. The copy was allowed. The next call, `python3 thread.py read` with the arguments from the prompt and nothing else, was denied.
- **So the policy refuses to run the helper itself.** Mounting it instead of downloading it didn't change that, and neither did an instruction that asked for the exact command. No setting short of this one lets the operator read or post through `bash`.
- **A denied operator can't even say so.** Its escalation needs `bash` too. Only the bridge reported the three refusals.

## What it gives up
- **The policy no longer screens the operator's `bash` calls.** `auto` could have refused a command from an operator led astray by text it read (a comment, command output), for example one that used the App's token against another pull request. That screen is gone for `bash`. It was never a guarantee: `auto` allows what it judges safe.

## What still holds
- **The sandbox reaches `api.github.com` and nothing else.**
- **The token is the App's installation token:** this repository only, Pull requests write and Metadata read, one hour, revoked at the end of the wake. It can't merge or push.
- **Fleet limits are in code the operator can't change** (`fleet.yml`): the allow-list of commands, one fleet, its lifetime and its spend.
- **The tripwire** escalates, and can disable `operator-wake.yml`, when the App's bot acts anywhere but a new comment on the channel.
- **The budgets and rate limits:** $3 or $15 a session, 6 wakes an hour, 60 a day, $200 in total. Each wake still needs the owner's approval.
- **The helper's own rules:** which comments it trusts, and what it will post.
- **The injection test (check 8) runs at this setting.** Its sessions take their tools from the deployed agent, so it measures the operator as it will run. It hasn't been run yet at any setting, and it comes before any fleet is created (`FLEET_LIVE` is still `false`).

## What wasn't chosen
- **Host-side tools, with `bash` off.** The operator's two actions, reading the channel and posting to it, could be custom tools the bridge runs itself. Permission policies don't apply to custom tools, the App's token would never enter the sandbox, and the bridge could check every post in code. That's stronger than both `auto` and this decision, but it's a larger change: the bridge's watch loop, the prompt and check 8's detection would all be reworked. The owner chose to allow `bash` now. It stays open as a later hardening slice; the runner plans it if the owner asks.

## After this merges
- **The owner dispatches `operator-apply.yml` once.** The agent's tools changed, so it needs a new version. Until then a wake still runs with `bash` on `auto`, and the injection test refuses to start because the deployed agent differs from `main`.
- **The first-run checklist's "Denied" case** now says that `bash` calls aren't checked, and that a denial can still come from another tool.
- **For the runner:** this owner decision isn't yet in `docs/owner-decisions.md`.
