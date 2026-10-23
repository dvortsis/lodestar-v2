import { describe, expect, it } from 'vitest';
import { spec, sv, values } from './testing';
import {
	applySaved,
	buildBatch,
	dirtyNames,
	discardAll,
	equal,
	fromValues,
	isDirty,
	RESET,
	stage
} from './values';

describe('equal', () => {
	it('is deep JSON equality', () => {
		expect(equal({ a: 1, b: [1, { c: 2 }] }, { b: [1, { c: 2 }], a: 1 })).toBe(true);
		expect(equal({ a: 1 }, { a: 1, b: 2 })).toBe(false);
		expect(equal([1, 2], [2, 1])).toBe(false);
		expect(equal(0, -0)).toBe(true);
		expect(equal(null, null)).toBe(true);
		expect(equal(null, 0)).toBe(false);
		expect(equal(null, {})).toBe(false);
		expect(equal('1', 1)).toBe(false);
		expect(equal([], {})).toBe(false);
		expect(equal({ a: [] }, { a: {} })).toBe(false);
		expect(equal(NaN, NaN)).toBe(true);
		expect(equal(true, true)).toBe(true);
	});

	it('never compares by reference', () => {
		const a = { x: [1] };
		expect(equal(a, structuredClone(a))).toBe(true);
	});
});

describe('dirty tracking', () => {
	const base = () =>
		fromValues(
			values('crawler', 3, {
				intensity: sv('balanced'),
				timeout_secs: sv(12, 'db'),
				map: sv({ a: 1, b: 2 }, 'file'),
				locked: sv('x', 'env', { env: 'LODESTAR_X', read_only: true }),
				ro: sv('/data', 'default', { read_only: true })
			})
		);

	it('counts a field dirty iff its staged value differs from the saved one', () => {
		let s = stage(base(), 'intensity', 'low');
		expect(dirtyNames(s)).toEqual(['intensity']);
		s = stage(s, 'intensity', 'balanced');
		expect(dirtyNames(s)).toEqual([]);
		expect(s.fields.intensity!.mine).toBeUndefined();
	});

	it('compares deeply: a reordered object is not a change', () => {
		const s = stage(base(), 'map', { b: 2, a: 1 });
		expect(dirtyNames(s)).toEqual([]);
	});

	it('remembers the base the edit started from', () => {
		let s = stage(base(), 'timeout_secs', 30);
		s = stage(s, 'timeout_secs', 40);
		expect(s.fields.timeout_secs).toMatchObject({ base: 12, mine: 40 });
	});

	it('makes RESET dirty only for a value saved in the app', () => {
		expect(isDirty(stage(base(), 'timeout_secs', RESET).fields.timeout_secs!)).toBe(true);
		expect(stage(base(), 'intensity', RESET).fields.intensity!.mine).toBeUndefined();
		expect(stage(base(), 'map', RESET).fields.map!.mine).toBeUndefined();
	});

	it('never stages a locked field', () => {
		const s0 = base();
		expect(stage(s0, 'locked', 'y')).toBe(s0);
		expect(stage(s0, 'ro', '/other')).toBe(s0);
		expect(stage(s0, 'locked', RESET)).toBe(s0);
		expect(stage(s0, 'intensity', 'low', spec('crawler.intensity', { readOnly: true }))).toBe(s0);
		expect(stage(s0, 'nope', 1)).toBe(s0);
	});

	it('takes an empty secret as no replacement', () => {
		const secret = spec('crawler.intensity', { writeOnly: true });
		const s = stage(stage(base(), 'intensity', 'hunter2', secret), 'intensity', '', secret);
		expect(dirtyNames(s)).toEqual([]);
	});

	it('discards every staged value', () => {
		const s = discardAll(stage(stage(base(), 'intensity', 'low'), 'timeout_secs', RESET));
		expect(dirtyNames(s)).toEqual([]);
	});
});

describe('the batch body', () => {
	const sections = () => ({
		crawler: fromValues(
			values('crawler', 3, { intensity: sv('balanced'), timeout_secs: sv(12, 'db') })
		),
		transport: fromValues(values('transport', 7, { utp: sv(true) })),
		storage: fromValues(values('storage', 1, { archive_raw: sv(false) }))
	});

	it('sends only dirty fields, each touched section once with if_revision, null for reset', () => {
		const s = sections();
		s.crawler = stage(stage(s.crawler, 'intensity', 'low'), 'timeout_secs', RESET);
		s.transport = stage(s.transport, 'utp', false);
		s.storage = stage(stage(s.storage, 'archive_raw', true), 'archive_raw', false); // back: clean
		expect(buildBatch(s)).toEqual({
			crawler: { if_revision: 3, intensity: 'low', timeout_secs: null },
			transport: { if_revision: 7, utp: false }
		});
	});

	it('is empty with nothing dirty', () => {
		expect(buildBatch(sections())).toEqual({});
	});

	it('can be limited to some fields', () => {
		const s = sections();
		s.crawler = stage(s.crawler, 'intensity', 'low');
		s.transport = stage(s.transport, 'utp', false);
		expect(buildBatch(s, (section) => section === 'transport')).toEqual({
			transport: { if_revision: 7, utp: false }
		});
	});
});

describe('applying a save', () => {
	it('takes saved values and the revision, and cleans what was sent', () => {
		const s0 = stage(
			fromValues(values('crawler', 3, { intensity: sv('balanced'), timeout_secs: sv(12) })),
			'intensity',
			'low'
		);
		const s = applySaved(
			s0,
			values('crawler', 4, { intensity: sv('low', 'db'), timeout_secs: sv(12) }, ['timeout_secs']),
			{ if_revision: 3, intensity: 'low' }
		);
		expect(s.revision).toBe(4);
		expect(s.restart).toEqual(['timeout_secs']);
		expect(dirtyNames(s)).toEqual([]);
		expect(s.fields.intensity!.saved.source).toBe('db');
	});

	it('keeps an edit made while the request was in flight, against the new saved value', () => {
		const s0 = stage(
			fromValues(values('crawler', 3, { intensity: sv('balanced'), timeout_secs: sv(12) })),
			'intensity',
			'aggressive' // changed after 'low' was sent
		);
		const s1 = stage(s0, 'timeout_secs', 30); // a field the save didn't carry
		const s = applySaved(
			s1,
			values('crawler', 4, { intensity: sv('low', 'db'), timeout_secs: sv(12) }),
			{ if_revision: 3, intensity: 'low' }
		);
		expect(dirtyNames(s).sort()).toEqual(['intensity', 'timeout_secs']);
		expect(s.fields.intensity).toMatchObject({ base: 'low', mine: 'aggressive' });
		expect(s.fields.timeout_secs).toMatchObject({ base: 12, mine: 30 });
	});

	it('cleans a sent reset', () => {
		const s0 = stage(fromValues(values('c', 3, { t: sv(12, 'db') })), 't', RESET);
		const s = applySaved(s0, values('c', 4, { t: sv(10) }), { if_revision: 3, t: null });
		expect(dirtyNames(s)).toEqual([]);
	});

	it('drops the edit of an in-flight field that ends up equal to the new saved value', () => {
		const s0 = stage(fromValues(values('c', 3, { t: sv(12) })), 't', 20);
		const s = applySaved(s0, values('c', 4, { t: sv(20, 'db') }), { if_revision: 3 });
		expect(dirtyNames(s)).toEqual([]);
	});
});

describe('editing a conflict', () => {
	it('rebases on theirs, as Keep mine would', () => {
		const s0 = fromValues(values('c', 3, { t: sv('theirs', 'db') }));
		const conflicted = {
			...s0,
			fields: {
				t: {
					saved: sv('theirs', 'db'),
					base: 'old',
					mine: 'mine',
					conflict: { mine: 'mine', theirs: 'theirs' }
				}
			}
		};
		const s = stage(conflicted, 't', 'newer');
		expect(s.fields.t).toEqual({ saved: sv('theirs', 'db'), base: 'theirs', mine: 'newer' });
	});

	it('never resets a secret', () => {
		const s0 = fromValues(values('c', 3, { token: sv(null, 'db') }));
		expect(stage(s0, 'token', RESET, spec('c.token', { writeOnly: true }))).toBe(s0);
	});
});
