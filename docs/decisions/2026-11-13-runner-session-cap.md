# Runner: owner decisions §2.61 to §2.65 and the 4 + 4 session cap (#806)

Records the owner's session-count decisions from 2026-11-12 and 2026-11-13 that runner-state cited as "to record", in `docs/owner-decisions.md` §2.61 to §2.65.

- **`runner/slices.yaml` `limits`:** `phase_total` 8 → 4 (build slices), and `extra_shared` stays 4 but now means the four cloud-testing sessions (§2.64). The launch procedure (`runner/README.md` step 7) enforces `phase_total`; the runner keeps the cloud-testing sessions within `extra_shared` through runner-state `cloud_tester` and `extra` (§2.60).
- **`runner/dashboard.yaml`:** the "Four slots" note, which described §2.59's eight, now says four plus four, with cloud testing first (§2.62) and the over-cap rule (§2.65).
- **Times are UTC.** The owner gave §2.64's end in local time; it's recorded as 2026-11-13T00:00Z (§2.35).
- **Runner's calls (§2.19), flagged to the owner:** the cloud-testing-first rule (§2.62: cloud-testing PRs reviewed and merged first, and cloud-testing fix slices take the next free slot ahead of features); which four build and four cloud-testing sessions were kept at 00:00Z (§2.64), and that parked drafts (#801, #802, M8-PERF-1b) resume in a kept slot rather than a new one (§2.65).
- **Not changed here:** build-plan §0.2 and `docs/handoff.md`. Plan r4 (#798) updates their cap text. Once both merge, their "not yet recorded" notes for §2.62 and §2.64 are stale, and the runner's next PR drops them.
