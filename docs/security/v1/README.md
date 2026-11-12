# The 1.0 security review: surfaces, method, severity and held findings

Before 1.0 ships, the whole system gets a security review, one surface at a time (`M8-SEC-1` to `M8-SEC-8`), and the release candidate gets a delta review (`G-v1-security`). This page is the method every one of those reviews follows. It sets no security policy of its own: the policies are in `docs/architecture.md`, `docs/owner-decisions.md` and `docs/decisions/`, and a review checks the code against them.

**Authority:** owner-decisions §2.47 (M8 OQ6: private vulnerability reporting, draft advisories, fix PRs without a proof of concept) and §1.3 item 1 (the repository is public); `docs/plan/2026-11-06-m8-hardening.md` §4.3 and §4.4; `runner/standards.md` §5; the Legal Notice §9 for reports that aren't about the code. The public reporting policy is [`SECURITY.md`](../../../SECURITY.md).

## 1. The eight surfaces
Each surface is one read-only review slice and writes one report in this folder. A finding is fixed in its own `X-*` slice, never in the review slice (§6).

| # | Slice | Report | Surface | Main spec |
|---|---|---|---|---|
| 1 | M8-SEC-1 | `1-network.md` | Network parsers and the crawler: bencode, KRPC, metainfo, the peer protocol (BEP 3/9/10, MSE), DHT routing and its limits; every fuzz target run for at least an hour | architecture §5, §6, §17 |
| 2 | M8-SEC-2 | `2-api-search-web.md` | The API, auth, sessions and the setup token; outputs (Torznab, RSS, export, SSE); the query language, tokenizer and release-name parser; query cost (regex and trigram prefilters, deep pagination, facet sampling, typeahead) as denial of service; the web UI and its headers | §8, §9, §10, §18.6 |
| 3 | M8-SEC-3 | `3-sources.md` | Sources and feeds (Nostr and Lemmy included), importers, plugin egress, source credentials, consent | §18 |
| 4 | M8-SEC-4 | `4-content-safety.md` | The filter engine, the child-safety core and its locked floor, packs, classify, enrichment, and every path that weakens protection | §20, owner-decisions §1.8 |
| 5 | M8-SEC-5 | `5-update-ci-supply-chain.md` | The update engine, signing and the key chain; every workflow's permissions and secrets; the self-hosted runner; `status-connector-deploy`; action pinning and dependencies | §12 |
| 6 | M8-SEC-6 | `6-cluster.md` | The cluster (CA, mTLS, join tokens, leases, fencing, revocation), worker mode, the companion manager | §18.10, §19 |
| 7 | M8-SEC-7 | `7-integrations-privacy.md` | Download-client and other integrations, log redaction, metrics, privacy (no telemetry, torrent details off by default) | §8, §18.9 |
| 8 | M8-SEC-8 | `8-store-host.md` | The store and the host (files, permissions, snapshots, rollback, Windows service), packaging, every `unsafe` block | §7, §11, owner-decisions §2.17 |

A finding that belongs to another surface is still recorded, in the report of the review that found it, with a note naming the surface it belongs to. Nothing is dropped because it's out of scope.

## 2. How a review runs
1. **Pin the head.** The review names the commit of `main` it reads, in full, at the top of its report. Every finding refers to that commit.
2. **A fresh reviewer.** A fresh Opus reviewer reads the surface's code at that head. It is given the code and the spec sections, not anyone's opinion of the code.
3. **Read-only.** The review changes no code, test or workflow. Its only output is its report and the held details (§5).
4. **The threat model first.** Before reading for bugs, the report writes down for its surface:
   - **assets:** what an attacker would want (the admin account, sessions, API keys, source and cluster credentials, the signing keys, the user's data and search history, the protection the child-safety core gives, the host itself);
   - **attackers:** who can send it input (a DHT node or peer on the internet; a client on the LAN, with or without a login; a source, feed, tracker or relay the user added; a compromised worker or companion; a malicious pull request or dependency; a local user on the host without Lodestar's privileges);
   - **entry points:** every place that input arrives;
   - **trust boundaries:** where input crosses from one of those attackers into code that trusts it.
5. **Then the code.** Read each entry point by hand against the threat model: that read is the review. `/security-review` reads a branch's diff, so as a supplementary pass give it the surface's paths as an explicit target, or run it on a scratch branch (never pushed) whose diff adds the surface's files to an empty base. At each one check at least:
   - input is bounded (sizes, counts, depth, time), and no input can cause a panic, an unbounded allocation or unbounded CPU;
   - authentication and authorization are checked on every path, including errors and redirects;
   - secrets are never logged, returned or stored in plain text where the spec says sealed or encrypted;
   - failures close safely: a filter that can't load hides, it doesn't show; a signature that can't be checked means no install;
   - every `unsafe` block has a stated invariant that holds.
6. **Fuzzing** (M8-SEC-1, and any other surface with a fuzz target): run the targets for the time the slice gives, and record each target's run time, executions and result. A crash is a finding.
7. **Re-run the checks.** If the review relies on a test passing, it runs that test itself at the pinned head.

## 3. Severity
Severity follows `runner/standards.md` §5, read for security. When a finding could fit two levels, it takes the higher one.

- **Blocking:** an attacker from the threat model (§2 step 4) can break a guarantee the spec makes. Examples: getting past authentication or the setup token; reading or changing data or settings without permission; running code; reaching a host or network the policy forbids; getting past the child-safety core, its locked floor or the weakening guardrails; a secret leaking; an unsigned update being installed; a crash, hang or memory or CPU blow-up that untrusted input can trigger. 
- **Important:** a weakness that needs a precondition the threat model doesn't grant the attacker today, a missing layer of defence in depth, or a gap that later slices would inherit. Example: a bound that holds only because a caller happens to enforce it.
- **Polish:** hardening with no plausible way to exploit it. A reviewer who sees a way to exploit a polish finding rates it higher instead.

Any finding in the child-safety core is at least important. Important security findings are treated like blocking ones everywhere that matters: they are held (§5), get their own fix slice, and block `R-v1-cut` and `G-v1-security` (§6). "Important" never means "may wait".

A finding can't be downgraded, closed as "won't fix" or accepted as a risk by a session or by the runner, if it is blocking or important. Accepting one loosens a security or content-safety decision, so only the owner can (CLAUDE.md).

## 4. Finding ids
Each finding gets an id `S<surface>-<nn>`, numbered in order within its surface: `S2-01`, `S2-02`. Findings from the RC delta review use `SD-<nn>`; findings from a private vulnerability report use `SVR-<nn>`. The id never changes and is never reused, so the public record and the private advisory always match.

## 5. Holding unfixed findings
**The rule:** the detail of a blocking or important finding is held outside the public repository until a release with its fix is published (owner-decisions §2.47). Merged on `main` but not yet released still counts as unfixed: everyone running the last release is still exposed.

"Detail" means anything that helps someone find or use the flaw: the file, function or line, the input that triggers it, the steps, a proof of concept, a log or a crash dump. The id, the severity, the surface and a status are not detail.

The fix's own code change necessarily shows where the flaw was; that is the one exception. Everything else about the fix (its title, body, commits, test names and decisions file) says only what the code now does.

**Never put the detail in:**
- any file on any branch of this repository, `runner-state` and its briefs included;
- a commit message, pull request title or body, review, or comment;
- an issue, discussion, or CI log or artifact;
- a slice id or title in `runner/slices.yaml`, or `runner/dashboard.yaml`;
- an Artifact page, a shared document or any other outside service.

**Where it goes:**
1. **The reviewer** sends each held finding's full detail to the runner by `send_message`, and writes only the id, severity and surface in the report.
2. **The runner** gives the detail to the owner in the runner's own conversation and adds an owner-queue item that names only the ids.
3. **The owner** opens a draft security advisory on the repository for each one and pastes the detail in. A draft advisory is private; Claude sessions here can't create one. A private vulnerability report already arrives as a draft advisory.
4. **The fix slice:** the runner adds an `X-*` slice whose id and title name the finding's id and the component, not the flaw (for example `X-sec-S2-01: search fix (S2-01)`). The brief on `runner-state` holds the same; the runner sends the detail to the fix session by `send_message`.
5. **The fix PR** names the finding's id and severity, and describes what the code now does, not how the old code could be exploited. Its tests assert the invariant in general terms (bounds, property tests, authorization on every path) without the specific input that triggered the flaw, because that input is a proof of concept. The exact-input regression test lands in a follow-up PR after the release, tracked by the finding's id. The PR's decisions file follows the same rule. (Whether the exact-input test may ship in the fix PR instead is a question for the owner; until answered, this strict default holds.)
6. **After the release with the fix is published,** the owner publishes the advisory. A disclosure slice the runner adds after each release (for example `X-sec-disclose-v1`, owning the rows in `docs/security/v1/*.md` and the follow-up regression tests) moves each released finding from `held` to `fixed in <version>`, writes it in full, and lands its exact-input test. Delta-review findings (`SD-*`) are recorded where `G-v1-security` writes, in `docs/gates/v1/security/`.

**Polish findings** aren't held. They are written in full in the report and go to the backlog (plan §4.4).

**Content-safety findings stay partly held even after the fix.** Their full record describes the class of flaw and what the fix does. It never lists evasion terms, names or patterns beyond what the shipped rules already publish, and never uses real suspected illegal material: examples are synthetic.

## 6. From finding to fix
- Each blocking or important finding becomes an `X-*` fix slice the runner adds, built and reviewed by Opus (plan §4.4). A fix slice never lands inside a review slice.
- `R-v1-cut` waits until every blocking and important finding is fixed on `main`. `G-v1-security` requires zero open ones on the RC.
- A fix after the RC follows the plan's re-run rule (RQ4): it lands on `main` and is cherry-picked to `release/v1.0`; the runner maps its delta, and `G-v1-security` reviews it.

## 7. The report
Each review writes `docs/security/v1/<n>-<surface>.md` with these sections, in this order:

1. **Head:** the full commit reviewed, the date (UTC) and the slice.
2. **Threat model:** assets, attackers, entry points and trust boundaries (§2 step 4).
3. **What was checked:** each entry point and how; `/security-review`'s result; fuzzing, if any.
4. **Findings,** by severity, blocking first:

   | Id | Severity | Status | Summary |
   |---|---|---|---|
   | S2-01 | blocking | held | held until a release with the fix (§5) |
   | S2-02 | important | fixed in v1.0.0 | (full summary, once released) |
   | S2-03 | polish | backlog | (full summary) |

   Status is one of `held`, `fixed on main, held`, `fixed in <version>`, or `backlog` for polish.
5. **Cross-surface notes:** findings that belong to another surface (§1).

## 8. Outside reports
A report through the repository's **Report a vulnerability** button follows the same path from step 3 of §5: it is already a draft advisory; once the owner forwards it, the runner assigns the next `SVR-<nn>` id; it is rated by §3; and it is fixed by an `X-*` slice under the same rules.
