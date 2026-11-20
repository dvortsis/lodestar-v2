// X-session-draft-keep: no secret ever reaches a draft, by the schema's flags; the kept marker
// is honoured once, on the screen it came from; blocked or full storage only loses the
// convenience. Sign out and a re-claimed instance dropping every draft: `auth/session.test.ts`.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { components } from '$lib/api/schema';
import { draftOf, planRestore, readDraft, writeDraft, type Draft } from './draft';
import { KEPT_KEY } from './draft-scope';
import { KEPT_MAX_AGE_MS, takeKept } from './saving';
import { dropKept, markKept } from './signed-out';
import { neverDrafted } from './draft-store';
import { readSection } from './schema';
import type { FieldSpec } from './types';
import { fromValues, stage, type SectionState } from './values';

type SettingsIndex = components['schemas']['SettingsIndex'];

// B2's golden `GET /settings`: the server's real sections and schemas.
const GOLDEN = join(
	import.meta.dirname,
	'../../../../../../crates/lodestar/src/settings/golden/settings_index.txt'
);
const golden = (): SettingsIndex => {
	const text = readFileSync(GOLDEN, 'utf8');
	return JSON.parse(text.slice(text.indexOf('\n') + 1));
};

/** Every field whose schema is, or holds, a `writeOnly` subschema: found by walking the JSON. */
function secretKeys(index: SettingsIndex): string[] {
	const holds = (p: unknown): boolean => {
		if (typeof p !== 'object' || p === null) return false;
		const o = p as Record<string, unknown>;
		if (o.writeOnly === true) return true;
		return Object.values(o).some((v) =>
			Array.isArray(v) ? v.some(holds) : typeof v === 'object' && holds(v)
		);
	};
	const out: string[] = [];
	for (const s of index.sections) {
		const props = (s.schema as { properties?: Record<string, unknown> }).properties ?? {};
		for (const f of s.fields) if (holds(props[f.name])) out.push(`${s.section}.${f.name}`);
	}
	return out.sort();
}

const SECRET = 'hunter2-never-stored';

/** A value for `spec` that carries `SECRET` wherever a credential could go. */
function secretValue(spec: FieldSpec): unknown {
	if (spec.kind === 'unknown' || spec.kind === 'map')
		return [{ name: 'qBittorrent', url: 'http://qb.lan:8080', password: SECRET }];
	return SECRET;
}

beforeEach(() => sessionStorage.clear());
afterEach(() => vi.restoreAllMocks());

describe('secrets never reach a draft (every secret in the golden schema)', () => {
	const index = golden();
	const keys = secretKeys(index);

	it('finds the secrets the server declares', () => {
		// The server's `.secret()` and `.secret_keys()` fields; a new one is covered automatically.
		expect(keys).toEqual(
			expect.arrayContaining([
				'integrations.clients',
				'integrations.torznab_api_key',
				'notifications.discord_url',
				'notifications.email_password',
				'notifications.ntfy_token',
				'notifications.ntfy_url',
				'notifications.webhook_url'
			])
		);
	});

	const specs = index.sections.flatMap((s) => readSection(s));
	const byKey = new Map(specs.map((s) => [s.key as string, s]));
	const specOf = (section: string, name: string) => byKey.get(`${section}.${name}`);

	it.each(keys)('%s is never drafted', (key) => {
		const spec = byKey.get(key)!;
		expect(spec).toBeDefined();
		expect(neverDrafted(spec)).toBe(true);
		const section = spec.section;
		const sections: Record<string, SectionState> = {
			[section]: fromValues({
				section,
				revision: 1,
				restart_required: [],
				fields: {
					[spec.name]: { value: null, source: 'default', env: null, read_only: false }
				}
			} as never)
		};
		sections[section] = stage(sections[section]!, spec.name, secretValue(spec), spec);
		const d = draftOf(sections, specOf, 1);
		expect(JSON.stringify(d ?? {})).not.toContain(SECRET);
		writeDraft('screen', d);
		for (let i = 0; i < sessionStorage.length; i++) {
			expect(sessionStorage.getItem(sessionStorage.key(i)!)).not.toContain(SECRET);
		}
	});

	it.each(keys)('%s in a crafted stored draft is dropped on restore', (key) => {
		const spec = byKey.get(key)!;
		const current: Record<string, SectionState> = {
			[spec.section]: fromValues({
				section: spec.section,
				revision: 1,
				restart_required: [],
				fields: {
					[spec.name]: { value: null, source: 'default', env: null, read_only: false }
				}
			} as never)
		};
		const draft: Draft = {
			v: 1,
			at: 1,
			sections: {
				[spec.section]: {
					revision: 1,
					base: { [spec.name]: null },
					mine: { [spec.name]: secretValue(spec) },
					reset: []
				}
			}
		};
		const plan = planRestore(draft, current, specOf);
		expect(plan.fields).toBe(0);
		expect(plan.dropped).toBe(1);
		expect(JSON.stringify(plan.sections)).not.toContain(SECRET);
	});

	it('marks a list whose items carry a credential, by the flag, not the name', () => {
		const [spec] = readSection({
			section: 'x',
			fields: [{ name: 'accounts', label_key: 'l', help_key: 'h', read_only: false }],
			schema: {
				properties: {
					accounts: {
						type: 'array',
						items: { type: 'object', properties: { pin: { type: 'string', writeOnly: true } } }
					}
				}
			}
		} as never);
		expect(spec!.writeOnly).toBe(false);
		expect(spec!.holdsSecret).toBe(true);
		expect(neverDrafted(spec!)).toBe(true);
	});
});

describe('the kept marker', () => {
	it('is honoured once, on the screen it came from', () => {
		expect(markKept('crawler', '/settings/crawler', 1000)).toBe(true);
		expect(takeKept('crawler', '/settings/crawler', 2000)).toBe(true);
		expect(takeKept('crawler', '/settings/crawler', 2000)).toBe(false);
	});

	it('is left alone by another screen, and dropped by its own on another path', () => {
		markKept('crawler', '/settings/crawler', 1000);
		expect(takeKept('network', '/settings/network', 2000)).toBe(false);
		expect(sessionStorage.getItem(KEPT_KEY)).not.toBeNull();
		expect(takeKept('crawler', '/settings/general', 2000)).toBe(false);
		expect(sessionStorage.getItem(KEPT_KEY)).toBeNull();
	});

	it('expires, and a bad or oversized one reads as none and is removed', () => {
		markKept('crawler', '/settings/crawler', 0);
		expect(takeKept('crawler', '/settings/crawler', KEPT_MAX_AGE_MS + 1)).toBe(false);
		for (const bad of ['{', '"x"', '{"id":1}', 'x'.repeat(5000)]) {
			sessionStorage.setItem(KEPT_KEY, bad);
			expect(takeKept('crawler', '/settings/crawler', 1)).toBe(false);
			expect(sessionStorage.getItem(KEPT_KEY)).toBeNull();
		}
	});

	it('is dropped when the user stays', () => {
		markKept('crawler', '/settings/crawler', 1);
		dropKept();
		expect(sessionStorage.getItem(KEPT_KEY)).toBeNull();
	});
});

describe('blocked storage only loses the convenience', () => {
	it('never throws', () => {
		const boom = () => {
			throw new DOMException('blocked', 'SecurityError');
		};
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(boom);
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(boom);
		vi.spyOn(Storage.prototype, 'removeItem').mockImplementation(boom);
		vi.spyOn(Storage.prototype, 'key').mockImplementation(boom);
		expect(markKept('crawler', '/settings/crawler', 1)).toBe(false);
		expect(takeKept('crawler', '/settings/crawler', 1)).toBe(false);
		expect(() => dropKept()).not.toThrow();
		expect(readDraft('crawler')).toBeNull();
	});
});
