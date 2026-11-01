// One hop to the body, as Security's: its preload list is fetched when Updates opens, not with every
// Settings page.
export const load = () => import('./UpdatesScreen.svelte');
