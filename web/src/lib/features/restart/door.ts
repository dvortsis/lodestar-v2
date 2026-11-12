// The one way the banner reaches the rest of the flow: a single import, so a Settings first view
// carries one small preload list instead of one per part. Nothing here is loaded until a change
// waits for a restart and its button, Review or the address prompt is needed.
export const parts = {
	review: () => import('./ReviewList.svelte').then((m) => m.default),
	dialog: () => import('./RestartDialog.svelte').then((m) => m.default),
	prompt: () => import('./AddressPrompt.svelte').then((m) => m.default),
	restarting: () => import('./RestartingPage.svelte').then((m) => m.default)
};
