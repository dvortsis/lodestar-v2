# Plan and throughput review (owner request, 2026-11-28)

The owner asked for "an ongoing review to ensure we're proceeding as quickly as possible, without
sacrificing quality". They then added that it "should continually reassess the plan and update as
inefficiencies are identified or improvements in strategy are identified, always keeping the end
goal in mind, along with app principals."

The runner runs it every 6 h (the scheduled task "Lodestar throughput review", into the runner
session). There are four parts: speed signals, the plan, the principles check, and the dashboard's plan
and estimates (added by the owner, 2026-11-28: "have the recurring review also update the roadmap
status dashboard's plan and estimates"). Each run acts
on what it finds, then posts a summary of at most 6 lines.

## The end goal
An excellent 1.0, out as quickly as possible with high throughput (`docs/owner-decisions.md`
§2.88). That means:
- every 1.0 gate green on the release candidate (architecture §17;
  `docs/plan/2026-11-06-m8-hardening.md`);
- the scope freeze in `docs/plan/2026-11-26-fastest-1.0-proposal.md` §3;
- better than bitmagnet on every §4.8 row except crawl rate.

## The app's principles (owner, `docs/owner-decisions.md` §1)
- **Priorities, in order:** performance, database size, first-class UX (§1.1.4). Search is
  "lightning-fast" and pinpoints ambiguous names (§1.1.2). Grab as much metadata as possible
  (§1.1.3).
- **Smallest, fastest UI:** Svelte 5 with the size budgets. Tokens, Riverstone, Atkinson
  Hyperlegible (CLAUDE.md UI rules).
- **Offline by default.** Enrichment is opt-in (§1.2.3). Privacy, consent and the content-safety
  rules are never weakened. The VPN binding fails closed.
- **Written for the moderate user:** safe defaults and plain help (§2.14).
- **Quality bar:** a fresh review per PR, `/security-review` where required, no skipped tests, no
  fixed sleeps, fuzz and property tests for parsers (CLAUDE.md, `runner/standards.md`).

A strategy change that trades any of these for speed is not an improvement. A change that loosens
an owner decision, a gate, an owner-set number or a security rule goes to the owner first, with a
recommendation.

## 1. Speed signals (`tools/throughput.py 24 state.json`)
| Signal | Act when | Action |
|---|---|---|
| Slots active | fewer than `phase_total` and a ready slice exists | Launch the most urgent ready slice (part 2) the same hour |
| Green PR unmerged | over 30 min | Review or merge it now |
| Open → merge | p90 over 6 h | Find the cause (review wait, CI, fix rounds) and fix that |
| Commits per PR | p90 over 10 | Briefs or rulings too vague: measure before ruling, tighten briefs |
| CI queue delay | p90 over 10 min | CI saturated: batch pushes; consider fewer slots |
| Slowest CI job | over 30 min | Track X-ci-test-shard; pick the next target after it lands |
| Slot on one task | over 12 h | Check the session; unblock, split or reassign |
| Red runs on `main` | any | Root-cause each. A merge that broke main gets a fix first; a flake is fixed once |
| Fix, flake or revert merges | over 25% of merges | Tighten reviews and self-review; tell the owner |

## 2. The plan (`tools/critical_path.py <plan_nodes.json> G-v1`)
The input is the status connector's `plan_nodes` output (saved to a file).
1. **Critical path.** Find the longest remaining chain to `G-v1`. Every slot that frees goes to the
   most urgent ready slice, the one with the longest chain behind it to 1.0, before lower-value
   work. Unmet conditions on the path the runner can meet itself (a migration slot, a brief, a
   placement) get met the same run. Ones only the owner can meet go to the owner early, with the
   date they'd start to block.
2. **Re-sequence and split.**
   - Reorder the queue to the critical path.
   - Split a slice that would take a slot over a day.
   - Run work early where owns are disjoint and a later gate still covers it. Example: the design
     review's early part on screens that are already final.
   - Drop or defer work outside the 1.0 scope freeze, and tell the owner.
3. **Waste.** Look for repeat review rounds and their cause, idle sessions, work redone, and slices
   waiting on the runner. Fix the cause, not the instance.
4. **Strategy, daily** (the first run after 18:00Z):
   - Compare the dashboard with the fastest-1.0 plan (§3 scope, §6.1 calendar).
   - Update the estimate to 1.0 and the date of each upcoming owner action.
   - Name the one or two biggest improvements available, and make them, or ask the owner.
5. **Records.** The runner records every plan change it makes in its next records PR, as a
   decisions file and a dashboard edit (CLAUDE.md). Owner-reserved changes are asked, never made.

## 3. Principles check
For each change made or proposed in the run, one line: which principle it could touch, and why
it holds.

## 4. The dashboard's plan and estimates (`tools/estimate.py <plan_nodes.json> 14 state.json reviews/estimate-fixed.json`)
1. **Estimate.** Run the estimator on the same `plan_nodes` file. For each target in
   `reviews/estimate-fixed.json` (the v0.6 crawler gate, the 1.0 cut, 1.0 ready to tag) it gives a
   likely date (p50) and a slow-case date (p75), from measured launch-to-merge times of planned
   slices over 14 days, the slots, the remaining chain and the fixed waits (live crawls, soaks).
   Owner steps are assumed on time and listed. Revise `estimate-fixed.json` when a measured run
   takes a different time than it says.
2. **Plan.** Every plan change the run makes in part 2 (re-sequencing, splits, deferrals, new or
   removed dependencies) goes into `runner/slices.yaml` and `runner/dashboard.yaml`
   (`roadmap`, `phases`, `workstreams`, `conditions`, `owner_queue`) so the dashboard shows the plan
   as it now stands. Owner-reserved changes are asked first, never made.
3. **Estimates on the dashboard.** The `notes.now` entry labelled "Estimates" carries the three
   dates (likely and slow case), what bounds each (the chain or the slots), the owner steps they
   assume, and the biggest risk. `notes.updated` gets the date. Roadmap step texts that give a date
   are kept in line with it.
4. **When to publish.** The dashboard reads main, so a change needs a PR. Open or update the
   runner's records PR (`claude/busy-allen-aptli2`) when a likely date moves by a day or more, the
   plan changed, or on the daily strategy pass, whichever comes first; otherwise log only. Run
   `cd tools/status-connector && npm test` first. One open runner PR at a time: add to it rather
   than opening another.

## Log
Each run appends one line to `reviews/throughput.log`: the time, the key numbers, the length of
the critical path, the three estimate dates, and what changed.
