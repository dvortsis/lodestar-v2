# Runner: two slots, a launch pause, and the SRC-14 split recorded

Runner, 2026-11-02. A docs-only PR that records instructions and splits already acted on in runner-state.

- **Two worker slots, then a launch pause** (owner, `docs/owner-decisions.md` §2.30). `runner/slices.yaml` `limits.phase_total` goes from 4 to 2. No new slice sessions start until the soak r3 verdict is in and the owner says to resume. Sessions already running may finish, and their PRs may merge.
- **SRC-14 was split into SRC-14a and SRC-14b** (runner, during the slice), with 14b in two PRs (14b-1 and 14b-2). All three have merged: #394, #399 and #400. Their decisions files hold the details. `runner/slices.yaml` and `runner/dashboard.yaml` now list the parts, so the status connector places them.
- **Build plan §0.6:** "§1.6's budgets" now names `docs/owner-decisions.md` §1.6. Before, it read as if it pointed to a section of the build plan or the architecture, and neither has one.
