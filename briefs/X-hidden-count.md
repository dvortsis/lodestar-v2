# X-hidden-count (runner brief, 2026-11-28)

**Slice:** X-hidden-count (runner/slices.yaml). Opus, core.
**Branch:** `slice/X-hidden-count` from current `main`.
**PR title (exact):** `X-hidden-count: P7's other counts (2,208 torrents, 2.2K) say what they count`
**Owns:** `docs/decisions/*-X-hidden-count.md`, plus files refined at step 0.

## Source
UI review 2026-11-13. `docs/review/2026-11-13-ui-ux/01-review.md` P7: "2,196 results", "2,208
torrents" and "2.2K" appear together with no "N hidden · Show". `02-skills-pass…md:66`: browse all
returns 2,196 while searchable is 2,208, so twelve are hidden.

X-search-toolbar (#1075) built the "N hidden · Show" part. This slice is the rest of P7 (runner
records r14): the other counts must say what they count.

## Do
1. **Step 0.** Locate every place these counts are shown (search toolbar or index banner, status
   pill, Overview, wizard), and where each number comes from (API field, metric). Send the runner:
   - what each one counts today, and the label or source fix for each;
   - the refined owns.

   Then wait for the reply. Fix at the source: if two numbers mean different things, label
   them. If one is plainly wrong, correct the number.
2. **Avoid while other slices run:**
   - `web/src/lib/features/search/query/**`, `assist/**`, `results/ResultsPanel.svelte` and
     `results/Rescue.svelte` (X-zero-rescue is running);
   - `crates/api/src/dto/torrents.rs` (X-detail-attributes).

   If you need one of these, say so at step 0.
3. Follow the UI rules (CLAUDE.md), the `docs/ui-glossary.md` terms, and en and zh messages.
   Write unit tests and, where a screen changes, update its e2e spec. Condition waits only.
4. Decisions file: `docs/decisions/<date>-X-hidden-count.md`.
5. Follow `.claude/skills/steward/SKILL.md`. Report `ready <PR> <sha>` to the runner session.
