// The screens this slice builds (§10.10.8), in the shape the shell's registry finds. Every one is
// generated from the schema: `sections` and no `load`. Imports stay tiny (shell/README.md).
import { m } from '$lib/paraglide/messages.js';
import IconCrawler from '~icons/ph/gauge';
import IconGeneral from '~icons/ph/sliders';
import IconNetwork from '~icons/ph/network';
import IconSearch from '~icons/ph/magnifying-glass';
import IconStorage from '~icons/ph/hard-drives';
import type { SettingsScreen } from '../shell/registry';

export const screens: SettingsScreen[] = [
	{
		id: 'crawler',
		group: 'crawling',
		title: () => m.settings_ui_screen_crawler_title(),
		sections: ['crawler', 'transport'],
		icon: IconCrawler
	},
	{
		id: 'network',
		group: 'crawling',
		title: () => m.settings_ui_screen_network_title(),
		sections: ['network'],
		icon: IconNetwork
	},
	{
		id: 'storage',
		group: 'library',
		title: () => m.settings_ui_screen_storage_title(),
		sections: ['storage'],
		icon: IconStorage
	},
	{
		id: 'search',
		group: 'library',
		title: () => m.settings_ui_screen_search_title(),
		sections: ['search'],
		icon: IconSearch
	},
	{
		id: 'general',
		group: 'system',
		title: () => m.settings_ui_screen_general_title(),
		sections: ['general'],
		icon: IconGeneral
	}
];
