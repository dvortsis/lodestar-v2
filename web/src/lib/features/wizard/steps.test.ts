import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { STEPS, advance, back, readProgress, stepIndex, writeProgress } from './steps';

beforeEach(() => sessionStorage.clear());
afterEach(() => vi.restoreAllMocks());

describe('wizard progress', () => {
	it('starts at the network check', () => {
		expect(readProgress()).toEqual({ step: 'network', skipped: [], done: [], restart: false });
	});

	it('advances through every step in order and stops at Done', () => {
		let p = readProgress();
		const seen = [p.step];
		for (let i = 0; i < STEPS.length + 2; i++) {
			p = advance(p, 'done');
			if (seen.at(-1) !== p.step) seen.push(p.step);
		}
		expect(seen).toEqual(STEPS.map((s) => s.id));
	});

	it('records done and skipped apart, and revisiting a step replaces its mark', () => {
		let p = advance(readProgress(), 'skipped');
		expect(p).toMatchObject({ step: 'how', skipped: ['network'], done: [] });
		p = back(p);
		expect(p.step).toBe('network');
		p = advance(p, 'done');
		expect(p).toMatchObject({ skipped: [], done: ['network'] });
		expect(back(readProgress()).step).toBe('network');
	});

	it('resumes from sessionStorage', () => {
		writeProgress({ step: 'storage', skipped: ['intensity'], done: ['network', 'how'] });
		expect(readProgress()).toEqual({
			step: 'storage',
			skipped: ['intensity'],
			done: ['network', 'how'],
			restart: false
		});
	});

	it('starts over on anything unreadable', () => {
		for (const raw of ['{', 'null', '[]', '{"step":"nope"}', '"x"']) {
			sessionStorage.setItem('lodestar.wizard', raw);
			expect(readProgress().step).toBe('network');
		}
		sessionStorage.setItem('lodestar.wizard', '{"step":"how","skipped":["x","updates",3]}');
		expect(readProgress()).toEqual({ step: 'how', skipped: ['updates'], done: [], restart: false });
	});

	it('works with storage blocked', () => {
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
			throw new Error('blocked');
		});
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
			throw new Error('blocked');
		});
		expect(readProgress().step).toBe('network');
		expect(() => writeProgress(readProgress())).not.toThrow();
	});

	it('remembers that a saved field needs a restart', () => {
		const p = advance(advance(readProgress(), 'done', true), 'done');
		expect(p.restart).toBe(true);
	});

	it('Skip exists only on the optional steps', () => {
		expect(STEPS.filter((s) => !s.optional).map((s) => s.id)).toEqual(['how', 'done']);
		expect(stepIndex('done')).toBe(STEPS.length - 1);
	});
});
