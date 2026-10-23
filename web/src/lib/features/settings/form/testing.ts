// Test helpers: values and specs the way the server sends them.
import type { FieldSpec, SectionValues, SettingSource, SettingValue } from './types';

export const sv = (
	value: unknown,
	source: SettingSource = 'default',
	more: Partial<SettingValue> = {}
): SettingValue => ({
	env: null,
	read_only: false,
	restart_required: false,
	source,
	value,
	...more
});

export const values = (
	section: string,
	revision: number,
	fields: Record<string, SettingValue>,
	restart: string[] = []
): SectionValues => ({ section, revision, fields, restart_required: restart });

export const spec = (key: `${string}.${string}`, over: Partial<FieldSpec> = {}): FieldSpec => {
	const [section, name] = key.split('.') as [string, string];
	return {
		key,
		section,
		name,
		labelKey: `settings.${section}.${name}.label`,
		helpKey: `settings.${section}.${name}.help`,
		kind: 'string',
		preview: false,
		group: 'g',
		order: 0,
		advanced: false,
		writeOnly: false,
		readOnly: false,
		restartRequired: false,
		...over
	};
};
