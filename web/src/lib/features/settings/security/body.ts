// One hop to the body: a dynamic import's preload list sits in the importing chunk, so the body's
// long list is fetched with this module when Security opens, not with every Settings page.
export const load = () => import('./SecurityScreen.svelte');
