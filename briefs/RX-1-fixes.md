# RX-1-fixes (slot 1): finish PR #1082 (RX-1) for the API lane

The runner decided this (2026-11-23T08:3xZ). The API lane stopped RX-1 at its per-slice spending limit and handed the fixes back to a runner slot (its PR comment, 08:30Z). The owner's instruction (06:09Z) allows a new slice when the API runner needs one. You take over **PR #1082, branch `slice/RX-1`, head 8c4ee3bb**. The lane has released the branch and won't push to it again. You are its only writer.

## Read first
1. CLAUDE.md, docs/handoff.md and .claude/skills/steward/SKILL.md.
2. On #1082, every comment that starts `runner:`. The two that set this work are **06:30Z (security review: S8-10 to S8-13)** and **06:54Z (general review: items 1 to 10)**. Their requirements are the spec. Fetch them with:
   `curl -s "https://api.github.com/repos/dvortsis/lodestar-v2/issues/1082/comments?per_page=100"`
3. The PR's own diff and its decisions file.

## The work, in one push if you can
- **S8-10 (blocking):** cap regex clauses per query at **4** in the parser and again at compile, with a stable `regex_*` error code and a clear message. A lease must not keep regex verifier state, or must charge its real size. Don't allocate dense sets for clauses with no work. Tests: 4 accepted, 5 refused, and a bound on lease size.
- **S8-11 (important):** one verify deadline shared by all of a compiled query's regex clauses, with planning and compile inside the budget. **Facet variants, the adult-facet variant pass and recovery count against the same per-search budget** (general item 1). A facet count computed under a stop is marked partial.
- **S8-12 (important):** a stopped verify never advances a saved search's watermark: percolate and `matched_promoted` honour `regex_verify::stopped`, with tests. The runner prefers this fix. Refusing regex in saved searches is the fallback, and if you choose it, tell the runner **before pushing**.
- **S8-13 (low):** record it in the decisions file by id only (one line). Give no exploit detail anywhere: not in commits, the PR, code comments or docs.
- **General items 2 to 10:**
  - the cache-exclusion test (blocking);
  - `query-language.md` corrections: the "word beside" advice, silent stops until RX-2, a negated clause's count being an upper bound, and the true 2 s limit;
  - size-report or search-bench prints the long-name marker's document frequency;
  - the lease cancel token comes from the pass, not the compile;
  - make the index figure agree (550.4 vs 549.8 B);
  - a closing `/` needs a delimiter after it (whitespace, `)` or end of input), otherwise the text is plain words; narrow the suggest proptest's `in_regex` excuse;
  - gate `availability_at` on `has_regex()`, and build the trigram analyzer once;
  - `has_regex` covers `Node::File`, or a comment says why it needn't.
- **Not in this PR:** lazy or intersected verification (follow-up X-regex-lazy-verify, before G-v07-scale) and the ci.yml 512 B comment (the runner's records PR).

## Rules
- **Owns:** the files #1082 already touches, plus what the items above need in `crates/search`, `crates/api` (search, saved searches, alerts and percolate), `crates/bench` (size-report or search-bench), `docs/query-language.md` and the PR's decisions file. Ask the runner before touching anything else.
- **Size gates unchanged:** index ≤560 B and store ≤976 B per torrent. Measure again if anything stored changes.
- **Tests:** no fixed sleeps; waits use a deadline of at least 10 s. Never skip or disable a test. No tokio test-util on the lodestar crate. `unsafe_code` stays forbidden.
- **Before pushing:** fmt, clippy -D warnings, and the workspace tests for every crate you touch; run the web checks too if `schema.d.ts` or `openapi.json` changes. Bring in main only on a conflict or when the runner asks.
- **Push once per round,** then send the runner `ready <full sha>` with a short list of what changed. The runner reviews it again (general and security) and merges.
- **Step 0:** before writing code, send the runner your plan in up to 10 lines, including the S8-12 choice, then wait for "go".
