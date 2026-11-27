# X-gs-followups-2 decisions

- **Focus after Show all goes to Related's heading.** §10.5 puts focus on the title when a drawer opens, and that still holds for every plain open. Show all asks for a section, so once Related has mounted its heading (`tabindex="-1"`, the one Retry already focuses) takes focus. The drawer's first view only scrolls and leaves the torrent's hash on the host (`data-ask`); Related, which loads on demand, takes focus on mount if the ask is for its torrent and focus is still on the drawer's title (or nowhere), and clears the ask. No observer and no timer. A reader who has moved on, or a different torrent (j/k), is left alone.
- **A withheld answer after Show all hands focus to the drawer's title,** not `<body>`, when the removed section held it.
- **A new query's rows start at row 1 in every view** (`ResultsPanel` `$effect.pre`: the same view, no rows before, rows now). A view change still carries the row.
- **A withheld (404) siblings answer hides Parsed's spans quietly,** as Related hides itself, instead of an error with Retry. It shows no more than before.
- **A siblings answer about another torrent is a failure in the grouped list too** (`expand.svelte.ts`), as `siblings.ts` already treats it.
- **One throwing listener no longer stops the others** hearing a siblings answer; the error is logged. A kept answer's place among the newest is refreshed on read.
- **The Table keeps the row carried into it.** It no longer resets `active` on mount (only on a later new query), so ResultsPanel's `carried` re-set is gone.
- **Size limits (runner §2.26, `web/.size-limit.mjs`, four entry lines).** Search collapse rows 3.05 → 3.08 KB (3,078 B), Detail Related section 5.61 → 5.74 KB (5,734 B), Detail drawer 28.31 → 28.27 KB (28,269 B) and Search results (lazy) 43.78 → 43.76 KB (43,752 B). The focus step lives in Related, so the first view of the drawer does not grow.
