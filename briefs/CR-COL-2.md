# CR-COL-2: the soak analyzer names what kind of run it read
**Model:** Sonnet (Python tooling following an existing pattern; no crawler, store or network code).

## Base and PR timing
Branch `slice/CR-COL-2` from `origin/main`, which has CR-COL (#197) and CR-0f (#200). Open a PR to `main` when done. Follow `.claude/skills/steward/SKILL.md`; the runner merges.

## Why
The v0.5.1 release gate reads `analyze_cr0.py` output from three kinds of run: the 24 h Unraid run, the ≥ 2 h gluetun leg, and rehearsals. Today the report doesn't say which kind it read. A rehearsal or a 2 h leg could then be mistaken for 24 h gate evidence (#197 review I2). The Linux collector already records `settings.leg`, `settings.rehearsal`, `settings.baseline_hours` and, since #200, `settings.intensity` in `run.json`. The Windows collector records some of these, or none.

## Do
1. **`docs/soak/tools/analyze_cr0.py`: a "run" section, at the top of both the Markdown and the JSON report.**
   - It shows `leg` (`main` or `gluetun`), `rehearsal` (true/false), `baseline_required_hours` (from `settings.baseline_hours`), `intensity` and the collector (linux or windows).
   - A field missing from `run.json` is shown as absent (`null`, and "not recorded" in the Markdown), never guessed. Windows evidence keeps working.
2. **A gate verdict line that can't be misread:** `gate_evidence: true` only if all of these hold:
   - not a rehearsal;
   - leg `main` with measured coverage ≥ 24 h; or leg `gluetun` with measured coverage ≥ 2 h, labelled as the gluetun leg;
   - the intensity is recorded.

   Otherwise `gate_evidence: false` with the reasons listed (`rehearsal`, `coverage_below_required`, `intensity_not_recorded`, …). This is an informational summary; it doesn't change any existing pass/fail computation.
3. **Tests in `docs/soak/tools/test_analyze_cr0.py`:**
   - one fixture each for main 24 h, a gluetun 2 h leg, a rehearsal, a Windows run with no `settings` fields, and a run that's too short;
   - assert both the JSON fields and the Markdown lines;
   - no fixed sleeps.
4. **CI:** the `soak tools` job already runs `docs/soak/tools/linux`. Change CI only if a test needs it, and name the change in the PR.

## Own
`docs/soak/tools/analyze_cr0.py`, `docs/soak/tools/test_analyze_cr0.py`, and test fixtures under `docs/soak/tools/`. If `docs/soak/tools/linux/README.md` should mention the new section, that one-line edit is allowed.

## Done when
`python3 -m unittest discover -s docs/soak/tools -p 'test_*.py'` and the same for `docs/soak/tools/linux` pass on Python 3.12. The steward self-review is done. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number. No model identifiers in commits, PRs or code.
