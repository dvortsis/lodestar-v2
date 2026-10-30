# Review log

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
