// One hop to the body, as Security's and Logging's: a dynamic import's preload list sits in the
// importing chunk, so About's list is fetched with this module when About opens, not with every
// Settings page.
export const load = () => import('./AboutScreen.svelte');
