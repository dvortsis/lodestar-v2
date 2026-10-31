// The screens generated from the schema (§10.10.8), in the shape the shell's registry finds
// (shell/screens.ts). Only implemented destinations belong here: Security, filters and address
// changes need their own guarded interactions, so a schema alone does not make them writable.
import IconGeneral from '~icons/ph/gear';
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

// One lazy module each for the purpose lines and the icons, shared by every entry (one preload
// list each).
const about = () => import('./about').then((mod) => mod.ABOUT);
const icons = () => import('./icons');

export const screens: Screen[] = [
	{
		id: 'crawler',
		group: 'crawling',
		title: () => m.settings_shell_crawler(),
		about,
		iconLoad: () => icons().then((mod) => ({ default: mod.crawler })),
		sections: ['crawler', 'transport']
	},
	{
		id: 'network',
		group: 'crawling',
		title: () => m.settings_shell_network(),
		about,
		iconLoad: () => icons().then((mod) => ({ default: mod.network })),
		sections: ['network'],
		readOnly: true
	},
	{
		id: 'storage',
		group: 'library',
		title: () => m.settings_shell_storage(),
		about,
		iconLoad: () => icons().then((mod) => ({ default: mod.storage })),
		sections: ['storage']
	},
	{
		id: 'search',
		group: 'library',
		title: () => m.settings_shell_search(),
		about,
		iconLoad: () => icons().then((mod) => ({ default: mod.search })),
		sections: ['search']
	},
	{
		id: 'general',
		group: 'system',
		title: () => m.settings_shell_general(),
		about,
		icon: IconGeneral,
		sections: ['general'],
		extra: () => import('../shell/SettingsExport.svelte')
	}
];
