import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screens as mine } from '../screens/screen';
import { ALIASES, buildRegistry, collect, fieldsFor, registered, sortScreens } from './registry';
import { goldenView, screen } from './testing';

beforeEach(() => vi.spyOn(console, 'error').mockImplementation(() => {}));

describe('discovery', () => {
	it('finds screen, screens and default exports, each screen once', () => {
		const a = screen('a');
		const b = screen('b');
		const c = screen('c');
		const d = screen('d');
		const found = collect({
			'/one.ts': { screen: a, default: a },
			'/two.ts': { screens: [b, c] },
			'/three.ts': { default: [d] },
			'/four.ts': {}
		});
		expect(found.map((s) => s.id).sort()).toEqual(['a', 'b', 'c', 'd']);
	});

	it('skips what is not a screen, an unknown group and a repeated id, with a dev error', () => {
		const first = screen('x');
		const found = collect({
			'/a.ts': { screen: first },
			'/b.ts': { screen: screen('x') },
			'/c.ts': { screen: { id: 'nope' } },
			'/d.ts': { screen: screen('y', { group: 'weird' as never }) }
		});
		expect(found).toEqual([first]);
		expect(console.error).toHaveBeenCalledTimes(3);
	});

	it('registers this slice’s screens and S-keys’ file as it is', () => {
		const ids = registered.map((s) => s.id);
		expect(ids).toEqual(
			expect.arrayContaining(['crawler', 'network', 'storage', 'search', 'general'])
		);
		expect(ids).toContain('keyboard');
		expect(new Set(ids).size).toBe(ids.length);
	});

	it('keeps every screen.ts to types, messages and icons (it ships in every Settings page)', async () => {
		const files = import.meta.glob(
			['/src/lib/features/*/screen.ts', '/src/lib/features/*/*/screen.ts'],
			{ query: '?raw', import: 'default', eager: true }
		) as Record<string, string>;
		expect(Object.keys(files).length).toBeGreaterThanOrEqual(2);
		const allowed = (spec: string) =>
			spec === '$lib/paraglide/messages.js' || spec.startsWith('~icons/ph/');
		for (const [path, source] of Object.entries(files)) {
			for (const m of source.matchAll(
				/^\s*(import|export)\b(?!\s+type\b)[^;]*?\bfrom\s+['"]([^'"]+)['"]/gms
			)) {
				// A value import of a type-only module still counts; `import type` is erased.
				expect(allowed(m[2]!), `${path} imports ${m[2]}`).toBe(true);
			}
			expect(source, `${path} has a dynamic import()`).not.toMatch(
				/\bimport\s*\(\s*['"][^'"]*['"]\s*\)(?!\s*\})/
			);
		}
	});
});

describe('order', () => {
	it('follows the group order, then the id table, with unknown ids last by title', () => {
		const sorted = sortScreens([
			screen('about'),
			screen('zeta', { group: 'system', title: () => 'Zeta' }),
			screen('alpha', { group: 'system', title: () => 'Alpha' }),
			screen('general'),
			screen('network', { group: 'crawling' }),
			screen('appearance', { group: 'preferences' }),
			screen('crawler', { group: 'crawling' })
		]);
		expect(sorted.map((s) => s.id)).toEqual([
			'appearance',
			'crawler',
			'network',
			'general',
			'about',
			'alpha',
			'zeta'
		]);
	});
});

describe('claims on the golden index', () => {
	const index = goldenView();
	const registry = buildRegistry(index);
	const all = index.fieldsOf(index.sections.map((s) => s.section));

	it('gives every field exactly one home', () => {
		for (const spec of all) expect(registry.screenOf(spec.key), spec.key).toBeDefined();
		const homed = registry.screens.flatMap((r) => r.keys);
		expect(homed.sort()).toEqual(all.map((s) => s.key).sort());
		expect(registry.doubleClaims.size).toBe(0);
	});

	it('puts Crawler on crawler + transport and Network, Storage, Search, General on theirs', () => {
		const keys = (id: string) => registry.fieldsFor(id).map((s) => s.key);
		expect(new Set(keys('crawler').map((k) => k.split('.')[0]))).toEqual(
			new Set(['crawler', 'transport'])
		);
		expect(keys('network')).toHaveLength(4);
		expect(keys('storage')).toEqual(['storage.mmap_mib', 'storage.archive_raw']);
		expect(keys('search')).toEqual(['search.enabled', 'search.heap_mib']);
		expect(keys('general')).toEqual(['general.data_dir']);
	});

	it('gives Keyboard its two preference fields through the fallback claim', () => {
		expect(registry.fieldsFor('keyboard').map((s) => s.key)).toEqual([
			'preferences.shortcuts',
			'preferences.shortcut_keys'
		]);
		expect(registry.screenOf('preferences.shortcuts')?.screen.id).toBe('keyboard');
	});

	it('makes an interim screen per section nobody claims, in the right group and mode', () => {
		const interim = registry.screens.filter((r) => r.interim);
		expect(interim.map((r) => r.screen.id).sort()).toEqual(['content', 'logging', 'preferences']);
		const by = Object.fromEntries(interim.map((r) => [r.screen.id, r]));
		expect(by.preferences).toMatchObject({ mode: 'instant' });
		expect(by.preferences!.screen.group).toBe('preferences');
		expect(by.content!.screen.group).toBe('preferences');
		expect(by.logging).toMatchObject({ mode: 'staged' });
		expect(by.logging!.screen.group).toBe('system');
		// The preferences screen leaves out the keyboard fields.
		expect(by.preferences!.keys).not.toContain('preferences.shortcuts');
		expect(by.preferences!.keys).toContain('preferences.theme');
	});

	it('stages every claimed non-preferences screen and orders the navigation', () => {
		expect(registry.get('crawler')?.mode).toBe('staged');
		const ids = registry.screens.map((r) => r.screen.id);
		expect(ids.indexOf('crawler')).toBeLessThan(ids.indexOf('network'));
		expect(ids.indexOf('network')).toBeLessThan(ids.indexOf('storage'));
		expect(ids.indexOf('general')).toBeLessThan(ids.indexOf('logging'));
	});

	it('makes an interim screen disappear once a screen claims its fields', () => {
		const claimed = buildRegistry(index, [
			...registered,
			screen('appearance', {
				group: 'preferences',
				sections: ['preferences'],
				groups: ['appearance']
			})
		]);
		expect(claimed.screenOf('preferences.theme')?.screen.id).toBe('appearance');
		expect(claimed.get('preferences')?.keys).toEqual(
			expect.not.arrayContaining(['preferences.theme'])
		);
	});

	it('fails on two screens claiming one field', () => {
		const twice = buildRegistry(index, [
			...registered,
			screen('dup', { group: 'crawling', sections: ['network'] })
		]);
		expect(twice.doubleClaims.get('network.http_bind')).toEqual(['network', 'dup']);
		expect(console.error).toHaveBeenCalled();
	});

	it('resolves fieldsFor(screen, index) like the registry does', () => {
		const network = mine.find((s) => s.id === 'network')!;
		expect(fieldsFor(network, index).map((s) => s.key)).toEqual(
			registry.fieldsFor('network').map((s) => s.key)
		);
	});

	it('knows only the registered screens before the schema arrives', () => {
		const early = buildRegistry(undefined);
		expect(early.screens.length).toBe(registered.length);
		expect(early.fieldsFor('crawler')).toEqual([]);
	});
});

describe('aliases', () => {
	it('sends backup and import to General’s backup section', () => {
		expect(ALIASES).toEqual({
			backup: '/settings/general#backup',
			import: '/settings/general#backup'
		});
	});
});
