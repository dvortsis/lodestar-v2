import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
	discover,
	injectBootLocales,
	merge,
	mergeMessages,
	placeholders,
	topLevelKeys
} from './messages.ts';

let root: string;
beforeEach(() => {
	root = mkdtempSync(join(tmpdir(), 'messages-'));
});
afterEach(() => rmSync(root, { recursive: true, force: true }));

function put(dir: string, locale: string, data: unknown) {
	const path = join(root, dir, 'messages');
	mkdirSync(path, { recursive: true });
	writeFileSync(join(path, `${locale}.json`), JSON.stringify(data));
}
const shared = 'src/lib/i18n';
const feature = (name: string) => `src/lib/features/${name}`;
const run = () => merge(discover(root));

describe('placeholders', () => {
	it('collects names', () => {
		expect(placeholders('{a} of {b_2}')).toEqual({ names: ['a', 'b_2'] });
		expect(placeholders('plain')).toEqual({ names: [] });
	});
	it.each(['{', 'a}', '{}', '{1x}', '{a b}', '{{a}}', '{n, plural, one {x} other {y}}'])(
		'rejects %j',
		(text) => expect(placeholders(text)).toHaveProperty('error')
	);
});

describe('merge', () => {
	it('merges features and shared strings', () => {
		put(shared, 'en', { common_ok: 'OK' });
		put(feature('search'), 'en', { search_results_count: '{count} results' });
		put(feature('search'), 'zh', { search_results_count: '{count} 个结果' });
		const r = run();
		expect(r.errors).toEqual([]);
		expect(r.messages.en).toEqual({ common_ok: 'OK', search_results_count: '{count} results' });
		expect(r.messages.zh).toEqual({ search_results_count: '{count} 个结果' });
	});

	it("merges a feature part's own folder, with the part's prefix", () => {
		put(feature('search'), 'en', { search_page_title: 'Search' });
		put(feature('search/results'), 'en', { search_results_none: 'No torrents match' });
		put(feature('search/results'), 'zh', { search_results_none: '没有匹配的种子' });
		const r = run();
		expect(r.errors).toEqual([]);
		expect(r.messages.en).toEqual({
			search_page_title: 'Search',
			search_results_none: 'No torrents match'
		});
		expect(discover(root).map((s) => s.name)).toContain('search/results');
	});

	it("fails on a part's key without the part's prefix", () => {
		put(feature('search/facets'), 'en', { search_results_x: 'wrong folder' });
		const r = run();
		expect(r.errors).toHaveLength(1);
		expect(r.errors[0]).toMatch(/must start with "search_facets_"/);
	});

	it('fails on a key a feature and its part both define', () => {
		put(feature('search'), 'en', { search_results_none: 'one' });
		put(feature('search/results'), 'en', { search_results_none: 'two' });
		expect(run().errors.filter((e) => e.includes('duplicate key'))).toHaveLength(1);
	});

	it('ignores a part folder with no messages', () => {
		mkdirSync(join(root, feature('search/query')), { recursive: true });
		put(feature('search'), 'en', { search_page_title: 'Search' });
		expect(discover(root).map((s) => s.name)).not.toContain('search/query');
	});

	it('fails on a key defined by two features', () => {
		// "a" and "a-x" both own the "a_x_" namespace prefix space: a_x_y is valid in each.
		put(feature('a'), 'en', { a_x_y: 'one' });
		put(feature('a-x'), 'en', { a_x_y: 'two' });
		const r = run();
		expect(r.errors).toHaveLength(1);
		expect(r.errors[0]).toMatch(/duplicate key.*already defined by a\b/);
		put(feature('a'), 'zh', { a_x_y: '一' });
		put(feature('a-x'), 'zh', { a_x_y: '二' });
		expect(run().errors.filter((e) => e.includes('duplicate key'))).toHaveLength(2);
	});

	it('fails on a zh key missing from en', () => {
		put(feature('search'), 'en', { search_a: 'A' });
		put(feature('search'), 'zh', { search_a: '甲', search_b: '乙' });
		const r = run();
		expect(r.errors).toEqual(['zh key "search_b" (search) has no en message']);
	});

	it('warns, listing keys, when zh lacks an en key', () => {
		put(feature('search'), 'en', { search_a: 'A', search_b: 'B' });
		put(feature('search'), 'zh', { search_a: '甲' });
		const r = run();
		expect(r.errors).toEqual([]);
		expect(r.warnings).toHaveLength(1);
		expect(r.warnings[0]).toContain('search_b');
		expect(r.warnings[0]).not.toContain('search_a,');
	});

	it('fails on invalid placeholders and mismatched zh placeholders', () => {
		put(feature('search'), 'en', { search_a: '{count results', search_b: '{n} items' });
		put(feature('search'), 'zh', { search_b: '{count} 项' });
		const r = run();
		expect(r.errors.some((e) => e.includes('search_a') && e.includes('unclosed'))).toBe(true);
		expect(r.errors.some((e) => e.includes('search_b') && e.includes('differ'))).toBe(true);
	});

	it('enforces key names and feature namespaces', () => {
		put(feature('search'), 'en', { count: 'bare', search_Bad: 'x', other_key: 'y' });
		const r = run();
		expect(r.errors).toHaveLength(3);
		// Shared keys are bare glossary terms; a feature may not redefine one.
		put(shared, 'en', { torrent: 'Torrent', search_x: 'x' });
		put(feature('search'), 'en', { search_x: 'y' });
		const shared_ = run().errors;
		expect(shared_).toHaveLength(1);
		expect(shared_[0]).toMatch(/search_x.*duplicate key/);
	});

	it('rejects non-string messages and bad JSON', () => {
		put(feature('a'), 'en', { a_x: 1 });
		put(feature('b'), 'en', {});
		writeFileSync(join(root, feature('b'), 'messages', 'en.json'), '{oops');
		const r = run();
		expect(r.errors.some((e) => e.includes('must be a string'))).toBe(true);
		expect(r.errors.some((e) => e.includes('en.json'))).toBe(true);
	});
});

describe('topLevelKeys', () => {
	it('lists keys in order with repeats, ignoring nested keys and string values', () => {
		expect(topLevelKeys('{"a":"x: \\"b\\":","b":{"a":1},"a":"y"}')).toEqual(['a', 'b', 'a']);
	});
});

describe('same-file duplicates and coverage', () => {
	it('fails on a key repeated within one file', () => {
		put(feature('a'), 'en', {});
		writeFileSync(join(root, feature('a'), 'messages', 'en.json'), '{"a_x":"1","a_x":"2"}');
		const r = run();
		expect(r.errors).toHaveLength(1);
		expect(r.errors[0]).toMatch(/a_x.*duplicate key in the same file/);
	});
	it('reports the share of en keys that zh covers', () => {
		put(feature('a'), 'en', { a_1: '1', a_2: '2', a_3: '3', a_4: '4' });
		put(feature('a'), 'zh', { a_1: '一', a_2: '二' });
		expect(run().coverage).toEqual({ en: 1, zh: 0.5 });
		expect(merge([]).coverage.zh).toBe(1);
	});
});

describe('mergeMessages', () => {
	it('writes sorted output only when valid', () => {
		put(feature('search'), 'en', { search_b: 'B', search_a: 'A' });
		expect(mergeMessages(root).errors).toEqual([]);
		const en = JSON.parse(readFileSync(join(root, 'messages/en.json'), 'utf8'));
		expect(Object.keys(en)).toEqual(['$schema', 'search_a', 'search_b']);

		put(feature('search'), 'en', { search_a: '{' });
		expect(mergeMessages(root).errors).not.toEqual([]);
		expect(JSON.parse(readFileSync(join(root, 'messages/en.json'), 'utf8')).search_b).toBe('B');
	});
});

describe('injectBootLocales', () => {
	const html = '<script>var L=/*boot-locales*/{"en":1}/*end*/;</script>';

	it('bakes only the locales the browser may pick on its own', () => {
		expect(injectBootLocales(html, { en: 1, zh: 0.35 })).toContain(
			'/*boot-locales*/{"en":1}/*end*/'
		);
		expect(injectBootLocales(html, { en: 1, zh: 0.96 })).toContain(
			'/*boot-locales*/{"en":1,"zh":1}/*end*/'
		);
	});

	it('is idempotent and says so when the marker is missing', () => {
		const once = injectBootLocales(html, { en: 1, zh: 0.96 })!;
		expect(injectBootLocales(once, { en: 1, zh: 0.96 })).toBe(once);
		expect(injectBootLocales('<script></script>', { en: 1, zh: 1 })).toBeNull();
	});
});
