// The purpose lines of this file's screens (screen.ts) that the registry shows or matches: Trackers'
// under its generated heading, and Language & region's for the Settings filter. Loaded when one
// opens or the filter is first used, so no Settings first view carries them (`Screen.about`).
import { m } from '$lib/paraglide/messages.js';
import type { About } from '../shell/screens';

export const ABOUT: Record<string, About> = {
	language: { purpose: m.settings_screen_language_purpose },
	trackers: { purpose: m.settings_screen_trackers_purpose }
};
