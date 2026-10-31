// The Appearance settings screen (§10.10.8): theme, mode, density, motion, illustrations and Show
// adult content. A hand-built screen, loaded when it opens; the theme picker and the custom theme
// editor are separate chunks inside it.
import { m } from '$lib/paraglide/messages.js';

export const screen = {
	id: 'appearance',
	group: 'preferences',
	title: () => m.appearance_title(),
	load: () => import('./AppearanceScreen.svelte')
};

export default screen;
