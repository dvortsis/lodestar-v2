// Settings → About's purpose line, for the Settings filter (`Screen.about`). Loaded when the filter
// is first used, so no Settings first view carries it. About's own page shows its lead itself.
import { m } from '$lib/paraglide/messages.js';
import type { About } from '../shell/screens';

export const ABOUT: Record<string, About> = {
	about: { purpose: m.settings_about_purpose }
};
