# X-analyzer-memview: a corpus-normalised memory view in analyze_cr0.py
**Model:** Sonnet (Python tooling, stats). The runner reviews the PR; an Opus reviewer only if the runner asks. Runner brief, 2026-11-01. Owner-approved (Unraid re-run plan, item 5).

## Why
The Unraid soak's leg 1 (2026-10-31) read a private-memory slope of 1.85 MiB/h, CI [0.92, 2.77], over a partial hours-12–24 window. The owner's bar is upper CI ≤ 2.0 MiB/h. The store grew at 669 torrents/h, so the slope may be index growth (about 2.8 KB per stored torrent), not a leak. The re-run's baseline ends 2026-11-02T08:39Z. The analyzer should report a second view that helps tell the two apart. **It must not change the gate:** the owner's bar and its verdict stay exactly as they are.

## Read first
- `docs/soak/tools/analyze_cr0.py` (`MEMORY_FIT`, `memory_fit()`, `markdown()`), `docs/soak/tools/test_analyze_cr0.py`, `docs/soak/tools/README.md`.
- `docs/decisions/2026-10-30-CR-0-acceptance.md` (the binding memory gate, owner #189).
- What a sample row carries: the collector `docs/soak/tools/linux/collect.py` and the parsed metrics (`writer_stored_total`, any stored-torrent gauge, `private_bytes`).

## Do
1. Add an **informational** block next to the memory gate, clearly labelled "not part of the gate":
   - the OLS slope of private memory against **stored torrents** over the same hours-12–24 window (MiB per 1,000 stored), with a 95% CI;
   - the stored-torrent growth over the window (torrents/h), and the hours-12–24 private slope that the store growth alone would explain at that per-torrent cost;
   - a two-variable view if it's simple and robust: private ≈ a + b·hours + c·stored, reporting b (time-only growth, the leak signal) with its CI, and saying plainly that the samples aren't independent and the two regressors are correlated, so this is a hint, not a test.
   - If stored torrents aren't in the samples, say so and skip the block; never invent a number.
2. Keep the existing `memory_fit` output and the gate verdict byte-for-byte identical. Add a regression test that the gate's JSON and markdown don't change for the existing fixtures.
3. Tests in `test_analyze_cr0.py`: synthetic series where memory tracks stored torrents (b ≈ 0, c > 0), where it grows with time alone (b > 0), and one with no stored data (block skipped). Pure Python, no new dependencies.
4. Document the block in `docs/soak/tools/README.md`, one short paragraph.
5. No raw evidence leaves the private directory: only aggregates, as the analyzer does now.

## Own
`docs/soak/tools/analyze_cr0.py`, `docs/soak/tools/test_analyze_cr0.py`, `docs/soak/tools/README.md`. Nothing else.

## Size
≲ 300 changed lines.

## Done when
`python3 -m pytest docs/soak/tools/test_analyze_cr0.py` (or the way CI's soak-tools job runs it) passes, and the CI `soak tools` job is green.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
