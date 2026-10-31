// One hop to the body, as Security's: a dynamic import's preload list sits in the importing chunk,
// so the body's long list is fetched with this module when Logging opens, not with every Settings
// page.
export const load = () => import('./LoggingScreen.svelte');
