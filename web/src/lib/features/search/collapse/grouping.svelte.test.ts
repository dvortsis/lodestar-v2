// "Group versions" in the URL and remembered per account (owner §2.66 item 2).
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.setConfig({ testTimeout: 20_000 });
const WAIT = { timeout: 10_000 };

const get = vi.hoisted(() => vi.fn());
const patch = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get, patch }
}));

const { ApiError } = await import('$lib/api/client');
const { CACHE, createGrouping, groupHref, groupedIn } = await import('./grouping.svelte');

const prefs = (on: unknown) => ({
	section: 'preferences',
	revision: 3,
	fields: { group_versions: { value: on }, density: { value: 'comfortable' } }
});

beforeEach(() => {
	get.mockReset();
	patch.mockReset();
	localStorage.clear();
});

describe('groupedIn and groupHref', () => {
	it('follows the URL, then the account', () => {
		expect(groupedIn('?q=a&group=1', false)).toBe(true);
		expect(groupedIn('?q=a&group=0', true)).toBe(false);
		expect(groupedIn('?q=a', true)).toBe(true);
		expect(groupedIn('?q=a', false)).toBe(false);
		expect(groupedIn('?q=a&group=yes', false)).toBe(false); // an unknown value: the account's
	});

	it('writes the choice out and keeps every other parameter', () => {
		const url = new URL('http://x/search?q=lantern&sort=-size#top');
		expect(groupHref(url, false)).toBe('/search?q=lantern&sort=-size&group=0#top');
		expect(groupHref(new URL('http://x/search?group=0&q=a'), true)).toBe('/search?group=1&q=a');
	});
});

describe('createGrouping', () => {
	it('is on by default, and starts from this browser’s cache', () => {
		expect(createGrouping().pref).toBe(true);
		localStorage.setItem(CACHE, '0');
		expect(createGrouping().pref).toBe(false);
	});

	it('reads the account’s choice once and caches it', async () => {
		get.mockResolvedValue(prefs(false));
		const g = createGrouping();
		await g.load();
		await g.load();
		expect(get).toHaveBeenCalledTimes(1);
		expect(get).toHaveBeenCalledWith('/settings/{section}', { path: { section: 'preferences' } });
		expect(g.pref).toBe(false);
		expect(localStorage.getItem(CACHE)).toBe('0');
	});

	it('keeps the cached choice when the read fails or the value is not a boolean', async () => {
		localStorage.setItem(CACHE, '0');
		get.mockRejectedValue(new Error('offline'));
		const a = createGrouping();
		await a.load();
		expect(a.pref).toBe(false);
		get.mockResolvedValue(prefs('on'));
		const b = createGrouping();
		await b.load();
		expect(b.pref).toBe(false);
	});

	it('saves a choice for the account as the one field', async () => {
		patch.mockResolvedValue({ revision: 8 });
		const g = createGrouping();
		await g.remember(false);
		expect(g.pref).toBe(false);
		expect(localStorage.getItem(CACHE)).toBe('0');
		expect(patch).toHaveBeenCalledWith('/settings/{section}', {
			path: { section: 'preferences' },
			body: { group_versions: false }
		});
	});

	it('saves one at a time and keeps the last of quick choices', async () => {
		let land!: () => void;
		patch.mockImplementationOnce(() => new Promise<void>((r) => (land = r)));
		patch.mockResolvedValue({});
		const g = createGrouping();
		const first = g.remember(false);
		await vi.waitFor(() => expect(patch).toHaveBeenCalledTimes(1), WAIT);
		// Two more choices while the first is still out: neither is sent before it lands.
		void g.remember(true);
		const last = g.remember(false);
		expect(patch).toHaveBeenCalledTimes(1);
		land();
		await first;
		await last;
		// The middle choice was overtaken before it was sent; the last one lands last.
		expect(patch.mock.calls.map((c) => c[1].body.group_versions)).toEqual([false, false]);
		expect(g.pref).toBe(false);
	});

	it('gives up quietly on a failure, keeping the choice here', async () => {
		patch.mockRejectedValue(new ApiError(500, 'internal', 'down'));
		const g = createGrouping();
		await g.remember(false);
		expect(patch).toHaveBeenCalledTimes(1);
		expect(g.pref).toBe(false);
	});

	it('keeps the choice a search was asked with until the search changes', async () => {
		localStorage.setItem(CACHE, '1');
		get.mockResolvedValue(prefs(false));
		const g = createGrouping();
		expect(g.pin('lantern')).toBe(true);
		await g.load();
		expect(g.pref).toBe(false);
		expect(g.pin('lantern')).toBe(true); // a view change: the rows stay as asked
		expect(g.applied).toBe(true);
		expect(g.pin('keeper')).toBe(false); // a new search takes the account's answer
		expect(g.applied).toBe(false);
	});

	it('never lets an older answer undo a choice made while it was out', async () => {
		let answer!: (v: unknown) => void;
		get.mockImplementationOnce(() => new Promise((r) => (answer = r)));
		const g = createGrouping();
		const loading = g.load();
		patch.mockResolvedValue({});
		await g.remember(false);
		answer(prefs(true));
		await loading;
		await vi.waitFor(() => expect(g.pref).toBe(false), WAIT);
		expect(localStorage.getItem(CACHE)).toBe('0');
	});
});
