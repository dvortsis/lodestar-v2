# Handoff: how to pick up Lodestar's build

This page is for whoever continues the build when the current sessions stop: a restarted Claude runner, a single Claude session, or an agent from another vendor. It tells you what to read, where the truth lives, how to find the next piece of work and the rules you can't break. It doesn't repeat the plan; it points to it.

Written 2026-11-02. The plan it points to is [`docs/build-plan.md` §0](build-plan.md#0-status-and-plan-from-2026-11-02).

## 1. Start here
Read these in order before changing anything:
1. **[`CLAUDE.md`](../CLAUDE.md).** The order of authority, working conventions, multi-session rules, model policy, UI rules and environment notes. Every agent follows it, whatever its vendor.
2. **[`docs/owner-decisions.md`](owner-decisions.md).** The owner's own decisions. They outrank everything else. §2.19 and §2.26 say what you may decide yourself and what goes to the owner.
3. **[`docs/build-plan.md` §0](build-plan.md#0-status-and-plan-from-2026-11-02).** Where things stand, the tracks and their order, the owner queue, the gates, the serial points and the expected complications.
4. **[`runner/standards.md`](../runner/standards.md).** The bar for every change and every review.
5. **For the slice you take:** its brief (on `runner-state` under `briefs/<id>.md`, or the plan file that drafts it), the architecture sections it cites, and the newest `docs/decisions/*` files for that area (`ls docs/decisions | grep <area>`).

## 2. Where the truth lives

| What | Where | Trust it for |
|---|---|---|
| Code, docs, decisions, plans | `main` | Everything merged. **The final word on what's done** |
| The slice graph | `runner/slices.yaml` on `main` | Ids, deps, owned paths, `ext:` conditions |
| Live slice state, briefs, runbooks | branch `runner-state`: `state.json`, `briefs/`, `notes/` (v0.5.1 cut runbook, owner steps, delta draft) | What's running, who's on it, review rounds, owner checklist |
| The v0.5.1 release | branch `release/v0.5.1`, tags `v0.5.1-rc.1`, `v0.5.1-rc.2` | Release notes, cut decisions |
| Soak and run evidence | `docs/soak/` (reports, `evidence/`, collector and analyzer in `tools/`) | Measured numbers |
| Decisions after 2026-10-22 | `docs/decisions/<date>-<slice>.md`, one file per PR | Why things are the way they are |

**`state.json` lags.** The runner updates it by hand, and on 2026-11-02 it still showed merged slices as building and a migration order that stops at V11. Before acting on it, reconcile it:
```bash
git fetch origin main runner-state
```
```bash
git log --first-parent --format='%h %ad %s' --date=short origin/main | head -40
```
```bash
git show origin/runner-state:state.json | python3 -c "import json,sys; s=json.load(sys.stdin); print(s['runner_session'], s['updated_at']); print(s.get('queue_note'))"
```
Then list open pull requests: the GitHub UI, the GitHub MCP `list_pull_requests`, or `gh api 'repos/dvortsis/lodestar-v2/pulls?state=open' --jq '.[] | [.number, .head.ref, .title] | @tsv'` (in cloud sessions `gh pr list` fails because GraphQL is blocked). A slice is done when its PR is merged on `main`, whatever `state.json` says.

## 3. Find the next piece of work
1. Read the ladder and track tables in [build-plan §0.2](build-plan.md#02-order-of-work-and-tracks) and the list in [§0.9 Next actions](build-plan.md#09-next-actions).
2. Check the slice in `runner/slices.yaml`. It's **ready** when every id in `deps` is merged and every `ext:<name>` condition in the `external` section holds. Most `ext:` conditions are owner actions (§0.3 of the plan).
3. Check that nobody else has it: no open PR on its branch, no running session for it in `state.json`, and no running slice whose `owns` overlaps its paths.
4. **Check the slot count.** In this phase at most four slices run at once, in total (owner §2.25 as the runner applies it; build-plan §0.2). Count the running sessions and open slice PRs first. When several slices are ready, **the ladder in §0.2 decides**, not graph readiness: a low-ranked slice that happens to be ready waits for a higher-ranked one.
5. If it's an `epic`, it needs expanding into slices first (`runner/README.md` step 8): read the architecture sections and the code as it stands, write the slices into `slices.yaml` in a docs PR, then start them.

## 4. Path A: a Claude runner
If Claude is available again and the runner has stopped:
- Start a new Opus session in **Auto** mode (never Plan) with the prompt *"Act as the Lodestar build runner: follow runner/README.md."*
- [`runner/README.md`](../runner/README.md) has the rest: the marker file `~/.lodestar-runner`, taking over `runner_session` in `state.json`, the hourly task and the watchdog, launching slices with `create_session`, the fresh-Opus review, the freshness rule and the landing lane.
- Slice sessions follow [`.claude/skills/steward/SKILL.md`](../.claude/skills/steward/SKILL.md).
- **First wake after a gap:** reconcile `state.json` against `main` (§2 above) and push the corrected state before launching anything.

## 5. Path B: a single agent without the runner
Use this path for any agent without Claude's cloud sessions, `send_message` or the runner's scheduled tasks, including agents from other vendors.

**Use Path B only when no runner is live:** `runner_session` in `state.json` names a session that's archived or gone, or `updated_at` is more than 6 hours old, and the owner hasn't restarted one. If a runner is live, register with it instead (`runner/README.md`, "Other sessions the runner watches").

**Work serially.** One slice at a time, start to PR. Don't run parallel branches you can't watch.

**Prefer slices you can finish safely.** Start with Track B, docs, plans and well-specified helper work. Take a core slice (store, search, crawler, protocols, parsers of untrusted input, security or content safety) only with your strongest model, and expect it to wait for the review in step 8.

For each slice:
1. `git fetch origin main` and branch `slice/<id>` from `origin/main`. Clones are shallow, so fetch before branching and before pushing.
2. Edit **only** the paths in the slice's `owns`. If a shared file needs a change, say so in the PR description.
3. Record every choice the spec leaves open in a new `docs/decisions/<YYYY-MM-DD>-<id>.md` (see [`docs/decisions/README.md`](decisions/README.md)). Never edit `docs/architecture.md` §16.
4. Run every check in [`CLAUDE.md` "Before pushing"](../CLAUDE.md):
   - Rust: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`;
   - in `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for anything with an e2e spec. Use the installed Chromium (`PW_CHROMIUM_PATH`, see [`web/e2e/README.md`](../web/e2e/README.md)); never run `playwright install`.
5. Review your own diff against [`runner/standards.md` §5](../runner/standards.md), sorting findings into blocking, important and polish. Fix every blocking and important finding. If your tooling can start a second, independent reviewer given only the diff, the brief and the standards, do that; otherwise review with fresh eyes in a separate pass.
6. Run a security review on anything that touches auth, credentials, filters, the cluster, companions, source plugins or untrusted-input parsers.
7. Open a PR to `main` titled `<id>: <title>`. Describe what changed, the checks and their results, the self-review, shared-file edits needed, missing contract fields and deviations from the spec.
8. **Don't merge.** Branch protection on `main` requires a PR and the `ci-ok` check. Every PR still needs **a fresh Opus review** before it merges (CLAUDE.md "Merging", owner §2.22), so a Path B PR waits for a Claude runner or session to review it, unless the owner explicitly waives that for the PR. The owner, or a returning runner, merges. Tell the owner the PR is ready and whether it has had that review.
9. Leave `state.json` alone; it belongs to the runner. Progress lives in the PR and its decisions file, which a returning runner reads. The `external` checks in `runner/slices.yaml` that mention runner-state also accept a merged decisions file or the owner's recorded message, so record owner answers and assignments in your slice's decisions file.

**Model policy, translated.** Use your strongest model for core work: bencode, KRPC, metainfo, the store format and migrations, the DHT crawler, the peer protocol, search, concurrency, security and anything that parses untrusted input. Smaller models may do well-specified, pattern-following work (Svelte screens per §10, packaging, CI, docs, test scaffolding), and their output is reviewed by the strongest model before it's proposed.

**What Path B can't do, and what to do instead:**

| Runner capability | Without it |
|---|---|
| Parallel cloud slices | Serial slices, smallest critical-path first |
| `send_message` between sessions | The PR description and a PR comment for the owner |
| Merging | A fresh Opus review, then the owner merges (or waives the review for that PR) |
| Hourly reconcile and watchdog | Reconcile (§2) at the start of every work session |
| Owner push notifications | List the owner's open items at the top of your report |

## 6. Rules you can't break
Each comes from `CLAUDE.md`, the owner's decisions or the runner's standards.
- **Every PR gets a fresh Opus review before it merges,** unless the owner waives it for that PR (§5 step 8).
- **Nothing goes directly to `main`,** not even a docs fix (owner §2.22). Never rebase or force-push a shared branch.
- **Ask the owner first** before anything that reverses or loosens an owner decision, weakens a security or content-safety decision, or changes a number the owner set. Performance and UX changes to owner-set numbers are the runner's to make under §2.26, recorded and flagged. Everything else you decide goes in a decisions file.
- **Store migrations:** one migration PR at a time, in the queue in [build-plan §0.7](build-plan.md#07-dependencies-and-serial-points). Never renumber a merged migration.
- **Search schema:** one `SCHEMA_VERSION` bump per milestone; M7's is SB-6. Ask it to carry your field instead of bumping.
- **Tests wait for conditions with a deadline of at least 10 s.** A fixed sleep is a blocking review finding.
- **UI:** no hex colours or arbitrary Tailwind values in components; use the tokens (`docs/design-tokens.md`). Spacing uses the §10.2 ramp.
- **Bundle budgets:** fix size-limit failures by code splitting. A changed limit needs a recorded §2.26 decision.
- **Crawler:** never tune away BEP 51 intervals, per-IP limits or the §18 politeness caps. New behaviour goes behind a switch, default off, until a live A/B shows it wins. No live runs on the soak PC.
- **Git:** fetch before pushing (shallow clones). Push once per round of fixes (each push starts ~37 CI jobs). No Git LFS (the proxy blocks it; brand assets are plain files checked by `SHA256SUMS.txt`).
- **History:** CI fails any branch containing the old lineage (root `15620683`, tip `2d5e761d`). Branch only from today's `main`.
- **Commits:** imperative subject ≤ 72 characters, a body that explains why, no model identifiers in commits, PRs or code.
- **Soak evidence:** commit aggregates and hourly snapshots only, with personal paths, machine names, IPs and tokens scrubbed.

## 7. Owner queue and gates
The owner's open decisions are in [build-plan §0.3](build-plan.md#03-owner-queue), ordered by what they unblock. The gates (v0.5.1, the crawler's v0.6 gate, import scale, SRC-G, 1.0) are in [§0.4](build-plan.md#04-v051-gate-and-v052) and [§0.6](build-plan.md#06-gates-added-by-this-plan). When you need an owner answer, ask once with your recommendation and keep working on everything else.

## 8. In flight on 2026-11-02
Superseded by live state as soon as anything moves; check §2 first. Slots at 02:23Z: **4 of 4** (CR-4, F-filter-standin-undo, S-blocklists, openapi-example-size).

| Item | State | Waits on |
|---|---|---|
| CR-4 uTP and IPv6 route backoff (#365) | review fixes verified; merge on green | CI |
| F-filter-standin-undo (#367) | changes requested by the runner's review | its session's push; owner question on adult holds |
| S-blocklists | building | — |
| openapi-example-size | building (launched 02:23Z) | — |
| Unraid soak r3 on rc.2 | baseline ends 2026-11-02 22:16Z | analysis, then the owner's gluetun leg and tag |
| Held epics: E-M6, E-v06-workers, E-v06-companions | held | the soak verdict |
| SRC-1 | not launched; V14 assigned by `docs/decisions/2026-11-02-master-plan-r2.md` | a free slot and no other open migration PR |
| SRC-7 and everything after it | blocked | owner Q2, Q15 |
