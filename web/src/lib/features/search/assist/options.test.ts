import { describe, expect, it } from 'vitest';
import { applyOption, buildOptions, charPos, MAX_OPTIONS, type SuggestItem } from './options';

const item = (over: Partial<SuggestItem> = {}): SuggestItem => ({
	kind: 'value',
	text: '2160p',
	insert: 'res:2160p',
	replace: { start: 0, end: 5 },
	count: 1284,
	...over
});

describe('applyOption', () => {
	it('replaces by characters, not UTF-16 units', () => {
		// "🎬" is one character and two UTF-16 units.
		expect(applyOption('🎬 res:21', { insert: 'res:2160p', replace: { start: 2, end: 9 } })).toBe(
			'🎬 res:2160p'
		);
	});
	it('replaces the whole query for a recent or saved search', () => {
		expect(applyOption('old', { insert: 'new q', replace: { start: 0, end: Infinity } })).toBe(
			'new q'
		);
	});
	it('counts a caret in characters', () => {
		expect(charPos('🎬ab', 2)).toBe(1);
		expect(charPos('🎬ab', 4)).toBe(3);
	});
});

describe('buildOptions', () => {
	it('lists the server items first, then recents and saved searches that fit', () => {
		const got = buildOptions(
			'res',
			[item()],
			['res:1080p big', 'other'],
			[
				{ name: 'Movies', q: 'type:movie res:2160p' },
				{ name: 'Music', q: 'type:music' }
			]
		);
		expect(got.map((o) => [o.kind, o.text])).toEqual([
			['value', '2160p'],
			['recent', 'res:1080p big'],
			['saved', 'Movies']
		]);
		expect(got.find((o) => o.kind === 'value')!.count).toBe(1284);
		expect(got.filter((o) => o.run).map((o) => o.kind)).toEqual(['recent', 'saved']);
	});
	it('shows every recent and saved search for an empty bar, minus the query itself', () => {
		expect(buildOptions('', [], ['a', 'b'], [{ name: 'S', q: 'q' }])).toHaveLength(3);
		expect(buildOptions('a', [], ['a', 'ab'], [])).toHaveLength(1);
	});
	it('runs "did you mean" and caps the list', () => {
		const dym = buildOptions('batmna', [item({ kind: 'did_you_mean', text: 'batman' })], [], []);
		expect(dym[0]!.run).toBe(true);
		const many = Array.from({ length: 30 }, (_, n) => item({ text: `v${n}` }));
		expect(buildOptions('x', many, [], [])).toHaveLength(MAX_OPTIONS);
	});
});
