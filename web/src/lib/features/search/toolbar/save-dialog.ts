// One hop to Save search's dialog: a dynamic import's preload list (here Bits UI's overlay code,
// ~30 files) sits in the importing chunk, so it is fetched with this module on the first press,
// not with the toolbar on every search (X-alert-search-headroom).
export const load = () => import('$lib/features/alerts/SaveSearchDialog.svelte');
