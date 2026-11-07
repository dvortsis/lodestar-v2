// The one hop to the dialogs' code, so the preload lists of their chunks stay out of the screen's
// chunk (as `body.ts` does for the screen itself). Loaded on first use by `SourcesScreen.svelte`.
export const consentDialog = () => import('./ConsentDialog.svelte').then((m) => m.default);
export const purgeDialog = () => import('./PurgeDialog.svelte').then((m) => m.default);
export const configDialog = () => import('./ConfigDialog.svelte').then((m) => m.default);
