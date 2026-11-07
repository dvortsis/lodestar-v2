# R-legal-status: Name the v0.5.1 baseline in the operator guidance on the release branch
**Model:** Opus (legal document, release branch). Small and docs-only. Runner brief, 2026-11-07T00:05Z. This is the §2.48 step (build-plan §0 row R-legal) before the owner tags `v0.5.1`.

## Why
The runner's §2.48 check of rc.5 (`release/v0.5.1` at `a0eb0a9087c2474ef4285416403eacbf8a475111`) found:
- **Accurate:** every factual claim in `docs/legal/operator-guidance.md` is true of v0.5.1, and every "(planned)" marker is correct.
- **Unchanged:** the Legal Notice is current (version 3, "Applies to Lodestar v0.5.1"), and `NOTICE_VERSION` stays 3. The substance test applies only to the notice, and the notice doesn't change.
- **The gap:** the guidance's status line (line 3) reads "*Status (2026-11-04): this page describes the current version.*" It doesn't name the release baseline that §2.48 and R-legal require, and its date predates #540's edit. `main`'s copy already names its baseline (0.5.2 / notice version 4).

The guidance isn't embedded in the binary; only the notice is (`crates/api/src/legal.rs`). The tag moves to the new commit, and the binary differs only in its embedded commit hash. rc.5's evidence carries over, because no code changes.

## Do
On a branch from `origin/release/v0.5.1`, with the PR **into `release/v0.5.1`**:
1. **`docs/legal/operator-guidance.md` line 3 (required):**
   > *Status (2026-11-06): this page describes Lodestar v0.5.1, the same software as version 3 of the Legal Notice. Features marked **(planned)** come in a later release.*
2. **Line 5 (editorial):** v0.5.1 has no inbound integration endpoint. Replace "through its web interface, its API, an alert feed or an integration." with "through its web interface, its API or an alert feed."
3. **Line 14 (editorial):** in this build keys carry search, torznab and feeds scopes, and only the feed route is reachable by a key. Replace "(API keys carry only the search scopes)" with "(API keys and feed tokens only read; they can't reach it)".
4. **Rules:**
   - Change nothing else. The notice files, `legal.rs` and the code stay untouched.
   - Add a short decisions file on the release branch, `docs/decisions/2026-11-07-R-legal-status.md`. It records the §2.48 check and its result, and says why the edits don't need a `NOTICE_VERSION` raise.
5. Before you change anything, check with `git show origin/release/v0.5.1:docs/legal/operator-guidance.md` that the three sentences are exactly as quoted. If one differs, ask the runner.

## Done when
- The PR touches only those two files.
- The release branch's CI on the PR is green. `ci-ok` covers it; `tools/status-connector` doesn't run on release.
- Follow `.claude/skills/steward/SKILL.md`, never merge, and push once.
- Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA via the claude-code-remote `send_message` tool.
