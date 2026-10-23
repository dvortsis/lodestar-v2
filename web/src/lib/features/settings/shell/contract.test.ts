// The server's contract with the UI (§10.10.11), read from B2's golden `GET /settings` on disk,
// so a server change fails here: every section, field, enum option, keyword list and group has
// its text in English and Chinese, every field gets a real widget and exactly one home screen.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { messageId } from '../form/messages';
import { widgetFor } from '../form/widgets';
import { buildRegistry } from './registry';
import { goldenIndex, goldenView } from './testing';

const read = (loc: string): Record<string, string> =>
	JSON.parse(readFileSync(join(import.meta.dirname, `../messages/${loc}.json`), 'utf8'));
const en = read('en');
const zh = read('zh');
const index = goldenIndex();
const view = goldenView();

/** Maps whose widgets other slices register (S-logs, S-keys); until then, the JSON input. */
const REGISTERED_ELSEWHERE = [
	'logging.events',
	'logging.kinds',
	'logging.components',
	'preferences.shortcut_keys'
];

/** Every server key the golden names, with where it came from. */
function serverKeys(): { key: string; from: string }[] {
	const out: { key: string; from: string }[] = [];
	for (const s of index.sections) {
		out.push({ key: s.label_key, from: s.section }, { key: s.help_key, from: s.section });
		const props = (s.schema as unknown as { properties: Record<string, Record<string, unknown>> })
			.properties;
		for (const f of s.fields) {
			const from = `${s.section}.${f.name}`;
			out.push({ key: f.label_key, from }, { key: f.help_key, from });
			const p = props[f.name]!;
			if (typeof p['x-keywords'] === 'string') out.push({ key: p['x-keywords'], from });
			if (Array.isArray(p.enum)) {
				for (const v of p.enum)
					out.push({ key: `settings.${s.section}.${f.name}.option.${v}`, from });
			}
			if (typeof p['x-group'] === 'string') {
				out.push({ key: `settings.group.${p['x-group']}`, from });
			}
		}
	}
	return out;
}

beforeEach(() => vi.spyOn(console, 'warn').mockImplementation(() => {}));

describe('messages for the golden index', () => {
	const keys = serverKeys();

	it('reads a useful number of keys', () => {
		expect(keys.length).toBeGreaterThan(150);
	});

	it.each(['en', 'zh'] as const)('has %s text for every key', (loc) => {
		const messages = loc === 'en' ? en : zh;
		const missing = keys.filter(({ key }) => !messages[messageId(key)]?.trim());
		expect(missing.map((k) => `${k.key} (${k.from})`)).toEqual([]);
	});

	it('writes help in sentences, not the schema’s developer description', () => {
		for (const { key } of keys.filter((k) => k.key.endsWith('.help'))) {
			expect(en[messageId(key)], key).toMatch(/[.!?]$/);
			expect(en[messageId(key)], key).not.toMatch(/`|§/);
		}
	});

	it('has no server message that nothing in the golden names', () => {
		const named = new Set(keys.map(({ key }) => messageId(key)));
		const server = Object.keys(en).filter(
			(k) => /^settings_/.test(k) && !/^settings_(form|ui)_/.test(k)
		);
		expect(server.filter((k) => !named.has(k))).toEqual([]);
	});

	it('keeps zh placeholders equal to en’s', () => {
		for (const k of Object.keys(en).filter((k) => k.startsWith('settings_') && zh[k])) {
			const names = (t: string) => [...t.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
			expect(names(zh[k]!), k).toEqual(names(en[k]!));
		}
	});
});

describe('widgets and homes', () => {
	const all = view.fieldsOf(index.sections.map((s) => s.section));

	it('gives every field a widget that is not the JSON fallback, except the maps others register', () => {
		const json = all
			.filter((s) => {
				const w = widgetFor(s);
				return 'builtin' in w && w.builtin === 'json';
			})
			.map((s) => s.key);
		expect(json.sort()).toEqual([...REGISTERED_ELSEWHERE].sort());
	});

	it('gives every field exactly one home screen', () => {
		const registry = buildRegistry(view);
		expect(registry.doubleClaims.size).toBe(0);
		const homes = new Map<string, number>();
		for (const r of registry.screens) for (const k of r.keys) homes.set(k, (homes.get(k) ?? 0) + 1);
		for (const spec of all) expect(homes.get(spec.key), spec.key).toBe(1);
	});
});
