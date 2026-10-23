// The screens this slice builds (§10.10.8), in the shape the shell's registry finds. Every one is
// generated from the schema: `sections` and no `load`. Imports stay tiny (shell/README.md); their
// icons are in shell/icons.ts, which the navigation loads after first paint.
import { m } from '$lib/paraglide/messages.js';
import type { SettingsScreen } from '../shell/registry';

export const screens: SettingsScreen[] = [
	{
		id: 'crawler',
		group: 'crawling',
		title: () => m.settings_ui_screen_crawler_title(),
		sections: ['crawler', 'transport']
	},
	{
		id: 'network',
		group: 'crawling',
		title: () => m.settings_ui_screen_network_title(),
		sections: ['network']
	},
	{
		id: 'storage',
		group: 'library',
		title: () => m.settings_ui_screen_storage_title(),
		sections: ['storage']
	},
	{
		id: 'search',
		group: 'library',
		title: () => m.settings_ui_screen_search_title(),
		sections: ['search']
	},
	{
		id: 'general',
		group: 'system',
		title: () => m.settings_ui_screen_general_title(),
		sections: ['general']
	}
];
