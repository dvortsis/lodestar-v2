import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { components } from '$lib/api/schema';
import { setLocale } from '$lib/paraglide/runtime.js';
import { indexView } from '../form/schema';
import type { FieldSpec } from '../form/types';
import {
	clearFindIndex,
	describeValue,
	FIELD_SCREENS,
	highlight,
	loadFindIndex,
	matches,
	normalize,
	search,
	type FindEntry
} from './find';
import { INERT, screens } from './screens';

type SettingsIndex = components['schemas']['SettingsIndex'];

// B2's golden `GET /settings`, the same one the schema reader is tested on.
const GOLDEN = join(
	import.meta.dirname,
	'../../../../../../crates/lodestar/src/settings/golden/settings_index.txt'
);
const golden = (): SettingsIndex => {
	const text = readFileSync(GOLDEN, 'utf8');
	return JSON.parse(text.slice(text.indexOf('\n') + 1));
};
const view = indexView(golden());
vi.mock('../form/schema', async (original) => ({
	...(await original<typeof import('../form/schema')>()),
	loadSettingsIndex: () => Promise.resolve(view)
}));

beforeEach(() => {
	clearFindIndex();
	vi.spyOn(console, 'warn').mockImplementation(() => {});
});
afterEach(() => {
	setLocale('en', { reload: false });
	vi.restoreAllMocks();
});

const hrefs = (entries: FindEntry[]) => entries.map((e) => e.href);

describe('matching', () => {
	it('is case and accent insensitive, by word prefix', () => {
		const hay = normalize('Crawl intensity: how hard it works');
		expect(matches('CRAWL', hay)).toBe(true);
		expect(matches('inten', hay)).toBe(true);
		expect(matches('crawl hard', hay)).toBe(true);
		expect(matches('tensity', hay)).toBe(false);
		expect(matches('crawl zebra', hay)).toBe(false);
		expect(matches('', hay)).toBe(false);
		expect(matches('réseau', normalize('Reseau'))).toBe(true);
	});
	it('matches Chinese as a substring, since it has no spaces to prefix by', () => {
		expect(matches('强度', normalize('爬虫强度设置'))).toBe(true);
		expect(matches('虫强', normalize('爬虫强度设置'))).toBe(true);
		expect(matches('端口', normalize('爬虫强度设置'))).toBe(false);
	});
});

describe('the index on the golden schema', () => {
	it('reaches every screen by its title', async () => {
		const entries = await loadFindIndex();
		for (const screen of screens) {
			const found = search(entries, screen.title(), 200);
			expect(hrefs(found), `${screen.id} by its title`).toContain(`/settings/${screen.id}`);
		}
	});

	it('reaches every field by its label, except the ones nothing reads yet', async () => {
		const entries = await loadFindIndex();
		const byKey = new Map(
			entries.filter((e) => e.spec).map((e) => [e.spec!.key as string, e] as const)
		);
		let reached = 0;
		for (const spec of view.fieldsOf(golden().sections.map((s) => s.section))) {
			const entry = byKey.get(spec.key);
			if (INERT.has(spec.key)) {
				expect(entry, `${spec.key} is inert`).toBeUndefined();
				continue;
			}
			// A section no screen shows has no entry; the Overview lists those separately.
			if (!entry) continue;
			reached++;
			expect(hrefs(search(entries, entry.label, 500)), spec.key).toContain(entry.href);
			expect(entry.href).toBe(`/settings/${entry.screen}#${spec.key}`);
		}
		expect(reached).toBeGreaterThan(40);
	});

	it('indexes fields only for screens whose form opens #field', async () => {
		const entries = await loadFindIndex();
		const fieldScreens = new Set(entries.filter((e) => e.spec).map((e) => e.screen));
		expect(fieldScreens.size).toBeGreaterThan(3);
		for (const id of fieldScreens) {
			const screen = screens.find((s) => s.id === id)!;
			expect(
				screen.sections?.length || FIELD_SCREENS.has(id),
				`${id}'s form doesn't open #field`
			).toBeTruthy();
		}
		for (const id of ['appearance', 'updates', 'logging', 'language'])
			expect(fieldScreens.has(id), id).toBe(false);
	});

	it('finds a field by its config.toml key, its environment name and its help', async () => {
		const entries = await loadFindIndex();
		expect(hrefs(search(entries, 'dht.bind'))).toContain('/settings/network#network.dht_port');
		expect(hrefs(search(entries, 'LODESTAR_DHT_PORT'))).toContain(
			'/settings/network#network.dht_port'
		);
		expect(hrefs(search(entries, 'port'))).toContain('/settings/network#network.dht_port');
	});

	it('shows where a field lives, and Advanced when it is behind the disclosure', async () => {
		const entries = await loadFindIndex();
		const advanced = entries.find((e) => e.spec?.advanced);
		expect(advanced?.trail.split(' › ').at(-1)).toBe('Advanced');
		const plain = entries.find((e) => e.spec && !e.spec.advanced);
		expect(plain?.trail).toContain(' › ');
	});

	it('puts a label match ahead of a help match', async () => {
		const entries = await loadFindIndex();
		const found = search(entries, 'port');
		const firstHelpOnly = found.findIndex((e) => !matches('port', e.head));
		const lastLabel = found.map((e) => matches('port', e.head)).lastIndexOf(true);
		if (firstHelpOnly >= 0) expect(lastLabel).toBeLessThan(firstHelpOnly);
	});

	it('reaches a field in Chinese too', async () => {
		setLocale('zh', { reload: false });
		const entries = await loadFindIndex();
		const label = entries.find((e) => e.spec?.key === 'network.dht_port')!.label;
		expect(hrefs(search(entries, label))).toContain('/settings/network#network.dht_port');
	});

	it('holds no value: nothing but text from the schema and the messages', async () => {
		const entries = await loadFindIndex();
		for (const e of entries)
			expect(Object.keys(e).sort()).toEqual(
				[
					'screen',
					'screenTitle',
					'label',
					'trail',
					'href',
					'hay',
					'head',
					...(e.spec ? ['spec'] : [])
				].sort()
			);
	});
});

describe('what a result says of its value', () => {
	const secretSpecs = (): FieldSpec[] =>
		view
			.fieldsOf(golden().sections.map((s) => s.section))
			.filter((spec) => spec.writeOnly || spec.holdsSecret);

	it('is only Set or Not set for every field the schema marks secret or write-only', () => {
		const specs = secretSpecs();
		expect(specs.length).toBeGreaterThan(3);
		for (const spec of specs) {
			const secret = 'hunter2-the-actual-secret';
			const set = describeValue(spec, secret, 'db');
			const unset = describeValue(spec, secret, 'default');
			expect(set.value, spec.key).toBe('Set');
			expect(unset.value, spec.key).toBe('Not set');
			expect(JSON.stringify([set, unset])).not.toContain(secret);
		}
	});

	it('shows a plain value and its source', () => {
		const spec = view.spec('network.dht_port')!;
		expect(describeValue(spec, 6881, 'default')).toEqual({ value: '6,881', source: 'Default' });
		expect(describeValue(spec, 7000, 'db').source).toBe('Saved in the app');
	});
});

describe('the highlight', () => {
	it('outlines the field once it is on the page, and clears it', async () => {
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		vi.stubGlobal('matchMedia', () => ({ matches: true }));
		const el = document.createElement('div');
		el.id = 'network.dht_port';
		el.innerHTML = '<div data-control></div>';
		document.body.append(el);
		const stop = highlight('network.dht_port' as FieldSpec['key']);
		expect(el.style.outline).toContain('2px solid');
		vi.advanceTimersByTime(2100);
		expect(el.style.outline).toBe('');
		stop();
		el.remove();
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});
});
