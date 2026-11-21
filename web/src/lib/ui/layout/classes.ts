// The page frame's class strings, in one place (§10.2). `PageFrame` and `PageHeader` use them, and
// so does the Search route directly: its first view is held to a byte budget (§10.1), and two
// components cost it about 0.9 kB that these plain strings don't.
/** The frame's gutters: 16 px, 24 px from `large`; none in print. */
export const PAGE_FRAME = 'flex w-full flex-1 flex-col px-4 py-4 large:px-6 large:py-6 print:p-0';
/** The frame's content box, to which a width cap is added: the whole width by default. */
export const PAGE_FRAME_BODY = 'flex w-full flex-1 flex-col gap-4';
export const PAGE_WIDE = 'max-w-7xl';
export const PAGE_MANUSCRIPT = 'max-w-(--manuscript-width)';
/** The page's one `h1`. */
export const PAGE_H1 = 'text-display font-semibold wrap-anywhere';
