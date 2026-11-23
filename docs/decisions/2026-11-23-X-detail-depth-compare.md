# X-detail-depth-compare decisions (PR 2: Compare "Differences only")

Runner brief, 2026-11-23Z. P11 in the 2026-11-13 UI review, 02 §4e, architecture §10.6.

## What changed
- **A "Differences only" switch** above the comparison, in both layouts (columns and the attribute-first list). A native checkbox with `role="switch"`, so its state is announced. Off by default and **not remembered**: §10.6 and the glossary don't say to keep it.
- **When on, rows where every torrent shows the same value are hidden.** Equality is on the formatted text the cell shows (`isSame` in `model.ts`): two nulls ("—") are the same, null against a value differs, and a composition row compares its summary text. The server's `differs` flag still drives only the "differs" badge; the two are separate on purpose, since a formatted value can match when the raw one doesn't.
- **A polite live region** under the switch says "N rows hidden because they're the same" (singular form too, en and zh). It is empty while the switch is off or nothing is hidden. The Actions row is never hidden or counted.
- **When every row is the same,** a plain sentence replaces the table, and the switch stays in place and keeps focus.
- Filtering only changes the rows array: focus stays on the switch, and the page's scroll position isn't set (the browser clamps it if the page gets shorter).

## Size
- **Compare route 31.14 → 31.79 KB (measured 31,781 B)** (runner, §2.26, as the brief allows): the switch, the count line, the equality check and four messages in both languages. The first try with the shared `Toggle` measured 33.06 KB (its `bind:checked` pulls Svelte runtime helpers the route doesn't otherwise load), so the switch is a plain checkbox with an `onchange` handler.

## Not done
- No glossary entry for the new strings; the wording follows the glossary tone, and zh wants the owner's usual review.
