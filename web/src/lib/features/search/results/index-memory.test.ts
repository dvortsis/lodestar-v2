import { afterEach, describe, expect, it, vi } from 'vitest';
import { recall, remember } from './index-memory';

const KEY = 'lodestar.indexState';
const answer = { growing: true, searchable_torrents: 84_210, growth_per_hour: 2104.5 };

afterEach(() => {
	vi.restoreAllMocks();
	localStorage.clear();
});

describe('index memory', () => {
	it('gives back what it kept', () => {
		remember(answer);
		expect(recall()).toMatchObject(answer);
		remember({ growing: false, searchable_torrents: 5_000_000, growth_per_hour: null });
		expect(recall()).toMatchObject({
			growing: false,
			searchable_torrents: 5_000_000,
			growth_per_hour: null
		});
	});

	it('reads a missing key as nothing remembered', () => {
		expect(recall()).toBeNull();
	});

	it('reads corrupt JSON as nothing remembered', () => {
		localStorage.setItem(KEY, '{"v":1,"growing":tr');
		expect(recall()).toBeNull();
		localStorage.setItem(KEY, 'null');
		expect(recall()).toBeNull();
	});

	it('reads malformed fields as nothing remembered', () => {
		for (const bad of [
			{ v: 1, growing: 'yes', searchable_torrents: 1, growth_per_hour: null },
			{ v: 1, growing: true, searchable_torrents: -1, growth_per_hour: null },
			{ v: 1, growing: true, searchable_torrents: 1.5, growth_per_hour: null },
			{ v: 1, growing: true, searchable_torrents: 1, growth_per_hour: 'fast' },
			{ v: 1, growing: true, growth_per_hour: null }
		]) {
			localStorage.setItem(KEY, JSON.stringify(bad));
			expect(recall()).toBeNull();
		}
	});

	it('reads another version as nothing remembered', () => {
		localStorage.setItem(KEY, JSON.stringify({ ...answer, v: 2 }));
		expect(recall()).toBeNull();
		localStorage.setItem(KEY, JSON.stringify(answer));
		expect(recall()).toBeNull();
	});

	it('survives a storage that throws', () => {
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
			throw new DOMException('denied', 'SecurityError');
		});
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
			throw new DOMException('full', 'QuotaExceededError');
		});
		expect(() => remember(answer)).not.toThrow();
		expect(recall()).toBeNull();
	});
});
