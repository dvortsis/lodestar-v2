# No sampling wait after a start (owner, 2026-11-15)

The owner's decision, from a planning session on 2026-11-15, about 17:45Z. Nothing is built yet. The runner records it in `docs/owner-decisions.md` §2, places the proposed slices below and writes their briefs.

## The owner's words
- "We can't make people wait 6-hours before seeing progress. That's a terrible UX choice."
- "I'm less concerned about politeness. If there are no repercussions for re-asks, I'm not worried about it."
- "Yes, let's honor interval during runs. I think we can prefer a safer restart so we're being efficient, which means keeping that part in. Just removing the wait time if it wasn't safe. I'd still like to implement the UX improvements."

## What it supersedes
- `2026-10-25-CR-0.md`, "Restart safety": suppress sampling for six hours plus the query timeout after every production start.
- `2026-10-30-master-plan.md`, CR-0d: "The first start is still suppressed for 6 h (owner: this is a first-run product choice)."
- `2026-10-31-owner-delegation.md`: "The first start is suppressed for 6 h, as on every start … suppression is the polite choice."
- `2026-10-30-CR-0d.md`: every outcome other than `resumed` meaning "suppress" (§1.6 and §2), including `first_start`'s "the first start stays suppressed".

## Decisions
- **No sampling wait after any start.** First installs, crashes, restored backups, unproven restarts and image updates all sample as soon as the DHT has bootstrapped.
  - **Why:** the owner's call above. The wait existed only so that a start that couldn't prove its state never re-asked a node before the `interval` it had set. BEP 51 sets no penalty for that. As far as the session could tell, common clients rate-limit only an IP that sends many queries a second, and a restart re-asks each node once, the same pattern as normal running. The cost of re-asking is a few wasted queries returning samples already seen, during the first hours after a start.
- **`interval` is still honoured within a run, unchanged:** the 60 s floor and 6 h cap, and our own 6 h backoffs for timeouts and nodes without BEP 51 (`crates/dht/src/node/sampler.rs:31-33`).
  - **Why:** asking a node early mostly returns the same samples, so honouring it is an efficiency rule as well as a courtesy.
- **Safe restart (CR-0d) stays, now for efficiency.** After a proven clean restart, the restored restrictions are honoured, so the node doesn't spend its first hours re-asking nodes whose samples it already has.
  - **On by default** (`crates/lodestar/src/config.rs:221` becomes `true`). This is the session's reading of "prefer a safer restart"; the runner confirms it with the owner when it records the decision.
  - **When the proof fails, the node samples at once with no restored restrictions.** The reason (`first_start`, `dirty`, `no_witness` …) stays as a diagnostic.
- **The Docker restart record stays inside the container** (`packaging/docker/README.md:150-156`). An image update now loses one run's restored restrictions, a small efficiency cost, instead of six hours of sampling. No packaging change.
- **The UX work from the session goes ahead** (proposed slices 3–5 below).

## Refinements for the runner to confirm and flag (§2.19)
- **Safe restart on by default** (above).
- **A failed proof restores nothing,** rather than a best-effort restore from an intact but unproven snapshot. It's simpler and needs no new trust rule. A best-effort restore can come later as an optimisation.
- **`invalidate_failed` and `locked` no longer wait for the restart record.** The run samples at once and doesn't leave a resumable marker. The exact rule is for CR-0w's Opus design review, since the marker protocol (`2026-10-30-CR-0d.md` §3) assumes those cases wait.

## Proposed slices
1. **CR-0w: remove the start wait** (core, Opus, Opus review).
   - `Proof` (`crates/dht/src/node/proof.rs`) stops suppressing. `sample_start_delay` (`crates/dht/src/node/mod.rs:143-146`) loses its six-hour default, or is removed. `Proof::new`'s `H` (`actor.rs:858`) stays as the cooling horizon for in-flight samples at shutdown.
   - `safe_restart` on by default.
   - Metrics contract (`docs/metrics.md`): `dht_sample_suppression_seconds` reads 0, or is retired. `dht_sample_suppression_reason` keeps its labels as the restart outcome. `stats.sampling_blocked` (`metrics.md:423`) is null except where CR-0w's review keeps a wait.
   - CR-SIM: the restart scenarios and goldens (`crates/crawler-sim`, `2026-10-30-CR-SIM-restart.md`) are re-recorded, with a scenario showing an unproven start sampling at once.
   - The legal notice (`docs/legal/legal-notice.md:45`) calls safe restart "optional". The slice checks whether the default change makes that a substantive notice change, and asks the runner before editing it.
2. **CR-0w-soak: measurement follows** (runner, then owner).
   - `docs/soak/tools/analyze_cr0.py` and the Linux collector: the measured window no longer starts after a suppression.
   - Also: the Windows soak baseline rule (`2026-11-12-M8-WIN-1.md`), the CR-SPEC windows (`2026-11-02-CR-SPEC.md`) and `docs/build-plan.md`'s soak text.
   - The next soak confirms the memory gate (hours 12–24) with sampling from minute one. r5 passed with the wait in place, and the sampler's 200,000 states now fill about six hours sooner.
3. **UX-start: the first hour shows progress** (helper, Sonnet, after CR-0w).
   - `web/src/lib/shell/progress.ts:14`: drop the six-hour uptime fallback (`STARTUP_SUPPRESSION_S`). The status pill says "Starting" only while bootstrapping, then shows warming up with live counts (found, stored). Words go through `docs/ui-glossary.md`.
   - Overview: in the first hour, show the pipeline (routing nodes, samples, metadata fetched, stored) instead of empty charts.
   - Wizard: replace `wizard_how_cold` (`web/src/lib/features/wizard/messages/en.json:56`, and zh), and show live counts on the later steps, since the crawler runs during setup.
   - Set expectations honestly: the index grows over days and weeks (`docs/architecture.md` §3, "Cold-start reality"). Soak r5 stored about 695 torrents an hour at full speed.
4. **D-start: user docs** (helper, Sonnet, `user-docs` skill, after CR-0w):
   - the guide: `docs/user/02-…docker.md:249`, `03-keep-lodestar-safe.md:308-313`, `04-…linux.md:235`, `05-use-lodestar.md:57,69`, `06-troubleshooting.md:14`, `appendix-b-settings-reference.md:33`;
   - `docs/operations.md:43` and `:205`;
   - `docs/ui-glossary.md:61`;
   - the README.
5. **Import up front.** The fastest way to a full index on day one is the bitmagnet import. Its wizard step is a hidden slot until `B-import-server-2`, which is held on `ext:owner-import-server-surface` (`runner/slices.yaml:65`). That is the owner's call: whether to unhold it now. Until then the wizard links the import guide (X-first-run).

## Follow-ups that aren't required
- **WK-10c's one-minute linger** (`2026-11-08-WK-10c.md`) was justified by the six-hour wait. It still works without it, but a simpler rule is now possible.
