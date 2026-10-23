import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { keys } from '$lib/keys';
import { toasts } from '$lib/ui/feedback/toasts.svelte';

const get = vi.hoisted(() => vi.fn());
const patch = vi.hoisted(() => vi.fn());
const handlers = vi.hoisted(() => ({ settings: undefined as ((d: unknown) => void) | undefined }));

vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get, patch }
}));
vi.mock('$lib/state/events.svelte', () => ({
	events: {
		on: (_topic: string, h: (d: unknown) => void) => {
			handlers.settings = h;
			return () => (handlers.settings = undefined);
		}
	}
}));

import { startShortcuts } from './boot';

const sectionOf = (shortcuts: unknown, shortcut_keys: unknown, revision = 1) => ({
	section: 'preferences',
	revision,
	restart_required: [],
	fields: {
		shortcuts: { value: shortcuts, read_only: false, env: null, source: 'database' },
		shortcut_keys: { value: shortcut_keys, read_only: false, env: null, source: 'database' }
	}
});
const info = (id: string) => keys.list().find((c) => c.id === id);
const flush = () => new Promise((r) => setTimeout(r, 0));

let stop: (() => void) | undefined;
beforeEach(() => {
	localStorage.clear();
	get.mockReset().mockResolvedValue(sectionOf(true, {}));
	patch.mockReset();
	toasts.items = [];
	keys.setSingleKeys(true);
});
afterEach(() => {
	stop?.();
	stop = undefined;
	for (const id of ['results.next', 'results.previous', 'search.focus']) keys.remap(id, null);
	vi.restoreAllMocks();
});

describe('startShortcuts', () => {
	it('registers the sheet and the palette toggle synchronously', () => {
		stop = startShortcuts();
		expect(info('help.shortcuts')).toMatchObject({ keys: ['?'], title: 'Keyboard shortcuts' });
		expect(info('shortcuts.single_keys')).toMatchObject({
			keys: [],
			palette: true,
			title: 'Turn off single-key shortcuts'
		});
		stop();
		stop = undefined;
		expect(info('help.shortcuts')).toBeUndefined();
	});

	it('applies the cached mapping before the server answers', () => {
		localStorage.setItem(
			'lodestar.shortcuts',
			JSON.stringify({ shortcuts: false, shortcut_keys: { 'results.next': 'n' } })
		);
		get.mockReturnValue(new Promise(() => {}));
		const off = keys.register({ id: 'results.next', title: 'Next', run: () => {} });
		stop = startShortcuts();
		expect(keys.singleKeys).toBe(false);
		expect(info('results.next')?.keys).toEqual(['n']);
		off();
	});

	it('survives storage that throws', async () => {
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
			throw new Error('blocked');
		});
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
			throw new Error('blocked');
		});
		get.mockResolvedValue(sectionOf(false, {}));
		stop = startShortcuts();
		await vi.waitFor(() => expect(keys.singleKeys).toBe(false));
	});

	it('applies the server mapping, caches it, and resets what was removed', async () => {
		const off = [
			keys.register({ id: 'results.next', title: 'Next', run: () => {} }),
			keys.register({ id: 'results.previous', title: 'Previous', run: () => {} })
		];
		get.mockResolvedValue(sectionOf(false, { 'results.next': 'n', 'results.previous': 'p' }, 2));
		stop = startShortcuts();
		await vi.waitFor(() => expect(info('results.next')?.keys).toEqual(['n']));
		expect(keys.singleKeys).toBe(false);
		expect(info('results.previous')?.keys).toEqual(['p']);
		expect(JSON.parse(localStorage.getItem('lodestar.shortcuts')!)).toEqual({
			shortcuts: false,
			shortcut_keys: { 'results.next': 'n', 'results.previous': 'p' }
		});

		get.mockResolvedValue(sectionOf(true, { 'results.next': 'n' }, 3));
		handlers.settings?.({ section: 'preferences', revision: 3 });
		await vi.waitFor(() => expect(info('results.previous')?.keys).toEqual(['k']));
		expect(keys.singleKeys).toBe(true);
		expect(info('results.next')?.keys).toEqual(['n']);
		off.forEach((f) => f());
	});

	it('ignores a malformed value and says so once', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		get.mockResolvedValue(sectionOf('yes', { 'Bad Id': 'n', 'results.next': 5 }));
		stop = startShortcuts();
		await flush();
		await flush();
		expect(keys.singleKeys).toBe(true);
		get.mockResolvedValue(sectionOf(true, ['x']));
		handlers.settings?.({ section: 'preferences', revision: 9 });
		await flush();
		expect(keys.singleKeys).toBe(true);
		expect(warn.mock.calls.length).toBeLessThanOrEqual(1);
	});

	it('fetches again for a preferences event, and only for that', async () => {
		stop = startShortcuts();
		await flush();
		expect(get).toHaveBeenCalledTimes(1);
		handlers.settings?.({ section: 'crawler', revision: 1 });
		await flush();
		expect(get).toHaveBeenCalledTimes(1);
		handlers.settings?.({ section: 'preferences', revision: 2 });
		await flush();
		expect(get).toHaveBeenCalledTimes(2);
	});

	it('keeps the cached state, quietly, when the fetch fails', async () => {
		localStorage.setItem(
			'lodestar.shortcuts',
			JSON.stringify({ shortcuts: false, shortcut_keys: {} })
		);
		get.mockRejectedValue(new ApiError(0, 'network', 'down'));
		stop = startShortcuts();
		await flush();
		expect(keys.singleKeys).toBe(false);
		expect(toasts.items).toEqual([]);
	});
});

describe('the palette toggle', () => {
	it('PATCHes shortcuts without if_revision, applies the answer and toasts', async () => {
		patch.mockResolvedValue(sectionOf(false, {}, 4));
		stop = startShortcuts();
		await flush();
		expect(keys.run('shortcuts.single_keys')).toBe(true);
		await vi.waitFor(() => expect(keys.singleKeys).toBe(false));
		expect(patch).toHaveBeenCalledWith('/settings/{section}', {
			path: { section: 'preferences' },
			body: { shortcuts: false }
		});
		expect(toasts.items.map((t) => t.text)).toEqual(['Single-key shortcuts are off']);
		expect(info('shortcuts.single_keys')?.title).toBe('Turn on single-key shortcuts');
	});

	it('turns them back on', async () => {
		get.mockResolvedValue(sectionOf(false, {}));
		patch.mockResolvedValue(sectionOf(true, {}, 5));
		stop = startShortcuts();
		await vi.waitFor(() => expect(keys.singleKeys).toBe(false));
		keys.run('shortcuts.single_keys');
		await vi.waitFor(() => expect(keys.singleKeys).toBe(true));
		expect(patch.mock.calls[0]![1].body).toEqual({ shortcuts: true });
		expect(toasts.items.map((t) => t.text)).toEqual(['Single-key shortcuts are on']);
	});

	it('shows an error toast and changes nothing when the save fails', async () => {
		patch.mockRejectedValue(new ApiError(500, 'internal', 'boom'));
		stop = startShortcuts();
		await flush();
		keys.run('shortcuts.single_keys');
		await vi.waitFor(() => expect(toasts.items).toHaveLength(1));
		expect(toasts.items[0]).toMatchObject({ kind: 'error' });
		expect(keys.singleKeys).toBe(true);
	});
});
