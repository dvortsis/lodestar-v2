# RP-1b: MW-2@2 and FK-2@2, the two guards RP-1's precision corpus found missing
**Model:** Opus (content-safety matching). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T08:58Z.

## Why
§20.6's guard column names two false-positive guards that the shipped rules don't have. The section "Known gaps" in `docs/decisions/2026-11-04-RP-1.md` describes them:
- **MW-2, music-production packs (VST and sample libraries).** MW-2@1 hides a pack that the classifier calls Music when it ships an installer, for example `Example Synth VST Collection v2.1 x64` or `Example Drum Samples Vol 3 WAV`.
- **FK-2, short-episode packs.** FK-2@1 flags a pack of short HD episodes when each episode is under the 40 MiB tiny-video threshold, for example `Example Shorts S01 1080p WEB`.

Both are recorded as `gap` lines in `crates/filter/data/packs/precision.txt`.

## Do (branch `slice/RP-1b` from `origin/main`; RP-UC (#469) has merged)
1. **Add both guards as new rule versions, MW-2@2 and FK-2@2** (RP-0's versioning). They're narrows, so each version records its direction as `narrows`. On existing installs, RP-3's acceptance flow (in review as #478) then treats accepting them as weakening, which needs the password. That's intended.
2. **Keep the guards as tight as the evidence allows.**
   - **MW-2:** a music-production pack has mostly audio and preset files, and its installer sits beside them. Don't let it become a way to slip malware past MW-2. A lone `setup.exe` with one `.wav` must still be held. Base the guard on the audio and preset share, and on recognised plug-in or sample-library structure (`.vst3`, `.dll` beside presets, `.nki`, `.fxp`, sample folders), and state the thresholds.
   - **FK-2:** a short-episode pack has several episode-numbered files of similar size from one series (`SxxEyy`). A single tiny HD file must still be flagged.
3. **Corpus and precision set.**
   - Remove the `gap` lines and add must-match cases so the malware and fake cases are still caught.
   - Report precision for both rules on the release-parser golden corpus and the pack corpora.
   - The child-safety core and MW-6 are unaffected.
4. **Budget.** Run the filter budget test and report the numbers. The headroom is already thin: calibrated mixed is 48–49.5 µs against 50. If the guards push it over, make them cheaper. Don't raise the budget.
5. **Decisions file:** `docs/decisions/2026-11-05-RP-1b.md`.

## Own
- `crates/filter/src/packs/mod.rs`: only the MW-2 and FK-2 rules and their versions;
- `crates/filter/data/packs/precision.txt`;
- the malware and fakes pack corpora under `crates/filter/data/packs/`;
- their tests;
- the decisions file.

Don't touch `crates/filter/src/engine.rs` or `crates/filter/src/packs/unauthorised_copy.rs`: the filter perf slice (X-filter-perf-1105) is working there now.

## Done when
Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-filter`, the api goldens, and the release filter budget. If disk space is tight, test per crate and say so. Then run `/security-review`.

Open the PR against `main`, titled "RP-1b: MW-2@2 and FK-2@2: the two guards RP-1's precision corpus found missing". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, the precision numbers and the budget numbers, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
