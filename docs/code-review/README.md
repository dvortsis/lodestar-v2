# Review log

## 2026-11-13

- [Full code review](2026-11-13-full-code-review.md): twenty read-only area reviews of `main` at `55e155a9`, with a second read of the top findings and a dependency sweep. 203 findings (16 rated blocking, 100 important, 87 polish), 153 of them not tracked anywhere. The first five concern content safety: the child-safety core's truncated read, CS-8a never firing, no re-evaluation of stored torrents, and update rollbacks that drop holds. Area reports are in [`2026-11-13-full-code-review/`](2026-11-13-full-code-review/). Nothing was built or run.

These are review records, not completed fixes.

## 2026-10-29 to 2026-11-04

- [Soak memory and performance review](2026-11-04-soak-memory-review.md): read-only review of the r1–r4 soak evidence and the rc.3 source. Where the memory sits (the 185 MiB fetch-pool step, the corpus-proportional plateau), the residual slope candidates after the mmap-cache fix (one commit per torrent and the whole-index merge regime, the unmeasured C heap under musl's allocator), r4 watch items, and the changes that can precede the next frozen soak, split into metrics-only and class A.
- [Master plan r2 review](2026-11-01-master-plan-r2-review.md): eight findings against `e528c369` (collector evidence gaps, schema-field backfill, runner takeover fence, 10M rebuild acceptance, measurement specification, required web checks, online import acceptance, ordering rules) plus owner decisions and scheduling risks.
- [Master plan review](2026-10-29-master-plan-review.md): review of the October 29 soak-to-release master plan; revise-before-approval findings on the restart shortcut, throughput inference, admission coordination, A/B design, release transfer, import contracts, PostgreSQL restoration, storage estimates, history replacement and CI mapping.

These are review records, not completed fixes.

## 2026-10-24

> **Status, 2026-10-30:** the Bugbot findings and the UI audit fixes listed below landed with the owner's soak follow-ups (landing range L4: the snapshot deletion journal, the file-cache budget, cold-compile cancellation, the verified partial-import upgrade, updater ordering and the audit fixes). DUR-26 (updater write errors) is F-updater. These entries are the 2026-10-24 record.

- [Supplied throughput strategy: Lodestar gap analysis](../soak/2026-10-24-strategy-gap-analysis.md): existing foundations, critical peer-resolution gaps and recommendations that conflict with product goals or need correction.
- [Modern DHT research](../soak/2026-10-24-modern-dht-research.md): primary-source implementation comparisons, protocol opportunities and benchmark limitations.
- [Metadata throughput strategy](../soak/2026-10-24-throughput-strategy.md): prioritized optimization proposals and a controlled validation plan.
- [Ongoing soak metadata-throughput investigation](../soak/2026-10-24-v0.5.0-metadata-throughput.md): five findings covering stalled DHT lookups, obsolete queued work, admission/lookup imbalance, peer reachability and telemetry. Runtime evidence and an offline lookup-progress probe; no application or runtime changes.
- [Bugbot gap review](bugbot-gap-review-v0.5-2026-10-24.md): six new findings in snapshot retention, file-query caching/cancellation, verified import upgrades and updater persistence. Includes standalone regression probes and validation limits; findings remain open.
- [UX/UI design audit](ui-ux-audit-v0.5-2026-10-24.md): ui-design-skills audit of the current frontend, with priorities, fixes, screenshots and a machine-readable validation ledger. Findings remain open.
- [Light review Area A: UI behaviour](codex-review-v0.5-light-area-a-results.md): four findings, review evidence and limitations.
- [Light review Area B: packaging and install](codex-review-v0.5-light-area-b-results.md): four findings, review evidence and limitations.

The light reviews examined fresh `main` at `2d5e761d30cb1b7947fb09493959668eabcb84ab`. The design audit examined workspace `ad0ec5a84555b8b36e7ae94ed515b84ad6d46e02`; its frontend source was identical. These logs record review results, not completed fixes.

## Existing v0.5 reviews

- [Area 1 results](codex-review-v0.5-area-1-results.md)
- [Area 2 results](codex-review-v0.5-area-2-results.md)
- [Area 3 results](codex-review-v0.5-area-3-results.md)

Review briefs: [v0.5](codex-review-v0.5.md), [v0.5 light](codex-review-v0.5-light.md).
