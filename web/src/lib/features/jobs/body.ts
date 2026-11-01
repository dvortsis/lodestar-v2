// One hop to the body, as Security's and Logging's: a dynamic import's preload list sits in the
// importing chunk, so the body's list is fetched with this module when Jobs opens, not with every
// Settings page.
export const load = () => import('./JobsScreen.svelte');
