// The Keyboard settings screen, in the shape the settings route is expected to find it: a feature
// folder exports `{ id, group, title, load }` and the route lazy-loads the component. Proposed to
// S-settings-1, which defines the screen registry.
import { m } from '$lib/paraglide/messages.js';

export const screen = {
	id: 'keyboard',
	group: 'preferences',
	title: () => m.shortcuts_screen_title(),
	load: () => import('./KeyboardScreen.svelte')
};

export default screen;
