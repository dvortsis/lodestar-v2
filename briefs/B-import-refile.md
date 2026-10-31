# B-import-refile: re-judge import candidates when filter or classification rules change
**Model:** Opus: content safety (§20), the store's import tables and the pipeline's refile path. Runner brief, 2026-10-31. Closes IMP-1's open item ("Candidates aren't refiled when filter or classification rules change", `docs/decisions/2026-10-30-IMP-1.md` ~l.150, review important 4) and the matching runner follow-up. It tightens content safety and doesn't loosen it. `/security-review` is required.

## Why
- §20.9 refiles *stored* torrents when filter or classification rules change: a torrent a new rule would hold or drop is held or dropped.
- **Import candidates** (`import_candidates`: valid hashes too incomplete for a canonical row) are judged once, at import. If a later rule (a user drop rule, a subscribed list, a pack switch, a floor term) would hold a candidate's name, the candidate stays until it's held by hash, stored, or deleted with its job.
- A candidate becomes a torrent only through the full pipeline, which re-judges it then. So the gap is what a candidate's name can do *before* that: where it's shown (job status, `lodestar import status`, reports, exports, Insights counts) and whether `complete_with_issues` still counts it.

## Read first
- `docs/decisions/2026-10-30-IMP-1.md` (candidates, `open_candidates`, conservation counters), `-IMP-2.md`, `-IMP-4.md`, `-IMP-4b.md`, `-B-import-server-1.md` (the shared loader).
- `docs/architecture.md` §20 (the pipeline, §20.7, the locked floor, §20.9 refile, quarantine and the seal) and `runner/standards.md`.
- `crates/pipeline/src/enrich.rs` (how refiles are produced and applied), `crates/store/src/lib.rs` (`set_enrichments_at`, `Refiled`), `crates/store/src/import.rs` (candidates), and wherever a rule change triggers §20.9 today (`grep -rn "refil" crates`).

## Do
1. **Inventory first.** List every place a candidate's name or fields can be read: API, CLI, exports, digest, logs, Insights. Put the list in your decisions file. It decides how urgent each step below is.
2. **Re-judge on rule change.** When §20.9 refiles stored torrents, also run the current engine over open candidates *by name* (and any other field the engine judges that candidates hold).
   - A candidate a drop or hold rule now matches is removed from `import_candidates`, or marked held so nothing shows it. Choose one, and say why.
   - Keep IMP-1's conservation counters correct: whatever is removed is still counted, under a reason that says a rule removed it. The job's `complete_with_issues` and `open_candidates` stay truthful.
   - Use the same engine the server uses (B-import-server-1's shared loader). Don't build a second one.
3. **Bounded work.** Candidates can number in the millions at the 10M/50M pilots. Process them in keyset batches on the existing refile path or the writer, inside the writer's share. Never in one transaction, and never block crawling. A rule change during a running offline import is the CLI's problem, not this one: IMP-4 fixes the engine for a run.
4. **Seal and logs:** a removed candidate's name never reaches a log line or an error. Follow the Private log preset and the seal rules.
5. **Tests:** a drop rule added after import removes a matching candidate and keeps the counters right; a non-matching candidate stays; a floor term or CS-1 hash added after import also removes; batching over many candidates finishes without one long transaction; nothing in logs. Condition waits only, no fixed sleeps, deadlines ≥ 10 s.
6. **Decisions file:** `docs/decisions/<date>-B-import-refile.md`, plus a one-line pointer from IMP-1's open item.

## Own
`crates/store/src/import.rs` (candidates), `crates/store/src/lib.rs` (only if the refile hook needs it), `crates/pipeline/src/enrich.rs` and the refile trigger, their tests, `docs/metrics.md` (only if a counter or reason is added), and your decisions file. **Don't touch** `crates/search/**`, `bench/**`, `crates/dht/**`, `crates/sources/**` or `web/`: X-search-files-perf, X-polish-1031b and S-settings-2b are running.

## Size
≲ 600 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and the openapi drift check pass. Run `/security-review` on your diff.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
