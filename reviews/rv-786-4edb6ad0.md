VERDICT: MERGE

PR #786 (M8-SEC-0), head 4edb6ad02d67b786570f97c98eebb1691b74d9f6. Code review plus /security-review, fresh Opus reviewer.

## Scope and checks
- Diff against origin/main: exactly the three owned paths (`SECURITY.md`, `docs/security/v1/README.md`, `docs/decisions/2026-11-12-M8-SEC-0.md`), 169 additions. **No shared-file edits.** The merge commit from main adds nothing to the diff.
- Docs only: no crate or `web/` change, so clippy, cargo test and the web checks don't apply. `cargo fmt --check` passes. CI on the head: `ci-ok`, `web-ok`, `e2e-ok` all success (the jobs under them are path-skipped).
- Hygiene: no 12-digit numbers, no model identifiers in the files, no personal details beyond the public repository URL (already used in the Legal Notice), no local times.
- Brief and plan §4.3 "Do" list covered: the owner's contact (private vulnerability reporting only, no email, owner-decisions §2.47 OQ6), the eight surfaces matching every `runner/slices.yaml` M8-SEC-1..8 title and spec column, severity from `runner/standards.md` §5, and the held-detail rule. Citations checked: §2.47 OQ6, §1.3 item 1 (public repo), plan §4.3/§4.4, Legal Notice §9 and its CyberTipline/synthetic-example wording, and the `#3-severity` anchor and `../../../SECURITY.md` link.
- Security: nothing loosens a security or content-safety decision. Every refinement is the same as §2.47 or stricter: held until release rather than merge, `runner-state` included, content-safety records partly held for good, any child-safety-core finding at least important, and only the owner can downgrade or accept blocking/important findings.
- **The owner's open question on exact-input tests:** the PR keeps the strict default (the fix PR ships general invariant tests, and the exact-input test lands in a post-release disclosure slice). It records the question in the decisions file (bullet 3) and README §5 step 5, and says that default holds until the owner answers. Not blocking, as asked.
- "(PR pending)" heading: decided under I3 below.

## Blocking
None.

## Important (cheap; fix in this PR per standards §5)
- **I1. Fuzz crashes in public CI disclose themselves, and the method doesn't say so.** `docs/security/v1/README.md:38` (§2 step 6) and `:62-67` (§5 "Never put the detail in"). `.github/workflows/ci.yml:417-422` runs `cargo fuzz` on PRs and uploads `fuzz/artifacts/<target>` as a public Actions artifact. libFuzzer also prints the crashing input in the job log. So a crash that M8-CI's weekly run or a PR's fuzz job finds is published at once, and the "CI log or artifact" ban can't be kept as written. M8-SEC-1's hour-long runs (and M8-CI, which comes later) inherit the gap. Add three points:
  - M8-SEC-1's long runs happen in the review session, and crash inputs never leave it except by `send_message`;
  - a crash input is never added to `fuzz/seeds/` or a corpus in the fix PR, because it is the exact input;
  - a crash first seen in public CI counts as already disclosed: the runner tells the owner at once and fixes it first.
- **I2. A fix slice's `owns` can name the flawed file.** `docs/security/v1/README.md:58` defines "the file" as detail. `:66` bans detail only from a slice's *id or title* in `runner/slices.yaml`, and `:73` (step 4) covers only id and title too. The entry's `owns` list (and the brief's owned paths) are public as soon as the runner commits them, often days before the fix PR. Say that a fix slice's `owns` and brief name paths at component or crate granularity (for example `crates/<crate>/**`), never the single file or function.
- **I3. The decisions heading should name the PR now.** `docs/decisions/2026-11-12-M8-SEC-0.md:1` reads `# M8-SEC-0 decisions (PR pending)`. `docs/decisions/README.md` asks for the slice and the PR, and the number is now known: change it to `# M8-SEC-0 decisions (#786)`. Several merged files on main have no PR number, so this doesn't block. The review decides it: fix it in this round.

## Optional
- **O1.** `docs/security/v1/README.md:24` (§2 step 1) says the review pins "the commit of `main`". `G-v1-security`'s delta review reads the RC on `release/v1.0`, so "the commit of `main` (or of `release/v1.0` for the RC delta review)" would be exact.
- **O2.** `docs/security/v1/README.md:50` and `:100`: only the owner can close a blocking or important finding, but the status list has no value for one the owner closes as not a flaw or a duplicate. Without one, a false positive stays `held` and blocks `R-v1-cut` with no way to record the owner's answer. Consider an `invalid (owner)` status.
- **O3.** `SECURITY.md:20-25`: the in-scope list leaves out the deployed `tools/status-connector` service, which M8-SEC-5 does review (`status-connector-deploy`). Either list it, or say the list is examples.
- **O4.** `docs/security/v1/README.md:44` has trailing whitespace at the end of the line.
- **O5.** README §5: a fix branch is public from its first push, and its code shows where the flaw is (the stated exception). A line asking the runner to schedule held fixes close to a release would shorten that window. This is advice for the runner, not a rule this PR must set.
