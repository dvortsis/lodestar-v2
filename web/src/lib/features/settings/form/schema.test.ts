import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { components } from '$lib/api/schema';
import { fieldAnchor, groupFields, indexView, readSection } from './schema';
import type { FieldSpec } from './types';
import { registerWidget, widgetFor, type WidgetChoice } from './widgets';

type SettingsIndex = components['schemas']['SettingsIndex'];
type Section = components['schemas']['SettingsSection'];

// B2's golden `GET /settings` (§10.10.11: the UI's fixtures match B2's golden responses). The
// first line is the status.
const GOLDEN = join(
	import.meta.dirname,
	'../../../../../../crates/lodestar/src/settings/golden/settings_index.txt'
);
const golden = (): SettingsIndex => {
	const text = readFileSync(GOLDEN, 'utf8');
	return JSON.parse(text.slice(text.indexOf('\n') + 1));
};

/**
 * Maps and lists whose widgets other slices register (S-logs, S-keys; S-settings-2b for the
 * client list and the channel lists); until then, the JSON input.
 */
const REGISTERED_ELSEWHERE = [
	'logging.events',
	'logging.kinds',
	'logging.components',
	'preferences.shortcut_keys',
	'integrations.clients',
	'notifications.route_alerts',
	'notifications.route_updates',
	'notifications.route_health',
	'insights.digest_channels'
];

/** A list of free strings; a list of objects or of enum values reads as unknown. */
const isStringList = (items: unknown) => {
	const i = items as { type?: string; enum?: unknown } | undefined;
	return i?.type === 'string' && i.enum === undefined;
};

const name = (w: WidgetChoice) => ('builtin' in w ? w.builtin : w.name);

beforeEach(() => vi.spyOn(console, 'warn').mockImplementation(() => {}));
afterEach(() => vi.restoreAllMocks());

describe('the schema reader on the golden index', () => {
	const index = golden();
	const view = indexView(index);
	const all = view.fieldsOf(index.sections.map((s) => s.section));

	it('reads every field of every section', () => {
		const expected = index.sections.flatMap((s) => s.fields.map((f) => `${s.section}.${f.name}`));
		expect(all.map((s) => s.key).sort()).toEqual(expected.sort());
	});

	it('reads every property against its schema', () => {
		for (const section of index.sections) {
			const props = (
				section.schema as unknown as { properties: Record<string, Record<string, unknown>> }
			).properties;
			for (const f of section.fields) {
				const p = props[f.name]!;
				const spec = view.spec(`${section.section}.${f.name}`)!;
				const at = spec.key;
				expect(spec.labelKey, at).toBe(f.label_key);
				expect(spec.helpKey, at).toBe(f.help_key);
				expect(spec.group, at).toBe(p['x-group']);
				expect(spec.order, at).toBe(p['x-order']);
				expect(spec.advanced, at).toBe(p['x-advanced']);
				expect(spec.preview, at).toBe(p['x-preview']);
				expect(spec.unit, at).toBe(p['x-unit']);
				expect(spec.widget, at).toBe(p['x-widget']);
				expect(spec.keywordsKey, at).toBe(p['x-keywords']);
				expect(spec.danger, at).toBe(p['x-danger']);
				expect(spec.config, at).toBe(p['x-config']);
				expect(spec.env, at).toBe(p['x-env'] ?? f.env ?? undefined);
				expect(spec.format, at).toBe(p.format);
				expect(spec.minimum, at).toBe(p.minimum);
				expect(spec.maximum, at).toBe(p.maximum);
				expect('auto' in spec, at).toBe('x-auto' in p);
				if ('x-auto' in p) expect(spec.auto, at).toEqual(p['x-auto']);
				expect(spec.readOnly, at).toBe(f.read_only || p.readOnly === true);
				expect(spec.writeOnly, at).toBe(p.writeOnly === true);
				expect(spec.restartRequired, at).toBe(f.restart_required);
				expect(spec.enum, at).toEqual(p.enum);
				const kind =
					p.enum !== undefined
						? 'enum'
						: p.type === 'object'
							? 'map'
							: p.type === 'array'
								? isStringList(p.items)
									? 'string-list'
									: 'unknown'
								: p.type;
				expect(spec.kind, at).toBe(kind);
			}
		}
	});

	it('gives every field a widget, and only the maps others register fall to JSON', () => {
		const json = all.filter((s) => name(widgetFor(s)) === 'json').map((s) => s.key);
		expect(json.sort()).toEqual([...REGISTERED_ELSEWHERE].sort());
	});

	it('picks the table’s widget for each shape', () => {
		const w = (key: string) => name(widgetFor(view.spec(key as `${string}.${string}`)!));
		expect(w('network.dht_enabled')).toBe('toggle');
		expect(w('crawler.intensity')).toBe('radio'); // x-widget: radio
		expect(w('preferences.mode')).toBe('segmented'); // 3 values
		expect(w('preferences.first_day_of_week')).toBe('segmented'); // 4 values
		expect(w('logging.console_level')).toBe('select'); // 5 values
		expect(w('crawler.timeout_secs')).toBe('number');
		expect(w('logging.data_until')).toBe('number'); // unregistered x-widget: duration
		expect(w('network.http_bind')).toBe('text');
		expect(w('preferences.theme')).toBe('text'); // unregistered x-widget: theme
		expect(w('preferences.time_zone')).toBe('text');
		expect(w('logging.libraries')).toBe('text');
		const mono = (key: string) => {
			const c = widgetFor(view.spec(key as `${string}.${string}`)!);
			return 'builtin' in c && c.mono;
		};
		expect(mono('network.http_bind')).toBe(true); // socket-addr
		expect(mono('logging.libraries')).toBe(true); // log-filter
		expect(mono('crawler.timeout_secs')).toBe(false);
	});

	it('handles every unit, widget and format the golden uses', () => {
		const set = (f: (s: FieldSpec) => string | undefined) =>
			[...new Set(all.map(f).filter((x): x is string => x !== undefined))].sort();
		expect(set((s) => s.unit)).toEqual(['connections', 'days', 'files', 'mebibytes', 'seconds']);
		expect(set((s) => s.format)).toEqual([
			'int32',
			'int64',
			'log-filter',
			'socket-addr',
			'time-range'
		]);
		expect(set((s) => s.widget)).toEqual(
			[
				'duration',
				'language',
				'log-components',
				'log-events',
				'log-kinds',
				'radio',
				'shortcuts',
				'theme',
				'time-zone'
			].sort()
		);
	});

	it('orders fields by group, then x-order', () => {
		expect(view.fieldsOf(['crawler']).map((s) => s.name)).toEqual([
			'intensity',
			'paused',
			'encryption',
			'max_sockets',
			'timeout_secs',
			'max_metadata_mib'
		]);
		// Sections in the order asked for.
		expect(view.fieldsOf(['transport', 'crawler'])[0]!.section).toBe('transport');
		expect(view.fieldsOf(['nope'])).toEqual([]);
	});

	it('groups fields into essentials and Advanced', () => {
		const groups = groupFields(view.fieldsOf(['crawler', 'transport']));
		expect(groups.map((g) => g.group)).toEqual(['intensity', 'fetching', 'transport']);
		const fetching = groups[1]!;
		expect(fetching.labelKey).toBe('settings.group.fetching');
		expect(fetching.essentials.map((s) => s.name)).toEqual(['encryption']);
		expect(fetching.advanced.map((s) => s.name)).toEqual([
			'max_sockets',
			'timeout_secs',
			'max_metadata_mib'
		]);
	});

	it('lists sections with their message keys', () => {
		expect(view.sections[0]).toEqual({
			section: 'general',
			labelKey: 'settings.general.label',
			helpKey: 'settings.general.help'
		});
	});

	it('anchors a field by its key', () => {
		expect(fieldAnchor('crawler.intensity')).toBe('crawler.intensity');
	});
});

describe('shapes the reader can’t read', () => {
	const section = (props: Record<string, unknown>): Section =>
		({
			section: 's',
			label_key: 'settings.s.label',
			help_key: 'settings.s.help',
			schema: { type: 'object', properties: props },
			fields: Object.keys(props).map((n) => ({
				name: n,
				env: null,
				label_key: `settings.s.${n}.label`,
				help_key: `settings.s.${n}.help`,
				read_only: false,
				restart_required: false
			}))
		}) as unknown as Section;

	it('falls back to unknown (and the JSON input) with a dev warning naming the field', () => {
		const specs = readSection(
			section({
				a: { type: 'array', items: { type: 'integer' } },
				b: { oneOf: [{ type: 'string' }] },
				c: { type: 'object', properties: { x: { type: 'string' } } },
				d: 'not an object',
				e: { type: 'string', enum: [1, 2] }
			})
		);
		expect(specs.map((s) => s.kind)).toEqual([
			'unknown',
			'unknown',
			'unknown',
			'unknown',
			'unknown'
		]);
		expect(specs.every((s) => name(widgetFor(s)) === 'json')).toBe(true);
		expect(console.warn).toHaveBeenCalledWith(expect.stringContaining('s.a'));
	});

	it('reads a nullable type and a string list', () => {
		const [a, b] = readSection(
			section({
				a: { type: ['integer', 'null'], minimum: 1, maximum: 9 },
				b: { type: 'array', items: { type: 'string' } }
			})
		);
		expect(a!.kind).toBe('integer');
		expect(name(widgetFor(a!))).toBe('number');
		expect(b!.kind).toBe('string-list');
		expect(name(widgetFor(b!))).toBe('chips');
	});

	it('treats an unknown danger class as the strictest', () => {
		const [a] = readSection(section({ a: { type: 'boolean', 'x-danger': 'scary' } }));
		expect(a!.danger).toBe('security');
	});

	it('reads secrets, sliders and many-valued enums', () => {
		const [a, b, c, d] = readSection(
			section({
				a: { type: 'string', writeOnly: true, 'x-widget': 'theme' },
				b: { type: 'integer', minimum: 0, maximum: 10, 'x-widget': 'slider' },
				c: { type: 'integer', 'x-widget': 'slider' },
				d: { type: 'string', enum: ['a', 'b', 'c', 'd', 'e'], 'x-widget': 'radio' }
			})
		);
		expect(name(widgetFor(a!))).toBe('secret');
		expect(name(widgetFor(b!))).toBe('slider');
		expect(name(widgetFor(c!))).toBe('number'); // a slider needs both bounds
		expect(name(widgetFor(d!))).toBe('radio');
	});
});

describe('the widget registry', () => {
	const spec = (over: Partial<FieldSpec>): FieldSpec => ({
		key: 's.f',
		section: 's',
		name: 'f',
		labelKey: '',
		helpKey: '',
		kind: 'string',
		preview: false,
		group: 'g',
		order: 0,
		advanced: false,
		writeOnly: false,
		readOnly: false,
		restartRequired: false,
		...over
	});
	const Theme = (() => {}) as never;
	const Other = (() => {}) as never;

	it('lets a registered x-widget win, and unregisters', () => {
		const off = registerWidget('theme', Theme);
		expect(widgetFor(spec({ widget: 'theme' }))).toEqual({ name: 'theme', component: Theme });
		off();
		expect(name(widgetFor(spec({ widget: 'theme' })))).toBe('text');
	});

	it('takes maps once their widget is registered', () => {
		const off = registerWidget('log-events', Theme);
		expect(name(widgetFor(spec({ kind: 'map', widget: 'log-events' })))).toBe('log-events');
		off();
	});

	it('keeps a later registration until it unregisters itself', () => {
		const a = registerWidget('w', Theme);
		const b = registerWidget('w', Other);
		a(); // stale unregister: no effect
		expect(widgetFor(spec({ widget: 'w' }))).toEqual({ name: 'w', component: Other });
		b();
	});

	it('never lets a registered widget show a secret', () => {
		const off = registerWidget('theme', Theme);
		expect(name(widgetFor(spec({ widget: 'theme', writeOnly: true })))).toBe('secret');
		off();
	});

	it('maps every shape', () => {
		const cases: [Partial<FieldSpec>, string][] = [
			[{ kind: 'boolean' }, 'toggle'],
			[{ kind: 'enum', enum: ['a', 'b'] }, 'segmented'],
			[{ kind: 'enum', enum: ['a', 'b', 'c', 'd', 'e'] }, 'select'],
			[{ kind: 'integer', minimum: 0, maximum: 9 }, 'number'],
			[{ kind: 'number' }, 'number'],
			[{ kind: 'string', format: 'url' }, 'text'],
			[{ kind: 'string-list' }, 'chips'],
			[{ kind: 'map' }, 'json'],
			[{ kind: 'unknown' }, 'json']
		];
		for (const [over, want] of cases) expect(name(widgetFor(spec(over))), want).toBe(want);
	});
});
