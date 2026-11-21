// The page frame (§10.2) as class strings, used by every destination's wrapper and `h1`. They are
// strings, not components, because a compiled component costs about 0.75 kB gzipped in every
// route that uses it (X-page-frame decisions, runner call), and §10.1 caps route JS. Steps are on
// the §10.2 spacing ramp; widths and colours are tokens or Tailwind's own scale.
import { FOCUS_RING } from '../classes';

/** The frame's gutters: 16 px, 24 px from `large`; none in print. Left-aligned, never centred. */
export const PAGE_FRAME = 'flex w-full flex-1 flex-col px-4 py-4 large:px-6 large:py-6 print:p-0';
const BODY = 'flex w-full flex-1 flex-col gap-4';
/** The frame's content box: the whole width (Search, Insights, Logs, the Settings shell). */
export const PAGE_BODY_FULL = BODY;
/** Up to 80 rem (Alerts, Compare, Discover, Detail). */
export const PAGE_BODY_WIDE = `${BODY} max-w-7xl`;
/** Up to `--manuscript-width` (the unavailable page). */
export const PAGE_BODY_MANUSCRIPT = `${BODY} max-w-(--manuscript-width)`;
/** The heading row, when the page puts something (actions) beside its `h1`. */
export const PAGE_HEADER = 'flex flex-wrap items-center justify-between gap-x-4 gap-y-2';
/** The page's one `h1`; give it `tabindex="-1"` and an `id` where a page moves focus to it. */
export const PAGE_H1 = `text-display font-semibold wrap-anywhere ${FOCUS_RING}`;
