# ENR-1: adult enrichment runtime (eligibility gate, what is sent, caches, filter before storage, re-classify on match)
**Model:** Opus (content safety and privacy core). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T06:05Z.

## Why
ThePornDB and StashDB (ENR-4 and ENR-5) can only be added once a runtime decides what may leave the install and judges what comes back. Its dependencies have all merged: ENR-0, ENR-6, SRC-13 and X-source-adult (#467). The owner's answers to Q2 and Q3 are recorded in `docs/owner-decisions.md` (the rule-pack Q1–Q5 bullets).

## The spec
`docs/plan/2026-11-04-v06-rulepacks.md`, the "ENR-1" bullet, which covers eligibility, never-send, what is sent, order, results, caches and gates. Also read:
- Q2, Q3, Q11, Q13 and Q15 in that plan's §5;
- `docs/owner-decisions.md` (the Q2 and Q3 answers);
- architecture §18.1, §18.7, §20.5 and §20.9;
- `docs/decisions/2026-11-05-X-source-adult.md`. It defines the always-adult source mask. An item from an always-adult source counts as Adult for eligibility, but only through that mask.

## Do (branch `slice/ENR-1` from `origin/main`, after #467 merged)
1. **The eligibility gate, as one function that every lookup goes through.** Eligible means a stored, verified torrent classified Adult (or adult through the always-adult mask), whose source is enabled, whose consent condition is met, and while "Show adult content" is on.
   - **Never eligible:**
     - anything the core or a filter holds (quarantine, tombstone, drop, hide);
     - `private_source` keys;
     - unverified items;
     - anything while the plugin's egress kill-switch is tripped.
2. **What is sent:** the display name, or the parser's site, date and title fields only. Never the info hash, the files, the trackers or anything about the install. Lookups come from user interest first (the detail drawer), then a capped background backfill within the manifest caps (Q15).
3. **Results become claims under the plugin's source id.** They go through SRC-2's filter-before-storage path.
   - Extend `unverified::judge` so the core also judges the enrichment text, stricter only. It can add a hold, never remove one, and it never counts as a counter-signal (Q3).
   - On a match, re-classify by reading the stored v2 claim into ENR-6's classify entry point, in `pipeline/src/enrich.rs` and `worker.rs`.
   - SRC-2's refile sweep re-judges enrichment claims when the rules change.
4. **Caches:** hits and misses in `source_state`, with Q15's TTLs.
5. **Gates (tests):**
   - a property test: no ineligible item is ever handed to a plugin;
   - a core-held item with a perfect match stays held;
   - no request carries the info hash;
   - cancellation: the plugin stops after shutdown, waited for as a condition with a ≥ 10 s deadline;
   - the re-judge sweep covers enrichment claims;
   - an always-adult (Sukebei) item that is verified is eligible; an unverified one isn't.
6. **Decisions file:** `docs/decisions/2026-11-05-ENR-1.md`.

## Own
`crates/sources/src/enrich.rs`, `crates/pipeline/src/adult_enrich.rs` (new), `crates/pipeline/src/unverified.rs`, `crates/pipeline/src/enrich.rs`, `crates/pipeline/src/worker.rs`, `crates/pipeline/tests/adult_enrich.rs`, plus the one-line `mod` and wiring edits in `crates/sources/src/lib.rs` and `crates/pipeline/src/lib.rs`.
- **Never edit `crates/pipeline/src/writer.rs`.** SRC-13b and RP-4 or RP-5 own it in turn. If you need a hook there, message the runner.
- **SRC-13b runs alongside you.** It owns `writer.rs`, the host wiring in `crates/lodestar`, and the torrent-cache consent change, and it won't touch your files. You both add lines to `pipeline/src/lib.rs`; whichever merges later brings `main` in.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass. If the container's disk can't hold the workspace run, test per crate and say so in the PR. Then run `/security-review`.

Open the PR against `main`, titled "ENR-1: Adult enrichment runtime: eligibility gate, what is sent, caches, filter before storage, re-classify on match". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
