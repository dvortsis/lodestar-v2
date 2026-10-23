// Playwright only: an in-memory `/settings` API answered through the OpenAPI mock, shared by
// every page of a test (a second browser context attaches the same backend to its own mock), so
// two tabs really race each other's revisions. Schema shapes follow B2b's golden index.
import type { components } from '../../../../api/schema';

type Index = components['schemas']['SettingsIndex'];
type Section = components['schemas']['SettingsSection'];
type Values = components['schemas']['SectionValues'];
type Value = components['schemas']['SettingValue'];
type Patch = Record<string, Record<string, unknown>>;
type Reply = { status: number; body?: unknown; headers?: Record<string, string> };

/** The part of the OpenAPI mock this needs (structural, so `src/` never imports `e2e/`). */
export interface MockLike {
	on(operationId: never, override: never): unknown;
}
interface RequestLike {
	url(): string;
	postDataJSON(): unknown;
}

const field = (section: string, name: string, over: Partial<Section['fields'][number]> = {}) => ({
	name,
	env: null,
	label_key: `settings.${section}.${name}.label`,
	help_key: `settings.${section}.${name}.help`,
	read_only: false,
	restart_required: false,
	...over
});

/** A screen's worth of fields: every widget shape, an env lock, a danger class and a secret. */
export const FIXTURE_INDEX: Index = {
	sections: [
		{
			section: 'crawler',
			label_key: 'settings.crawler.label',
			help_key: 'settings.crawler.help',
			fields: [
				field('crawler', 'intensity'),
				field('crawler', 'paused'),
				field('crawler', 'max_sockets', { restart_required: true }),
				field('crawler', 'timeout_secs', { restart_required: true })
			],
			schema: {
				type: 'object',
				properties: {
					intensity: {
						type: 'string',
						enum: ['low', 'balanced', 'aggressive'],
						'x-group': 'intensity',
						'x-order': 0,
						'x-widget': 'radio',
						'x-preview': true,
						'x-config': 'dht.intensity',
						'x-env': 'LODESTAR_INTENSITY'
					},
					paused: { type: 'boolean', 'x-group': 'intensity', 'x-order': 1 },
					max_sockets: {
						type: 'integer',
						minimum: 0,
						maximum: 4096,
						'x-auto': 0,
						'x-preview': true,
						'x-unit': 'connections',
						'x-group': 'fetching',
						'x-order': 2
					},
					timeout_secs: {
						type: 'integer',
						minimum: 1,
						maximum: 120,
						'x-unit': 'seconds',
						'x-group': 'fetching',
						'x-order': 3,
						'x-advanced': true
					}
				}
			} as never
		},
		{
			section: 'network',
			label_key: 'settings.network.label',
			help_key: 'settings.network.help',
			fields: [
				field('network', 'http_bind', { restart_required: true, env: 'LODESTAR_HTTP_BIND' }),
				field('network', 'dht_port', { restart_required: true, env: 'LODESTAR_DHT_PORT' })
			],
			schema: {
				type: 'object',
				properties: {
					http_bind: {
						type: 'string',
						format: 'socket-addr',
						'x-danger': 'disruptive',
						'x-preview': true,
						'x-group': 'web_ui',
						'x-order': 0
					},
					dht_port: {
						type: 'integer',
						minimum: 1,
						maximum: 65535,
						'x-group': 'dht',
						'x-order': 1,
						'x-env': 'LODESTAR_DHT_PORT'
					}
				}
			} as never
		},
		{
			section: 'secrets',
			label_key: 'settings.secrets.label',
			help_key: 'settings.secrets.help',
			fields: [field('secrets', 'token')],
			schema: {
				type: 'object',
				properties: { token: { type: 'string', writeOnly: true, 'x-group': 'api', 'x-order': 0 } }
			} as never
		}
	]
};

const v = (
	value: unknown,
	source: Value['source'] = 'default',
	over: Partial<Value> = {}
): Value => ({
	env: null,
	read_only: false,
	restart_required: false,
	source,
	value,
	...over
});

export const fixtureValues = (): Values[] => [
	{
		section: 'crawler',
		revision: 3,
		restart_required: [],
		fields: {
			intensity: v('balanced'),
			paused: v(false),
			max_sockets: v(0),
			timeout_secs: v(12, 'db', { restart_required: true })
		}
	},
	{
		section: 'network',
		revision: 2,
		restart_required: [],
		fields: {
			http_bind: v('0.0.0.0:8090', 'file', { restart_required: true }),
			dht_port: v(6881, 'env', {
				env: 'LODESTAR_DHT_PORT',
				read_only: true,
				restart_required: true
			})
		}
	},
	{ section: 'secrets', revision: 1, restart_required: [], fields: { token: v(null, 'db') } }
];

/** What a reset (`null`) goes back to. */
const DEFAULTS: Record<string, unknown> = {
	'crawler.intensity': 'balanced',
	'crawler.paused': false,
	'crawler.max_sockets': 0,
	'crawler.timeout_secs': 10,
	'network.http_bind': '0.0.0.0:8090',
	'secrets.token': null
};

const err = (status: number, code: string, extra: Record<string, unknown> = {}, top = {}) => ({
	status,
	body: { error: { code, message: `Refused: ${code}`, ...extra }, ...top }
});

const RESERVED = new Set(['if_revision', 'confirm_adult']);

export class SettingsBackend {
	readonly sections = new Map<string, Values>();
	/** Canned replies for the next batch saves (`PATCH /settings`), in order. */
	readonly nextPatch: (Reply | ((body: Patch) => Reply))[] = [];
	readonly patches: Patch[] = [];
	readonly previews: Patch[] = [];

	constructor(
		readonly index: Index = FIXTURE_INDEX,
		values: Values[] = fixtureValues()
	) {
		for (const s of values) this.sections.set(s.section, structuredClone(s));
	}

	/** Answers the four settings operations on this mock. */
	attach(mock: MockLike): this {
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('listSettings', () => ({ status: 200, body: this.index }));
		on('getSettings', (r: RequestLike) => {
			const name = decodeURIComponent(new URL(r.url()).pathname.split('/').pop() ?? '');
			const s = this.sections.get(name);
			return s ? { status: 200, body: structuredClone(s) } : err(404, 'unknown_section');
		});
		on('patchSettingsBatch', (r: RequestLike) => {
			const body = r.postDataJSON() as Patch;
			this.patches.push(body);
			const canned = this.nextPatch.shift();
			if (canned) return typeof canned === 'function' ? canned(body) : canned;
			return this.save(body);
		});
		on('previewSettings', (r: RequestLike) => {
			const body = r.postDataJSON() as Patch;
			this.previews.push(body);
			return this.preview(body);
		});
		return this;
	}

	/** Another tab saves: values change and the revision moves. */
	change(section: string, fields: Record<string, Partial<Value>>): number {
		const s = this.sections.get(section)!;
		for (const [k, f] of Object.entries(fields)) s.fields[k] = { ...s.fields[k]!, ...f };
		return ++s.revision;
	}

	private check(body: Patch): Reply | null {
		const stale: Record<string, Values> = {};
		for (const [name, patch] of Object.entries(body)) {
			const s = this.sections.get(name);
			if (!s) return err(404, 'unknown_section', { section: name });
			if (typeof patch.if_revision === 'number' && patch.if_revision < s.revision) {
				stale[name] = structuredClone(s);
			}
			for (const [k, value] of Object.entries(patch)) {
				if (RESERVED.has(k)) continue;
				const f = s.fields[k];
				if (!f) return err(400, 'unknown_field', { section: name, field: k });
				if (f.source === 'env') {
					return err(409, 'set_by_environment', {
						section: name,
						field: k,
						message: `The environment variable ${f.env} sets this.`
					});
				}
				if (
					name === 'crawler' &&
					k === 'timeout_secs' &&
					typeof value === 'number' &&
					value > 120
				) {
					return err(422, 'invalid_setting', {
						section: name,
						field: k,
						message: 'Must be from 1 to 120.'
					});
				}
			}
		}
		return Object.keys(stale).length ? err(409, 'stale_revision', {}, { current: stale }) : null;
	}

	private save(body: Patch): Reply {
		const refused = this.check(body);
		if (refused) return refused;
		const out: Record<string, Values> = {};
		for (const [name, patch] of Object.entries(body)) {
			const s = this.sections.get(name)!;
			let changed = false;
			for (const [k, value] of Object.entries(patch)) {
				if (RESERVED.has(k)) continue;
				const f = s.fields[k]!;
				const next: Value =
					value === null
						? { ...f, source: 'default', value: DEFAULTS[`${name}.${k}`] ?? f.value }
						: { ...f, source: 'db', value };
				if (JSON.stringify(next) !== JSON.stringify(f)) changed = true;
				// A secret's value is never sent back (B2b).
				s.fields[k] = name === 'secrets' ? { ...next, value: null } : next;
			}
			if (changed) s.revision++;
			s.restart_required = Object.keys(s.fields).filter(
				(k) => s.fields[k]!.restart_required && s.fields[k]!.source === 'db'
			);
			out[name] = structuredClone(s);
		}
		return { status: 200, body: { sections: out } };
	}

	private preview(body: Patch): Reply {
		const refused = this.check(body);
		if (refused) return refused;
		const sections: Record<string, unknown> = {};
		for (const [name, patch] of Object.entries(body)) {
			const fields: Record<string, unknown> = {};
			for (const [k, value] of Object.entries(patch)) {
				if (RESERVED.has(k)) continue;
				const estimates =
					k === 'max_sockets' && value === 0
						? [{ name: 'sockets', value: 400 }]
						: k === 'http_bind' && typeof value === 'string'
							? [{ name: 'url', value: `http://192.168.1.5:${value.split(':').pop()}/` }]
							: [];
				fields[k] = { estimates, restart_required: k !== 'intensity', value, warnings: [] };
			}
			sections[name] = { revision: this.sections.get(name)!.revision, fields };
		}
		return { status: 200, body: { sections } };
	}
}
