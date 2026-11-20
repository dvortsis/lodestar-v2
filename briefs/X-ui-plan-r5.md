# X-ui-plan-r5 (runner brief 2026-11-20)

Track U step: write the UI review's plan-level findings into §10, so the screens that come next build on them. This is a docs-only slice. It edits docs/architecture.md §10 and docs/ui-glossary.md, and touches no code.

## Read first
- The `runner/slices.yaml` entry for X-ui-plan-r5. Its notes are the scope.
- docs/review/2026-11-13-ui-ux/, all of it, but especially 04-plan-and-flow-review-moderate-user.md and 01-review.md.
- docs/decisions/2026-11-13-runner-ui-review-decisions.md and 2026-11-14-runner-ui-review-placement.md, which place each finding.
- docs/owner-decisions.md §2.14 (the reader), §2.69 (item 17), and §2.9, §2.13 and §2.39 (the nine-step wizard stays).
- CLAUDE.md's order of authority. Owner decisions come first, §10 next, and ui-design-skills only fill gaps.

## Scope
Cover D1, D2, D6, D8, D10 and D11, plus the rules behind U12, U17 and U33:
- name the §2.14 reader in §10;
- reorder §10.4 around the words and clicks a moderate user meets;
- Settings: "start here";
- a help section;
- typed units;
- the `o` guard;
- "alert" in the glossary.

U1 and D7 were declined. Keep the nine-step wizard.

Where a finding conflicts with an owner decision, the owner decision wins. List each such case in the PR. Never reverse or loosen an owner decision. If one seems to need changing, stop and tell the runner.

Keep the PR at or under about 1,500 changed lines. If it would be larger, tell the runner before splitting.

## Owns
- docs/architecture.md (§10 only)
- docs/ui-glossary.md
- docs/decisions/2026-11-20-X-ui-plan-r5.md
- this slice's own entries in slices.yaml and dashboard.yaml, if needed

## Checks
- Every §10 cross-reference you touch still resolves.
- Times are written in UTC with a Z.
- No model identifiers.
- Run `tools/status-connector`'s `npm test` if you touch the yaml.

## Done
CI green. Send the runner "ready <pr> <full sha>". A fresh Opus review follows, checking each D/U item against the review docs.
