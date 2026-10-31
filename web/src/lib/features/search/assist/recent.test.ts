import { afterEach, describe, expect, it, vi } from 'vitest';
import { RECENT_MAX, addRecent, loadRecent } from './recent';

afterEach(() => {
	localStorage.clear();
	vi.restoreAllMocks();
});

describe('recent queries', () => {
	it('keeps the newest first, moves a repeat up, and caps the list', () => {
		for (let i = 0; i < RECENT_MAX + 3; i++) addRecent(`q${i}`);
		addRecent('q5');
		const got = loadRecent();
		expect(got).toHaveLength(RECENT_MAX);
		expect(got[0]).toBe('q5');
		expect(got.filter((q) => q === 'q5')).toHaveLength(1);
	});

	it('ignores blank and absurdly long queries', () => {
		addRecent('   ');
		addRecent('x'.repeat(500));
		expect(loadRecent()).toEqual([]);
	});

	it('reads corrupt or foreign storage as empty', () => {
		for (const bad of ['{nope', '{"a":1}', '"str"', '[1,null,"ok",""]']) {
			localStorage.setItem('lodestar.search.recent', bad);
			expect(loadRecent()).toEqual(bad.includes('ok') ? ['ok'] : []);
		}
	});

	it('survives storage that throws on read and on write', () => {
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
			throw new Error('blocked');
		});
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
			throw new Error('full');
		});
		expect(loadRecent()).toEqual([]);
		expect(() => addRecent('res:1080p')).not.toThrow();
	});
});
