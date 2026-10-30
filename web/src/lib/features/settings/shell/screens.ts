import { m } from '$lib/paraglide/messages.js';
import type { FieldSpec } from '../form/types';
import type { SettingsIndexView } from '../form/schema';

export const groups = [
	{ id: 'crawling', label: () => m.settings_shell_crawling() },
	{ id: 'library', label: () => m.settings_shell_library() },
	{ id: 'system', label: () => m.settings_shell_system() }
] as const;

export interface Screen {
	id: string;
	group: (typeof groups)[number]['id'];
	label: () => string;
	purpose: () => string;
	sections: string[];
}

// Only implemented destinations belong here. Security, filters and address changes
// need their own guarded interactions; a schema alone does not make them writable.
export const screens: readonly Screen[] = [
	{
		id: 'crawler',
		group: 'crawling',
		label: () => m.settings_shell_crawler(),
		purpose: () => m.settings_shell_crawler_purpose(),
		sections: ['crawler', 'transport']
	},
	{
		id: 'network',
		group: 'crawling',
		label: () => m.settings_shell_network(),
		purpose: () => m.settings_shell_network_purpose(),
		sections: ['network']
	},
	{
		id: 'storage',
		group: 'library',
		label: () => m.settings_shell_storage(),
		purpose: () => m.settings_shell_storage_purpose(),
		sections: ['storage']
	},
	{
		id: 'search',
		group: 'library',
		label: () => m.settings_shell_search(),
		purpose: () => m.settings_shell_search_purpose(),
		sections: ['search']
	},
	{
		id: 'general',
		group: 'system',
		label: () => m.settings_shell_general(),
		purpose: () => m.settings_shell_general_purpose(),
		sections: ['general']
	}
];

export function screenFields(screen: Screen, index: SettingsIndexView): FieldSpec[] {
	return index.fieldsOf(screen.sections).map((spec) => ({
		...spec,
		readOnly:
			spec.readOnly ||
			screen.id === 'network' ||
			(spec.danger !== undefined && spec.danger !== 'disruptive')
	}));
}

/** Navigation matching does not load any setting values or private data. */
export function matchesScreen(screen: Screen, query: string): boolean {
	const normalize = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLocaleLowerCase();
	const haystack = normalize(`${screen.label()} ${screen.purpose()}`);
	return normalize(query)
		.trim()
		.split(/\s+/)
		.every((word) => haystack.includes(word));
}
