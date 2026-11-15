# X-filter-scratch-trim: bound the joined reading's per-thread Scratch

**Model:** Opus (filter, per-thread state). **Branch:** `slice/X-filter-scratch-trim`. **PR title:** `X-filter-scratch-trim: <title>`.

## Why
Found in X-filter-split-nits (#953): the per-thread `Scratch` behind the joined reading in `Core::pattern_signals` is never trimmed, unlike `ReadBufs`. A thread that once judged a text at the token cap keeps its buffers, about 12 MB, until the thread ends. Across the judge pool that's a few tens of MB held for nothing. This is a memory-retention finding, so best practice applies: bound it.

## Steps
1. Trim `Scratch` after use the way `ReadBufs` does: same threshold style, `shrink_to` or replace when capacity is past a bound. Bring the buffers back to a small steady capacity after a large text.
2. **Performance:** run release `budget_worst_case` and the filter bench before and after, alternating the binaries (as you did for #953). The worst case and the typical shape must not regress beyond noise. If trimming costs measurable time on the hot path, trim only when capacity is above the bound.
3. **Test:** after judging a cap-length text on a thread, the thread's `Scratch` capacity is back under the bound. It must fail without the trim. No fixed sleeps.
4. Write a decisions file, `docs/decisions/2026-11-15-X-filter-scratch-trim.md`, with the bound, the numbers, and why the threshold matches `ReadBufs`.

## Rules
- Don't change filter verdicts, budgets, `BUDGET` or the token caps. The parity and golden tests must pass unchanged.
- Before the PR: fmt, clippy `-D warnings` (Linux and `x86_64-pc-windows-gnu`), the filter tests.
- Add your own `runner/slices.yaml` entry and placement rule only. Run `npm test` in `tools/status-connector`.
- Use the steward skill. Report `ready <sha>`.
