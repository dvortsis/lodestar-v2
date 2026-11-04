# X-thp-off: stop transparent huge pages inflating the worker's resident memory (r4 contingency, memory fix 3)
**Model:** Opus. Core: the process and allocator, Linux-specific. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04; auto-approved under owner §2.33. This is Track R priority.

## Why (evidence from soak r4, rc.3, baseline hour about 17, read-only host reads)
- The worker (`lodestar serve --child`) has RssAnon 492 MiB, of which **AnonHugePages is 212 MiB, 44%**. RssFile is only 14 MiB, so this isn't index mmaps.
- X-merge-policy's A/B (PR pending, `docs/decisions/2026-11-04-X-merge-policy.md`) measured the whole indexing path at about 2.7 KiB retained per torrent at a soak-sized corpus, with a flat dhat live heap. r4's provisional slope is about 19 KiB per torrent. So the RSS growth isn't live heap; it's retained or inflated pages.
- mimalloc (v3, `crates/lodestar/src/main.rs:43-44`, `libmimalloc-sys` defaults) has `allow_thp` on. With the host's THP set to "always", khugepaged collapses sparse 2 MiB ranges of mimalloc arenas into huge pages, so resident memory grows over time even when the heap doesn't. That fits r3 and r4's slow climb and steps.

## Do
1. **At the very start of `main`** (before the runtime and the allocator's arenas grow; ideally before any thread spawns) on Linux:
   - call `prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0)`. It's inherited by the `--child` worker across fork and exec, and verified on the child;
   - **and** set mimalloc's `allow_thp` off: through `mi_option_set` if the crate exposes it, otherwise through `MIMALLOC_ALLOW_THP=0` set for the child's environment when the supervisor spawns it.
   - Document which takes effect, and keep both if they're cheap.
   - Non-Linux builds compile it out.
2. **A setting for the escape hatch:** an environment variable (for example `LODESTAR_THP=allow`) restores today's behaviour. The default is off. Record it in docs/user/ (the settings appendix) and in the decisions file.
3. **Tests:**
   - a Linux test that `prctl(PR_GET_THP_DISABLE)` reads 1 in the worker after start (spawn the real binary through its test harness);
   - unit tests for the env override.
4. **Measure locally with the existing harness** (`bench/examples/mem-ingest.rs` or X-merge-policy's `mem-merge.rs`, if merged): 20k torrents at 1 doc per commit, musl and gnu, recording AnonHugePages and Anonymous from smaps_rollup over time, THP on against off. Report it in the PR. Say plainly that the container's THP mode may differ from Unraid's; the soak is the acceptance.
5. **Decisions file:** `docs/decisions/2026-11-04-X-thp-off.md`, with the evidence above, the mechanism, the escape hatch, the performance check (search and ingest latency unchanged within noise), and the delta note: this is a process-level change, and soak r5 is the acceptance.

## Own
`crates/lodestar/src/main.rs` (the startup lines only), the supervisor's child-spawn env in `crates/lodestar/src/` (name the file in the PR), a new `crates/lodestar/src/thp.rs`, its tests, the settings doc row, and your decisions file.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass (Windows and macOS compile it out), the measurement is in the PR, and a fresh self-review is done.

Branch `slice/X-thp-off`. The PR is titled "X-thp-off: Disable transparent huge pages for the worker by default". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
