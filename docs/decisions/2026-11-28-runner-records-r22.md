# runner-records-r22 decisions (#1164)

This file records the owner's answers of 2026-11-28 (owner-decisions §2.88) and the runner's calls since r21 (#1160). The rulings are the runner's, under §2.26 and its reading of §2.85–§2.88, except where they apply §2.88. A ruling that only grants owns or records a slice isn't flagged. The owner can veto any flagged call.

## The owner's answers (§2.88)
- **Item 29, no:** the outside scheduler file leaves the repository.
  - What stays: its SHA-256 and the review's findings, in `docs/reviews/dht-survey-evidence/verification.md`.
  - What changes: `verify-proposal.ps1` now requires `-ProposalAttachment`, and it checks both digests.
  - What doesn't: the file stays in git history and in #970. Purging it needs a history rewrite, which is the owner's call.
- **Items 31 and 33, yes:** these become the slice X-testvm-iam-volumes. It edits the policy file under `/security-review`, and the owner applies the reviewed policy in AWS.
- **Item 34, no:** the 4 s uTP first window holds.
- **Item 39, yes:** `HANDOVER.md` is deleted from `runner-state`. With it gone, no Path B holder is recorded (`docs/handoff.md`), which matches the runner holding the build since §2.84.
- **Item 40, yes:** CDLA-Permissive-2.0 joins `web/licenses/allow.json` in the PR that first bundles it. Nothing bundles it today.
- **The Hyper-V VM:** approved for finding problems only. The scope, which is the runner's, is in X-test-vm's inbox on `runner-state`. Its first use is an unmeasured Aggressive (A) screen while SSM blocks in-instance commands on AWS.
- **The dashboard:** items 29, 31, 33, 34, 39 and 40 leave the owner queue. Item 43 lists the flags below.

## Rulings since r21
- **CMP-G (#1161)**
  - `companions-smoke.yml` stays out of `ci-ok`. It's an advisory check that skips merges to `main`. CI's test job already runs the gate on both OSes.
  - The real-companion smoke (the Q20 input) is a separate slice, X-cmp-real-smoke. It waits on CMP-13's catalogue pins.
  - A Linux test job failed in `crates/lodestar/tests/watch.rs`, waiting for search, while `main` was green. The runner re-ran it once (§2.18). The fix is X-watch-search-wait.
- **X-cmp-privilege-refusal-ci (#1163)**
  - The refusal test existed only for Unix and asserted only when run as root, so CI never asserted it. The slice adds an assertion on Windows CI, where runners are administrators.
  - It also adds one `ci.yml` step that runs that test under `sudo` on Linux, after "privilege drop as root". That step is the runner's grant, and the only `ci.yml` change.
- **X-vpn-bind: the runner accepted the session's step-0 proposal.**
  - **The split:** this slice does the binding, the pause and resume, the setting and the API status. X-vpn-bind-ui adds the settings field and "VPN down" in the UI.
  - **The setting** is restart-required, like encryption.
  - **A new dependency:** `if-addrs`, Windows-only, with an exact pin.
  - **The guard is the bound socket**, not the 2 s probe.
  - **uTP** travels on the bound DHT sockets, so `crates/utp` doesn't change.
  - **DNS fails closed too (the runner's call, under §2.85's leak-protection rule).** With a crawl interface set, hostname lookups (DHT bootstrap, trackers) must not reach the system resolver outside the interface. The runner first accepted that as a stated limit, then withdrew it. The design is X-vpn-bind's step-0 follow-up, with a test that no lookup reaches the system resolver.
  - **Shared files with UX-start (#1152):** `dto/ops.rs`, `openapi.json` and `schema.d.ts`. Whichever PR lands second regenerates them.
- **X-testvm-delete-on-termination (#1162)** was granted its PowerShell test file for one assertion (recorded in r21).
- **The API lane's round 2 is closed.** The lane coordinator reported its credit used up, after 10 lane PRs merged.
  - #1145 X-facet-exclude-fill is finished in-house with the one-line size-limit change the runner granted on 2026-11-27.
  - `reserved_for_api_lane` is closed in `runner-state`, and its unmerged ids return to the runner's queue.
- **The laptop's Windows-account route is declined.** CI covers the Windows checks, the laptop can't give steady runs, and App Control blocks unsigned builds. The Hyper-V VM is the route the owner approved.

## Flags for the owner (§2.19)
1. **Remote workers** (`lodestar worker`) keep today's routes until X-vpn-bind-worker lands. The built-in worker inside the main process is covered (X-vpn-bind).
2. **The VPN setting is restart-required,** not applied live (X-vpn-bind).
3. **CMP-G's gate** runs the test companion with `allow_privileged: true`, because Windows CI runners are administrators. The product's refusal is asserted separately (X-cmp-privilege-refusal-ci). `allow_privileged` is ignored outside test builds.
4. **The companion smoke workflow** is advisory, not part of `ci-ok` (CMP-G).
