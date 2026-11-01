// The Overview's only reference to the privacy banner: a door with no dependencies, so the
// Overview's chunk lists one file to preload, not the banner's whole graph (§10.1).
export const load = () => import('./PrivacyBanner.svelte');
