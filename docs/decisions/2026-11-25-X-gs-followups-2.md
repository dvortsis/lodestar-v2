# X-gs-followups-2 decisions

- **Focus after Show all goes to Related's heading.** §10.5 puts focus on the title when a drawer opens, and that still holds for every plain open. Show all asks for a section, so once Related has mounted and scrolled into view its heading (`tabindex="-1"`, the one Retry already focuses) takes focus. A keyboard or screen-reader user lands where the click took them. `revealSection` in `section.svelte.ts` does both steps and gives up after 5 s if no heading appears (a withheld answer shows nothing).
- **A withheld (404) siblings answer hides Parsed's spans quietly,** as Related hides itself, instead of an error with Retry. It shows no more than before.
- **A siblings answer about another torrent is a failure in the grouped list too** (`expand.svelte.ts`), as `siblings.ts` already treats it.
- **One throwing listener no longer stops the others** hearing a siblings answer; the error is logged. A kept answer's place among the newest is refreshed on read.
- **The Table keeps the row carried into it.** It no longer resets `active` on mount (only on a later new query), so ResultsPanel's `carried` re-set is gone.
