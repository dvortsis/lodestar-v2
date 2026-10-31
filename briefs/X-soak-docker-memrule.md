# X-soak-docker-memrule: the Linux collector's 1 GiB stop rule measures process memory in Docker mode
**Model:** Opus. Soak evidence and acceptance tooling. Runner brief, 2026-10-31. Small.

## Why
`docs/soak/tools/linux/collect.py` stops collection after five consecutive samples with `working_set_bytes > GIB` (~901, ~925, reason `rss_over_1GiB_for_five_samples`). Docker mode computes `working_set_bytes` from the cgroup: `memory.current − inactive_file` (~347), or the Docker API equivalent (~365). That is what `docker stats` shows.

On Linux the cgroup is charged for **page cache from the container's file reads**. The SQLite and index reads land in `active_file`, which this formula doesn't subtract. So the guardrail counts cache, not process memory. The Windows collector measures `WorkingSet64`, where ReadFile cache isn't charged to the process. Process mode here measures `VmRSS`.

On the rc.2 Unraid soak, Docker working set has reached 1,071 MiB on single samples (max 3 consecutive) while anon memory sits at ~800 MiB. The gap is growing like cache. A trip would interrupt the run, which then needs a re-run under the plan. The binding memory gate (the private-memory slope over hours 12–24, `analyze_cr0.py` MEMORY_FIT) is unaffected.

## Read first
- `docs/soak/tools/linux/README.md` (stop rules, Docker mode)
- `collect.py`: `resources()` for both Docker paths and process mode, and the stop loop
- `test_collect.py`
- `docs/soak/2026-10-25-CR-0.md`, the stop-rule line
- `docs/decisions/2026-10-30-CR-0-acceptance.md`
- `analyze_cr0.py`: `STOP_REASONS`, `rss_max_mib`, MEMORY_FIT

## Do
1. **In Docker mode, apply the 1 GiB × 5 stop rule to process memory, not cgroup working set.**
   - Use the cgroup's `anon` (already read as `private_bytes`). If `memory.stat` breaks it out and it isn't already counted, add `kernel_stack`/`sock`/`shmem` only if the decisions file justifies it. Keep it simple and conservative.
   - Use a new reason string, e.g. `process_memory_over_1GiB_for_five_samples`, so evidence from the two rules can't be confused.
   - Add it to `analyze_cr0.py` `STOP_REASONS`.
   - Process mode keeps today's rule on VmRSS.
   - Record which basis the rule used in `run.json` settings, e.g. `memory_stop_basis: "anon" | "rss"`.
2. **Keep working set as evidence.** It's still recorded every sample. The collector notes (once, in `events.log`) when cgroup working set first passes 1 GiB, and counts such samples in `status.json`/`result.json`. It never stops on it.
3. **A missing `anon` value fails safe:** if `private_bytes` is absent in Docker mode, fall back to today's working-set rule for that sample rather than skipping the check.
4. **Tests in `test_collect.py`:**
   - Docker mode with working set over 1 GiB but anon under it doesn't stop.
   - Anon over 1 GiB for 5 samples stops with the new reason.
   - Process mode is unchanged.
   - The anon-missing fallback.
   - Rehearsal/fixture runs still pass.
   - The analyzer accepts the new reason.
5. **Docs:**
   - Update the README's stop-rule sentence and the Docker metrics table.
   - Write the decisions file `docs/decisions/<date>-X-soak-docker-memrule.md`, flagged under §2.19: it changes how a soak guardrail measures memory in Docker mode, and only for runs started after this merges. The current rc.2 run keeps its collector.

## Own
`docs/soak/tools/linux/{collect.py,test_collect.py,README.md}`, `docs/soak/tools/analyze_cr0.py` (`STOP_REASONS` only, plus its test if one covers reasons), and your decisions file. Don't touch the Windows collector or any crate.

## Size
≲ 250 changed lines.

## Done when
The soak-tools tests pass, i.e. the repo's soak tools CI job, run locally:
- `python3 -I -m pytest docs/soak/tools` or the repo's equivalent;
- `test_collect.py`;
- `test_analyze_cr0.py`.

Run `/security-review` (the collector runs with the Docker socket).

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally: the seven-day usage warning is escalating. Push once per round of fixes.
