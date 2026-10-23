import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	clearDraft,
	DRAFT_MAX_BYTES,
	DRAFT_PREFIX,
	draftOf,
	planRestore,
	readDraft,
	writeDraft
} from './draft';
import { spec, sv, values } from './testing';
import type { FieldSpec } from './types';
import { dirtyNames, fromValues, RESET, stage, type SectionState } from './values';

const SECRET = 'hunter2-very-secret';

const specs: Record<string, FieldSpec> = {
	'crawler.intensity': spec('crawler.intensity', {
		kind: 'enum',
		enum: ['low', 'balanced', 'aggressive']
	}),
	'crawler.timeout_secs': spec('crawler.timeout_secs', {
		kind: 'integer',
		minimum: 1,
		maximum: 120
	}),
	'crawler.token': spec('crawler.token', { writeOnly: true }),
	'crawler.port': spec('crawler.port', { kind: 'integer' }),
	'transport.utp': spec('transport.utp', { kind: 'boolean' })
};
const specOf = (s: string, n: string) => specs[`${s}.${n}`];

const current = (): Record<string, SectionState> => ({
	crawler: fromValues(
		values('crawler', 3, {
			intensity: sv('balanced'),
			timeout_secs: sv(12, 'db'),
			token: sv(null, 'db'),
			port: sv(6881, 'env', { env: 'LODESTAR_PORT', read_only: true })
		})
	),
	transport: fromValues(values('transport', 1, { utp: sv(true) }))
});

const edited = () => {
	const s = current();
	s.crawler = stage(s.crawler!, 'intensity', 'low');
	s.crawler = stage(s.crawler, 'timeout_secs', RESET);
	s.crawler = stage(s.crawler, 'token', SECRET, specs['crawler.token']);
	s.transport = stage(s.transport!, 'utp', false);
	return s;
};

beforeEach(() => sessionStorage.clear());
afterEach(() => vi.restoreAllMocks());

describe('drafts', () => {
	it('round-trips, without secrets or locked fields', () => {
		const d = draftOf(edited(), specOf, 1000)!;
		expect(d).toEqual({
			v: 1,
			at: 1000,
			sections: {
				crawler: {
					revision: 3,
					base: { intensity: 'balanced', timeout_secs: 12 },
					mine: { intensity: 'low' },
					reset: ['timeout_secs']
				},
				transport: { revision: 1, base: { utp: true }, mine: { utp: false }, reset: [] }
			}
		});
		expect(writeDraft('crawler', d)).toBe(true);
		const raw = sessionStorage.getItem(DRAFT_PREFIX + 'crawler')!;
		expect(raw).not.toContain(SECRET);
		expect(raw).not.toContain('token');
		expect(readDraft('crawler')).toEqual(d);

		const plan = planRestore(readDraft('crawler')!, current(), specOf);
		expect(plan).toMatchObject({ fields: 3, dropped: 0, conflicts: [] });
		expect(dirtyNames(plan.sections.crawler!).sort()).toEqual(['intensity', 'timeout_secs']);
		expect(plan.sections.crawler!.fields.timeout_secs!.mine).toBe(RESET);
		expect(dirtyNames(plan.sections.transport!)).toEqual(['utp']);
	});

	it('removes the draft when nothing is left to keep', () => {
		writeDraft('crawler', draftOf(edited(), specOf, 1)!);
		expect(draftOf(current(), specOf, 2)).toBeNull();
		writeDraft('crawler', null);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'crawler')).toBeNull();
		writeDraft('crawler', draftOf(edited(), specOf, 1)!);
		clearDraft('crawler');
		expect(readDraft('crawler')).toBeNull();
	});

	it('keeps no draft over 64 KiB', () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		const big = {
			v: 1 as const,
			at: 0,
			sections: {
				crawler: { revision: 1, base: {}, mine: { x: 'a'.repeat(DRAFT_MAX_BYTES) }, reset: [] }
			}
		};
		writeDraft('crawler', draftOf(edited(), specOf, 1));
		expect(writeDraft('crawler', big)).toBe(false);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'crawler')).toBeNull();
		expect(console.warn).toHaveBeenCalled();
	});

	it('survives storage that throws on read, write or quota', () => {
		const d = draftOf(edited(), specOf, 1)!;
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
			throw new DOMException('full', 'QuotaExceededError');
		});
		expect(writeDraft('crawler', d)).toBe(false);
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
			throw new DOMException('blocked', 'SecurityError');
		});
		expect(readDraft('crawler')).toBeNull();
		vi.spyOn(Storage.prototype, 'removeItem').mockImplementation(() => {
			throw new DOMException('blocked', 'SecurityError');
		});
		expect(() => clearDraft('crawler')).not.toThrow();
	});

	it('survives storage that is absent or blocked outright', () => {
		const desc = Object.getOwnPropertyDescriptor(globalThis, 'sessionStorage')!;
		Object.defineProperty(globalThis, 'sessionStorage', {
			configurable: true,
			get() {
				throw new DOMException('blocked', 'SecurityError');
			}
		});
		try {
			expect(writeDraft('crawler', draftOf(edited(), specOf, 1))).toBe(false);
			expect(readDraft('crawler')).toBeNull();
		} finally {
			Object.defineProperty(globalThis, 'sessionStorage', desc);
		}
	});

	it.each([
		'not json',
		'null',
		'[]',
		'{"v":2,"at":1,"sections":{}}',
		'{"v":1,"at":"x","sections":{}}',
		'{"v":1,"at":1,"sections":{"crawler":{"revision":"3","base":{},"mine":{}}}}',
		'{"v":1,"at":1,"sections":{"crawler":{"revision":3,"base":[],"mine":{}}}}'
	])('reads corrupt JSON %j as no draft, and removes it', (text) => {
		sessionStorage.setItem(DRAFT_PREFIX + 'crawler', text);
		expect(readDraft('crawler')).toBeNull();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'crawler')).toBeNull();
	});

	it('drops and counts fields the schema no longer allows', () => {
		const draft = {
			v: 1 as const,
			at: 1,
			sections: {
				crawler: {
					revision: 3,
					base: {
						intensity: 'balanced',
						timeout_secs: 12,
						gone: 1,
						port: 1,
						token: 'x',
						mistyped: 1
					},
					mine: { intensity: 'extreme', timeout_secs: 30, gone: 2, port: 2, token: SECRET },
					reset: ['mistyped']
				},
				removed: { revision: 1, base: { a: 1 }, mine: { a: 2 }, reset: [] },
				transport: { revision: 1, base: { utp: true }, mine: { utp: 'no' }, reset: [] }
			}
		};
		const plan = planRestore(draft, current(), specOf);
		// intensity: not an option; gone: unknown; port: locked; token: secret; mistyped: unknown;
		// removed.a: no such section; utp: wrong type. Only timeout_secs survives.
		expect(plan.dropped).toBe(7);
		expect(plan.fields).toBe(1);
		expect(dirtyNames(plan.sections.crawler!)).toEqual(['timeout_secs']);
		expect(dirtyNames(plan.sections.transport!)).toEqual([]);
	});

	it('merges the draft onto values that changed since', () => {
		const d = draftOf(edited(), specOf, 1)!;
		const now = current();
		now.crawler = fromValues(
			values('crawler', 5, {
				intensity: sv('aggressive', 'db'), // changed elsewhere too: a conflict
				timeout_secs: sv(12, 'db'),
				token: sv(null, 'db'),
				port: sv(6881, 'env', { env: 'LODESTAR_PORT', read_only: true })
			})
		);
		const plan = planRestore(d, now, specOf);
		expect(plan.conflicts).toEqual(['crawler.intensity']);
		expect(plan.sections.crawler!.revision).toBe(5);
		expect(plan.sections.crawler!.fields.intensity!.conflict).toEqual({
			mine: 'low',
			theirs: 'aggressive'
		});
	});

	it('leaves edits made since the page loaded alone', () => {
		const d = draftOf(edited(), specOf, 1)!;
		const now = current();
		now.transport = stage(now.transport!, 'utp', false);
		now.crawler = stage(now.crawler!, 'intensity', 'aggressive');
		const plan = planRestore(d, now, specOf);
		expect(plan.sections.crawler!.fields.intensity!.mine).toBe('aggressive');
		// Counted: only what the draft brings back (timeout_secs' reset), not the two newer edits.
		expect(plan.fields).toBe(1);
	});
});
