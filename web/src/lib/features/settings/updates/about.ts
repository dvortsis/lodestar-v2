// Updates' purpose line, for the Settings filter (`Screen.about`). Loaded when the filter is first
// used, so no Settings first view carries it.
import { m } from '$lib/paraglide/messages.js';
import type { About } from '../shell/screens';

export const ABOUT: Record<string, About> = {
	updates: { purpose: m.settings_updates_purpose }
};
