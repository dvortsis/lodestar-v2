// Settings → Companions' navigation icon and purpose line, loaded after the navigation shows (the
// purpose line also when the Settings filter is first used). One module for both keeps the
// registry entry small (shell/screens.ts).
import { m } from '$lib/paraglide/messages.js';
import type { About } from '../shell/screens';

export { default } from '~icons/ph/puzzle-piece';
export const ABOUT: Record<string, About> = {
	companions: { purpose: m.settings_companions_purpose }
};
