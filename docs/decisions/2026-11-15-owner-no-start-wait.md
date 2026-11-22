# No sampling wait after a start (owner, 2026-11-15) (#942)

The owner's decision, from a planning session on 2026-11-15, about 17:45Z. Nothing is built yet. The runner records it in `docs/owner-decisions.md` §2, places the proposed slices below and writes their briefs.

## The owner's words

- "We can't make people wait 6-hours before seeing progress. That's a terrible UX choice."
- "I'm less concerned about politeness. If there are no repercussions for re-asks, I'm not worried about it."
- "Yes, let's honor interval during runs. I think we can prefer a safer restart so we're being efficient, which means keeping that part in. Just removing the wait time if it wasn't safe. I'd still like to implement the UX improvements."
- Asked whether "prefer a safer restart" means safe restart on by default: "Correct".
- Asked whether to start the bitmagnet import (`B-import-server-2`) now: "I can't run tests on the unraid machine yet". The session reads this as: the import stays deferred for now, and no step in these slices may depend on a run on the owner's Unraid machine until the owner says it's available.

## What it supersedes

- `2026-10-25-CR-0.md`, "Restart safety": suppress sampling for six hours plus the query timeout after every production start. Also `docs/architecture.md:111`, which repeats it.
- `2026-10-30-master-plan.md`, CR-0d: "The first start is still suppressed for 6 h (owner: this is a first-run product choice)."
- `2026-10-31-owner-delegation.md`: "The first start is suppressed for 6 h, as on every start … suppression is the polite choice."
- `2026-10-30-CR-0d.md`, where it makes an unproven start wait:
  - §0.1, the safety invariant (item 4);
  - §1.6 and §2: every outcome other than `resumed` means "suppress", including `first_start`'s "the first start stays suppressed";
  - §2's N1 (a run commits a blanket gate and a resumed run inherits it) and I1 (a late landing waits for this run's start + H, even on a resume);
  - §5.2, the off-run's window;
  - §5.3: `dht_sample_suppression_seconds` "never reads 0 while sampling is disabled".
- **`docs/owner-decisions.md` §2.34 O1** says the 24 h gate window "excludes the 6 h first-start suppression and the 60 min warm-up". With no suppression there is nothing to exclude. The runner amends that entry when it records this decision; the warm-up is unchanged.

## Decisions

- **No sampling wait after any start.** First installs, crashes, restored backups, unproven restarts and image updates all sample as soon as the DHT has bootstrapped. That includes the first clean restart after an unproven run: no gate carries over (below).
  - **Why:** the owner's call above. The wait existed only so that a start that couldn't prove its state never re-asked a node before the `interval` it had set. BEP 51 sets no penalty for that. As far as the session could tell, common clients rate-limit only an IP that sends many queries a second. One restart re-asks each node at most once, the same pattern as normal running. The cost is a few wasted queries that return samples already seen, during the first hours after a start.
  - **One case to check:** a crash loop under a restart policy (Docker's `unless-stopped`, Windows service recovery) re-asks the same nodes on every restart, which the six-hour wait used to prevent. The owner's condition was "if there are no repercussions for re-asks". CR-0w's Opus review decides whether that needs a guard, such as a short wait only after several unclean starts in a row.
- **`interval` is still honoured within a run, unchanged:** the 60 s floor and 6 h cap (`crates/dht/src/node/sampler.rs:32-33`), our own 6 h backoff for nodes without BEP 51 (`sampler.rs:31`) and for timeouts (`SAMPLE_TIMEOUT_BACKOFF`, `crates/dht/src/node/actor.rs:86`).
  - **Why:** asking a node early mostly returns the same samples, so honouring it is an efficiency rule as well as a courtesy.
- **Safe restart (CR-0d) stays, now for efficiency.** After a proven clean restart, the restored per-node restrictions are honoured, so the node doesn't spend its first hours re-asking nodes whose samples it already has.
  - **On by default** (`crates/lodestar/src/config.rs:221` becomes `true`; owner confirmed, above).
  - **When the proof fails, the node samples at once with no restored restrictions.** The reason (`first_start`, `dirty`, `no_witness` …) stays as a diagnostic.
  - **No blanket gate.** Only per-node restrictions carry over a restart. An unproven run commits no gate, a resumed run drops any gate it restores, and a late landing no longer adds this run's start + H (`proof.rs:414-416`, `:505-508`, `:579-587`). Otherwise a first install restarted cleanly after 1 h would still wait about 5 h.
- **The Docker restart record stays inside the container** (`packaging/docker/README.md:150-156`). An image update now loses one run's restored restrictions, a small efficiency cost, instead of six hours of sampling. No code or image change; the README's text changes (D-start).
- **The UX work from the session goes ahead** (proposed slices 3 and 4 below).

## Refinements for the runner to confirm and flag (§2.19)

- **A failed proof restores nothing,** rather than a best-effort restore from an intact but unproven snapshot. It's simpler and needs no new trust rule. A best-effort restore can come later as an optimisation.
- **`invalidate_failed` and `locked` no longer hold sampling.** The run samples at once. It can't promise to leave no resumable marker: when invalidation fails, the previous clean marker stays on disk, so a crash after sampling lets the next start resume from that older snapshot. With no blanket gate, that only costs re-asks of nodes whose restrictions it misses. The exact rule is for CR-0w's Opus design review, since the marker protocol (`2026-10-30-CR-0d.md` §2 and §3) assumes those cases wait.

## Proposed slices

1. **CR-0w: remove the start wait** (core, Opus, Opus review).
   - `Proof` (`crates/dht/src/node/proof.rs`) stops suppressing, and the blanket gate goes (Decisions, above). `Proof::new`'s `H` (`actor.rs:859`) stays as the cooling horizon for in-flight samples at shutdown.
   - **`sample_start_delay`'s coupling to safe restart goes.** Today `Mode::On` needs `sample_start_delay == None` (`proof.rs:153-157`, `crates/dht/src/node/mod.rs:163`). Setting the default to `Some(0)` would quietly turn safe restart off for every run. The slice removes the six-hour default (`mod.rs:143-146`) without that side effect.
   - `safe_restart` on by default.
   - Metrics contract (`docs/metrics.md`): `dht_sample_suppression_seconds` reads 0, or is retired, and `metrics.md:694`'s "unless a restored blanket gate still holds" goes. `dht_sample_suppression_reason` keeps its labels as the restart outcome. `stats.sampling_blocked` (`metrics.md:423`) is null except where CR-0w's review keeps a wait.
   - CR-SIM: the restart scenarios and goldens (`crates/crawler-sim`, `2026-10-30-CR-SIM-restart.md`) are re-recorded. New scenarios show an unproven start sampling at once, and a clean restart after an unproven run sampling at once.
   - The legal notice (`docs/legal/legal-notice.md:45`) calls safe restart "optional". The slice checks whether the default change makes that a substantive notice change, and asks the runner before editing it.
2. **CR-0w-soak: measurement follows** (runner, then owner).
   - `docs/soak/tools/analyze_cr0.py` and the Linux collector: the measured window no longer starts after a suppression.
   - Also: the Windows soak baseline rule (`2026-11-12-M8-WIN-1.md`), the CR-SPEC windows (`2026-11-02-CR-SPEC.md`), owner-decisions §2.34 O1 (above) and `docs/build-plan.md`'s soak text.
   - A soak confirms the memory gate (hours 12–24) with sampling from minute one. r5 passed with the wait in place, and the sampler's 200,000 states now fill about six hours sooner.
   - The owner can't run tests on the Unraid machine yet (above). Until then the runner decides whether a cloud run (`docs/plan/2026-11-08-cloud-test-plan.md`) screens the change and which release the Unraid confirmation gates.
3. **UX-start: the first hour shows progress** (helper, Sonnet, after CR-0w).
   - `web/src/lib/shell/progress.ts:14`: drop the six-hour uptime fallback (`STARTUP_SUPPRESSION_S`). The status pill says "Starting" only while bootstrapping, then shows warming up with live counts (found, stored). Words go through `docs/ui-glossary.md`.
   - Overview: in the first hour, show the pipeline (routing nodes, samples, metadata fetched, stored) instead of empty charts.
   - Wizard: replace `wizard_how_cold` (`web/src/lib/features/wizard/messages/en.json:56`, and zh), and show live counts on the later steps, since the crawler runs during setup.
   - Set expectations honestly: the index grows over days and weeks (`docs/architecture.md` §3, "Cold-start reality"). Soak r5 stored about 695 torrents an hour at full speed.
4. **D-start: user and packaging docs** (helper, Sonnet, `user-docs` skill, after CR-0w):
   - the guide: `docs/user/02-…docker.md:249`, `03-keep-lodestar-safe.md:308` and `:313` and its safe-restart section, `04-…linux.md:235`, `05-use-lodestar.md:57,69`, `06-troubleshooting.md:14`, `appendix-b-settings-reference.md:33`;
   - `docs/operations.md:43` (the default becomes `true`) and `:205`;
   - `packaging/docker/README.md:152-156` ("waits the full six hours", "waits the six hours once") and `packaging/systemd/README.md:39-44`;
   - `docs/ui-glossary.md:61`;
   - the README.

## Notes

- **Import up front.** The fastest way to a full index on day one is the bitmagnet import. Its wizard step is a hidden slot until `B-import-server-2` is built. Its owner condition is met (`runner/dashboard.yaml:959-960`, owner §2.37), but the slice is deferred by its notes (`runner/slices.yaml:738`). The owner's answer keeps it deferred for now (above). Meanwhile X-first-run's import guide link (`slices.yaml:876`) is the shortcut; no new slice.
- **WK-10c's one-minute linger** (`2026-11-08-WK-10c.md`) was justified by the six-hour wait. It still works without it, but a simpler rule is now possible. Not required.
