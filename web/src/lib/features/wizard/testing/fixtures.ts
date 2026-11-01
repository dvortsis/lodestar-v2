// Tests only (vitest and Playwright): the settings the wizard's steps read, shaped as B2b's
// golden index sends them, and a fetch wrapper that answers `GET /settings` with it.
import type { components } from '../../../api/schema';

type Index = components['schemas']['SettingsIndex'];
type Values = components['schemas']['SectionValues'];
type Value = components['schemas']['SettingValue'];

const field = (
	section: string,
	name: string,
	over: Partial<Index['sections'][number]['fields'][number]> = {}
) => ({
	name,
	env: null,
	label_key: `settings.${section}.${name}.label`,
	help_key: `settings.${section}.${name}.help`,
	read_only: false,
	restart_required: false,
	...over
});

export const WIZARD_INDEX: Index = {
	sections: [
		{
			section: 'crawler',
			label_key: 'settings.crawler.label',
			help_key: 'settings.crawler.help',
			fields: [field('crawler', 'intensity')],
			schema: {
				type: 'object',
				properties: {
					intensity: {
						type: 'string',
						enum: ['low', 'balanced', 'aggressive'],
						'x-group': 'intensity',
						'x-order': 0,
						'x-widget': 'radio',
						'x-config': 'dht.intensity',
						'x-env': 'LODESTAR_INTENSITY'
					}
				}
			} as never
		},
		{
			section: 'storage',
			label_key: 'settings.storage.label',
			help_key: 'settings.storage.help',
			fields: [
				field('storage', 'mmap_mib', { restart_required: true }),
				field('storage', 'archive_raw', { restart_required: true })
			],
			schema: {
				type: 'object',
				properties: {
					mmap_mib: {
						type: 'integer',
						minimum: 0,
						maximum: 65536,
						'x-unit': 'mebibytes',
						'x-group': 'database',
						'x-order': 0,
						'x-advanced': true
					},
					archive_raw: { type: 'boolean', 'x-group': 'database', 'x-order': 1 }
				}
			} as never
		},
		{
			section: 'updates',
			label_key: 'settings.updates.label',
			help_key: 'settings.updates.help',
			fields: [field('updates', 'mode', { env: 'LODESTAR_UPDATES' })],
			schema: {
				type: 'object',
				properties: {
					mode: {
						type: 'string',
						enum: ['off', 'notify', 'auto', 'image'],
						'x-group': 'updates',
						'x-order': 0,
						'x-widget': 'radio',
						'x-env': 'LODESTAR_UPDATES'
					}
				}
			} as never
		}
	]
};

const v = (value: unknown, over: Partial<Value> = {}): Value => ({
	env: null,
	read_only: false,
	restart_required: false,
	source: 'default',
	value,
	...over
});

export const wizardValues = (over: { updates?: Value } = {}): Values[] => [
	{
		section: 'crawler',
		revision: 3,
		restart_required: [],
		fields: { intensity: v('balanced') }
	},
	{
		section: 'storage',
		revision: 2,
		restart_required: [],
		fields: {
			mmap_mib: v(1024, { restart_required: true }),
			archive_raw: v(false, { restart_required: true })
		}
	},
	{
		section: 'updates',
		revision: 1,
		restart_required: [],
		fields: { mode: over.updates ?? v('off', { env: 'LODESTAR_UPDATES' }) }
	}
];

export const lockedUpdates = (): Value =>
	v('notify', { source: 'env', env: 'LODESTAR_UPDATES', read_only: true });
