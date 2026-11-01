# Master-plan update decisions (runner, 2026-10-30)

These are the runner's choices in the post-soak master plan (`docs/build-plan.md` §0). They sit on top of the owner's answers in `docs/owner-decisions.md` §2.25. The owner can veto any of them under §2.19. The items marked **owner** must be confirmed before the step that depends on them.

## History swap

- **`next` held the rewritten `main` until the owner force-pushed it** on 2026-10-30. It points at `058d9937`, the rewritten counterpart of the old `main` tip `2d5e761d`; slice sessions branched from it until then. It's deleted now that `main` equals it.
- **What the rewrite changed at the tip:** `docs/architecture.md` and `docs/owner-decisions.md` (37 insertions, 46 deletions). It also drops 4 content-filter design commits from the Oct 22 history. The owner confirmed both tree diffs as intended (§2.25).
- **The swap, done by the owner:**
  1. Back up every ref privately (`git bundle create … --all`).
  2. Pause the runner and its scheduled tasks.
  3. Lift the ruleset for the push only.
  4. Push with `git push --force-with-lease=main:2d5e761d… origin 058d9937…:main`.
  5. Restore the ruleset straight away; the runner checks it's back.

  Old `refs/pull/*` keep the old commits reachable. Purging them needs GitHub support.

- **Lineage guard:** after the swap, CI's `changes` job fails any PR whose head contains the old root `15620683` or the old tip `2d5e761d`. A clone from before the swap could otherwise merge the removed history back in. Cloud sessions from before the swap are archived, not resumed.
- **Old branches:** about 150 remote branches still hold the old history. **Owner:** delete them after the backup if the rewrite was meant to scrub content. The paused slices `B-update` (5/5 WIP), `S-settings-1` and `M-settings-form-3` are ported first, each as a diff from its old merge-base.
- SHAs cited from the old history in `docs/` and `runner-state` stay as historical text.

## Landing the owner's local work

The owner's commits form one linear chain on `work/soak-followups`, and most of them edit the same docs (`architecture.md`, `build-plan.md`, `metrics.md`, plan files). Splitting them by topic would mean hand-resolving docs conflicts in every PR. They land instead as **contiguous ranges, merged in order** with merge commits, so each PR's diff is exactly its range:

| PR  | Commits                            | Content                                                                                                                               | Review                                                        |
| --- | ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- |
| L1  | `ad0ec5a8`                         | README, user-guide screenshots, v0.5 review and soak reports                                                                          | docs                                                          |
| L2  | `43d054df`, `80d7e198`             | CR-0 (sampler, wire tokens, `network-policy`, lookup ownership), soak analyzer, lifecycle regressions                                 | Opus protocol review, `/security-review`, `full-ci`           |
| L3  | `bff278fb` … `07421c3c`            | PW-1..4: detail visibility, setup completion, Back focus, Discover All, build and shutdown docs                                       | web, E2E, `/security-review` (adult cache)                    |
| L4  | `a7c6d5c8` … `5e1a53c0`            | V12 deletion journal, file-cache budget, cold-compile cancel, UI audit fixes, partial-import upgrade, updater ordering, docs lint     | Opus (on-disk format), `full-ci`                              |
| L5  | `18a02564` … `e41f0cf3`            | Admission diagnostics, Windows bundle validation, collector hardening, plus a new CI job for `docs/soak/tools/test_*.py`              | Opus                                                          |
| L6  | `ce59c3e3` … `d63799fc`            | Baseline docs, shared status presentation, CR-0 checkpoint evidence. About 10.5k lines, mostly evidence: the size is stated in the PR | docs, web                                                     |
| L7  | `23a986fd` (on `fix/dht-cr0` only) | Bugbot and Codex light reviews, the UI audit, crawler plan addendum, Magnetico/Bitmagnet and import-wizard plans                      | docs. **Personal Windows paths are scrubbed** first (3 files) |

- Before each PR, its range is checked for personal paths, process IDs, machine names, hashes, peer IPs and tokens.
- Each PR also gets one search for wording that the history rewrite removed, so none of it comes back.
- `848e10a0` (Settings) isn't in this chain. It's the base for **S-settings-reconcile** after L6.
- A blocking review finding is fixed before merge, even when that changes soaked code. Each such change is listed in the release delta (§0.4).

## Crawler program

- **The next limiter is unproven.** The CR-0 checkpoint shows admission saturation: 63.7M actor capacity rejections and 98.9% of evictions before an accepted lookup. The ~25 s per-lookup figure is a bound that assumes the 32 slots are always full; it wasn't measured. The comparison with v0.5.0 is between different windows, and an external download overlapped the run. So there are three hypotheses, each with its own candidate:
  - H1, lookup latency × the 32-request cap;
  - H2, admission and retry churn;
  - H3, reachability and transport.
- **Candidates, each behind its own runtime switch and measured alone before any combination:**

  | Candidate | What it is                                                                                              | Tests      |
  | --------- | ------------------------------------------------------------------------------------------------------- | ---------- |
  | CR-ADM    | Bounded admission and wakeup (DUR-02–04)                                                                | H2         |
  | CR-1a-H   | Responder hints                                                                                         | H1         |
  | CR-1a-S   | Streaming batches. Lookup ownership stays until terminal, so this aims at conversion, not freeing slots | conversion |
  | CR-1c     | Lookup cap sized to the intensity preset, after CR-ADM and CR-1a-H                                      | H1         |
  | CR-1b     | Target prefixes                                                                                         | discovery  |
  | CR-4      | uTP wiring and IPv6 coverage                                                                            | H3         |
  | CR-5      | Profile-driven tuning                                                                                   | —          |

- **CR-0d: restart without the 6 h sampling suppression, only through a crash-safe protocol:**
  - Before the first send in a run that loaded a snapshot, the clean marker is invalidated durably (fsync).
  - Shutdown stops new samples first and settles or records the outstanding ones. Then it writes the snapshot atomically, with a version, checksum and generation.
  - All of these mean full suppression: a backward clock jump or an implausible gap, a corrupt or version-mismatched file, a foreign data or node identity, or a restored backup (generation not newer).
  - Restored restrictions are always honoured.
  - It needs an Opus protocol review.
  - It can't help a first install. The first start is still suppressed for 6 h (**owner:** this is a first-run product choice).
- **CR-SIM:** a deterministic replay harness screens every candidate before a live run.
- **CR-COL:** a Python collector for Linux and Docker. It must pass the existing fault, shutdown, identity and evidence-publication rehearsals.
- **A/B runs:**
  - 6 h of _measured_ time, counted after suppression and warmup;
  - a controlled starting state;
  - at least 2 repetitions per arm, in ABBA order;
  - treatment boundaries and host activity logged;
  - per-family (IPv4/IPv6) counters;
  - whether a Defender exclusion was active, on Windows.

  No imports run on the experiment host. Bitmagnet comparator runs never overlap a Lodestar run and never follow one back to back: they're on separate days, from fresh data, with the version pinned.

- **Gate metric: unique infohashes whose info dictionary was fetched and verified from the network in this run, committed and searchable.**
  - It excludes imports, recovery, rediscoveries, filtered or quarantined items, and failures. Those are reported separately.
  - It's measured on **one node and one public IP, from a fresh data directory with no imports,** at the Balanced preset on a home connection.
  - **Owner:** confirm the proposed thresholds, a 24 h mean ≥ 100/min with no hour below 70/min and p95 discovery-to-searchable ≤ 10 min, before the first acceptance analysis. The 10 min runs from first sighting and includes the metadata fetch; it doesn't replace §1.6's < 5 s from stored metadata to searchable.
  - New counters are needed: `metadata_new_verified_total{origin}`, `searchable_new_total{origin}`, and discovery-to-searchable latency.
- **Guardrails are never tuned away to hit the target:** BEP 51 intervals, per-IP limits, and the §18 politeness caps. Each run also records packets/s, bytes, connects/s, open sockets and router/NAT health.
- **If every candidate is measured and the single-node metric stays below 100/min,** the runner reports the ceiling and the dominant limiter, and the owner re-decides. Remote workers (more IPs) serve the 1,000/min stretch, not the gate.

## v0.5.1 release

- The release is cut from a **release branch** at a fixed SHA, and tested as **`v0.5.1-rc.N`** artifacts. `release.yml` accepts prerelease tags without moving `:latest`.
- **The delta from the soaked `744b84d6` is mapped line by line to targeted tests.** A non-trivial change to the crawler lifecycle, the sampler or the endpoint policy means another soak on the rc artifact.
- **The gate:**
  - L1–L7 merged, plus F-auth, F-filters, F-metainfo, F-updater and F-status;
  - CR-0 soak accepted;
  - every workflow dispatched on the cutoff SHA and green, with no regenerated baselines;
  - a ≥ 6 h Windows **service-mode** run of the rc MSI, plus a portable smoke test;
  - a **24 h Linux Docker soak on the owner's Unraid box** with CR-COL, including a ≥ 2 h gluetun leg. It replaces the old pre-CR-0 container on its Oct 22 volume, so it is also the upgrade test.
- **The soak fails** if any of these happen: a stall, memory growth, an interruption, or less than 24 h of measured coverage. The fix is a fix PR and a re-soak of a frozen build; runs are never stitched together.
- **Migrations V13 and later don't merge to `main` until the release branch is cut.** The daily `:nightly` image must not run on real data.
- **The update engine** ships disabled unless real signing keys and a manifest exist. **Owner:** decide about the keys.
- **Honest UI** (F-status):
  - the transport (uTP) setting is labelled "not active yet" until CR-4;
  - the status pill and Insights show a progress-aware status (stored per hour, stalled-pipeline warning);
  - the Docker `HEALTHCHECK` stays a liveness check.
- **Docs and release notes** are built from the release branch. New strings need zh entries or the documented fallback.
- **Owner:** consider making `Web / web` and `E2E / run` required checks. §2.22 left them outside `ci-ok` on purpose, so this is the owner's call.

## Imports

- **CLI first, for the owner's two real sources:**
  - Magnetico: read-only SQLite.
  - Bitmagnet: a restored, isolated PostgreSQL 16, read through the existing `crates/sources-importers/src/bitmagnet.rs`, hardened:
    - TLS;
    - the credential from env or a file, not argv;
    - bounded file batches;
    - one cursor per source;
    - rows without a name kept;
    - text read as raw bytes, because the dump is `SQL_ASCII`.
- **Stated scope reduction:** these are deferred: the SQL/ZIP dump parser, the other crawler families, uploads (which need an exception to the JSON-only middleware, so the owner decides first), MongoDB/MySQL, the wizard, and **recovery until CR-3**.
- **Acceptance:**
  - the cursor advances in the same transaction as the batch;
  - crash replay is idempotent;
  - the cursor is bound to the source's identity;
  - file reads are bounded;
  - filter, floor and tombstone checks run on every record;
  - pause and cancel are truthful;
  - verification, completeness and recovery stay separate states, with a per-record private/public/unknown policy.
- **Migration order:** V12 (deletion journal, L4) → V13 (import jobs) → V14 (CR-3 eligibility). Migrations are written only by an Opus slice that owns the store schema, one at a time.
- **Capacity is measured at each pilot (10k → 100k → 1M → 10M → 50M):**
  - bytes per record (SQLite, indexes, provenance);
  - source copies, the PostgreSQL restore, WAL and temp space;
  - backups, and old and new indexes during a rebuild;
  - the have-filter;
  - `filter_hits` and the review queue;
  - relevance (NDCG@10);
  - backup and restore time;
  - cold start;
  - RSS;
  - search p95;
  - Insights overhead.

  Each pilot is measured against §1.6's budget of about 2.3 KB per torrent all-in; any excess, with what drives it, goes to the owner before the next stage. A backup is taken before each stage, and removal by job provenance is tested.

## Execution

- **Priority ladder for at most 4 helper slices at a time** (the plan the owner approved, §2.25):
  1. L, F and CR-COL (the v0.5.1 Linux soak needs it);
  2. the release cut;
  3. C;
  4. I;
  5. S;
  6. the M5 screen backlog;
  7. v0.6 epics.

  Docs ride along. The owner can reorder this in one line.

- **One operator per lane.** Every change reaches `main` through a PR the runner merges, and the owner tells the runner before taking a lane locally.
- **Soak evidence policy:** commit aggregates and hourly snapshots only, and state each PR's repo-size delta. Git LFS is blocked.
