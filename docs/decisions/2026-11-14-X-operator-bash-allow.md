# X-operator-bash-allow decisions (#878)

This changes one OP-4 security decision (`2026-11-11-OP-4.md`: the operator's toolset runs on the `auto` permission policy). The owner decided it on 2026-11-14 at about 08:05Z, asked directly, with the evidence and the alternatives below in front of them: "Allow bash now". It replaces, for `bash` only, what `2026-11-14-X-operator-mount-helpers.md` says about the policy: that it "stays `auto`", and that `bash` is bounded by "the `auto` policy". The plan's "bash on `auto`" (`docs/plan/2026-11-08-cloud-operator.md`, the OP-4 slice) is superseded the same way.

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
- **The policy no longer screens the operator's `bash` calls.** `auto` could have refused a command from an operator led astray by text it read (a comment, command output). That screen is gone for `bash`. It was never a guarantee: `auto` allows what it judges safe. In particular, none of these is screened any more:
  - **Using the App's token against another pull request** on this repository, through any `bash` command that calls `api.github.com`.
  - **Fetching code and running it.** `api.github.com` serves the contents of any public repository, so the download-and-run that `auto` refused in `ping-1` would now run. Code fetched that way can do only what `bash` already can: reach `api.github.com`, with the token.
  - **The prompt's fallback `curl`.** Its three rules are prompt rules only.

## What still holds
- **The sandbox reaches `api.github.com` and nothing else.**
- **The token is the App's installation token:** this repository only, Pull requests write and Metadata read, one hour, revoked at the end of the wake. It can't merge or push. Whether it can update a pull request's branch from its base is still to be verified (first-run check 7).
- **Fleet limits are in code the operator can't change** (`infra/fleet/limits.yaml`, which `fleet.yml` enforces): the allow-list of commands, one fleet, its lifetime and its spend.
- **The tripwire** escalates and disables `operator-wake.yml` when the App's bot acts anywhere but a new comment on the channel. It finds such an act afterwards; it doesn't prevent it. Disabling the workflow does stop a session that's still running.
- **The budgets and rate limits:** $3 or $15 a session, 6 wakes an hour, 60 a day, $200 in total. Each wake still needs the owner's approval.
- **The helper's own rules:** which comments it trusts, and what it will post.
- **The injection test (check 8) runs at this setting.** Its sessions take their tools from the deployed agent, so it measures the operator as it will run. It hasn't been run yet at any setting, and it comes before any fleet is created (`FLEET_LIVE` is still `false`).

## What wasn't chosen
- **Host-side tools, with `bash` off.** The operator's two actions, reading the channel and posting to it, could be custom tools the bridge runs itself. Permission policies don't apply to custom tools, and the bridge could check every post in code. The sandbox couldn't use the App's token at all: today it holds only a placeholder, but any `bash` call to `api.github.com` has the real token put in at the proxy. That's stronger than both `auto` and this decision, but it's a larger change: the bridge's watch loop, the prompt and check 8's detection would all be reworked. The owner chose to allow `bash` now. It stays open as a later hardening slice; the runner plans it if the owner asks.

## After this merges
- **The owner dispatches `operator-apply.yml` once.** The agent's tools changed, so it needs a new version. Until then a wake still runs with `bash` on `auto`, and the injection test refuses to start because the deployed agent differs from `main`.
- **The first-run checklist's "Denied" case** now says that `bash` calls aren't checked, and that a denial can still come from another tool.
- **For the runner:** this owner decision isn't yet in `docs/owner-decisions.md`.
- **Found in review, not fixed here:** the injection test's `check_deployed` compares the deployed agent's tools with `agent.json` exactly. The API reference describes the tools it returns as resolved, with fields `agent.json` leaves out. If that's what the API returns, check 8 refuses to start whatever is deployed. It predates this change and needs one read of the deployed agent to confirm.
