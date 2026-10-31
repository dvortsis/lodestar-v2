// The Settings screens built by S-settings-2 (§10.10.8). Language & region only for now: the
// others wait on server sections (docs/decisions/2026-10-31-S-settings-2.md). The body loads
// when the screen opens.
import IconLanguage from '~icons/ph/translate';
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

export const screens: Screen[] = [
	{
		id: 'language',
		group: 'preferences',
		title: () => m.settings_language_title(),
		purpose: () => m.settings_language_purpose(),
		icon: IconLanguage,
		load: () => import('./LanguageScreen.svelte')
	}
];

export default screens;
