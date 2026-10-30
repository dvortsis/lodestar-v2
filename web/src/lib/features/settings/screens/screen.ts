// The screens generated from the schema (§10.10.8), in the shape the shell's registry finds
// (shell/screens.ts). Only implemented destinations belong here: Security, filters and address
// changes need their own guarded interactions, so a schema alone does not make them writable.
import IconCrawler from '~icons/ph/broadcast';
import IconNetwork from '~icons/ph/globe';
import IconStorage from '~icons/ph/hard-drives';
import IconSearch from '~icons/ph/magnifying-glass';
import IconGeneral from '~icons/ph/gear';
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

export const screens: Screen[] = [
	{
		id: 'crawler',
		group: 'crawling',
		title: () => m.settings_shell_crawler(),
		purpose: () => m.settings_shell_crawler_purpose(),
		icon: IconCrawler,
		sections: ['crawler', 'transport']
	},
	{
		id: 'network',
		group: 'crawling',
		title: () => m.settings_shell_network(),
		purpose: () => m.settings_shell_network_purpose(),
		icon: IconNetwork,
		sections: ['network'],
		readOnly: true,
		notice: () => m.settings_shell_network_readonly()
	},
	{
		id: 'storage',
		group: 'library',
		title: () => m.settings_shell_storage(),
		purpose: () => m.settings_shell_storage_purpose(),
		icon: IconStorage,
		sections: ['storage']
	},
	{
		id: 'search',
		group: 'library',
		title: () => m.settings_shell_search(),
		purpose: () => m.settings_shell_search_purpose(),
		icon: IconSearch,
		sections: ['search']
	},
	{
		id: 'general',
		group: 'system',
		title: () => m.settings_shell_general(),
		purpose: () => m.settings_shell_general_purpose(),
		icon: IconGeneral,
		sections: ['general'],
		extra: () => import('../shell/SettingsExport.svelte')
	}
];
