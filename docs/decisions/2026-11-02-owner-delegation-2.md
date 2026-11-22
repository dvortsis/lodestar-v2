# Owner delegation of pending questions (2026-11-02)

The owner wrote: "For any questions waiting on me: Use your recommended solution, for best UX/ performance." `docs/owner-decisions.md` §2.28 records the answers. Most apply a recommendation that was already written beside the question. Three had no written recommendation in the plan, so the runner formed or cited one under this instruction. The owner can veto each (§2.19).

| Question | Source of the answer | Answer |
|---|---|---|
| Does r1's V3 → V12 upgrade count as the upgrade test? | Formed now (build plan §0.3 row 1 asked it without a recommendation) | It counts if rc.2's migration code matches r1's build; the runner checks, and otherwise rc.2 gets a short upgrade smoke on a copy of the Oct 22 volume |
| May migrations land on `main` before the v0.5.1 tag? | Formed now; it lifts the owner's 11-01 21:08Z hold. The runner's earlier view is in `2026-11-02-master-plan-r2.md` | Yes. v0.5.1 ships from `release/v0.5.1` (V12), `main` already carries V13, and the v0.5.2 gate adds the V12 → V13+ upgrade and rollback checks |
| Release adult stand-in holds automatically (#367)? | The runner's recommendation in its owner checklist (`q-adult-standin-release`), not in a repo doc | No; they stay held and are counted for review |

The review of #371 also kept two written recommendations as written, rather than tightening or loosening them. Q1 keeps the owner's approval of the drafted consent texts before SRC-15b merges. For Q2, a grey ToS case comes to the owner.
