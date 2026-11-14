// Settings → Outputs' navigation icon and purpose line, loaded after the navigation shows (the
// purpose line also when the Settings filter is first used). One module for both keeps the
// registry entry small (shell/screens.ts).
import { m } from '$lib/paraglide/messages.js';
import type { About } from '../shell/screens';

export { default } from '~icons/ph/share-network';
export const ABOUT: Record<string, About> = {
	outputs: { purpose: m.settings_outputs_purpose }
};
