import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';

const api = vi.hoisted(() => ({
	listSavedSearches: vi.fn(),
	getSavedSearch: vi.fn(),
	listAlerts: vi.fn(),
	markAlertsRead: vi.fn()
}));
vi.mock('./api', async (orig) => ({ ...(await orig<typeof import('./api')>()), ...api }));

import { createAlertDetail } from './detail.svelte';
import { createAlertsList } from './list.svelte';
import { row, saved } from './fixtures';

const flush = () => new Promise((r) => setTimeout(r));

beforeEach(() => {
	vi.resetAllMocks();
	api.markAlertsRead.mockResolvedValue({ ok: true });
});

describe('the list controller', () => {
	it('loads in the server’s order and keeps the rows when a refresh fails', async () => {
		const a = saved({ id: 2, name: 'B' });
		const b = saved({ id: 1, name: 'A' });
		api.listSavedSearches.mockResolvedValueOnce([a, b]);
		const list = createAlertsList();
		await list.load();
		expect(list.items?.map((s) => s.id)).toEqual([2, 1]);
		api.listSavedSearches.mockRejectedValueOnce(new ApiError(0, 'network', 'down'));
		await list.load();
		expect(list.error?.code).toBe('network');
		expect(list.items).toHaveLength(2);
	});
	it('refetches once for events delivered together', async () => {
		api.listSavedSearches.mockResolvedValue([saved()]);
		const list = createAlertsList();
		list.refreshSoon();
		list.refreshSoon();
		list.refreshSoon();
		await flush();
		expect(api.listSavedSearches).toHaveBeenCalledTimes(1);
	});
	it('replaces and removes a row in place', async () => {
		api.listSavedSearches.mockResolvedValue([saved({ id: 1 }), saved({ id: 2 })]);
		const list = createAlertsList();
		await list.load();
		list.replace(saved({ id: 2, paused: true }));
		expect(list.items?.map((s) => s.paused)).toEqual([false, true]);
		list.remove(1);
		expect(list.items?.map((s) => s.id)).toEqual([2]);
	});
});

describe('the detail controller', () => {
	const open = async (
		rows = [row(5, false), row(4, false), row(3, true)],
		next: string | null = null
	) => {
		api.getSavedSearch.mockResolvedValue(saved({ unread: 2 }));
		api.listAlerts.mockResolvedValue({ items: rows, next_cursor: next });
		const d = createAlertDetail(1);
		await d.load();
		return d;
	};

	it('loads both together, places the mark and marks read through the top id, once', async () => {
		const d = await open();
		expect(d.opened).toBe(2);
		expect(d.mark).toEqual({ before: 2, at: row(3, true).created_at });
		expect(api.markAlertsRead).toHaveBeenCalledTimes(1);
		expect(api.markAlertsRead).toHaveBeenCalledWith(1, 5);
	});
	it('does not mark read when the top row is read or there are no rows', async () => {
		await open([row(3, true), row(2, true)]);
		await open([]);
		expect(api.markAlertsRead).not.toHaveBeenCalled();
	});
	it('ignores a failed read mark', async () => {
		api.markAlertsRead.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		const d = await open();
		await flush();
		expect(d.error).toBeUndefined();
		expect(api.markAlertsRead).toHaveBeenCalledTimes(1);
	});
	it('keeps the opened count after reading, and shows the server’s count after an event', async () => {
		const d = await open();
		expect(d.fresh).toBe(0);
		api.getSavedSearch.mockResolvedValue(saved({ unread: 3 }));
		await d.refreshSearch();
		expect(d.fresh).toBe(3);
		expect(d.opened).toBe(2);
	});
	it('keeps the old counts and reports the error when a refresh fails', async () => {
		const d = await open();
		api.getSavedSearch.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		await d.refreshSearch();
		expect(d.error?.status).toBe(500);
		expect(d.search?.name).toBe('Lantern Keeper releases');
		expect(d.rows).toHaveLength(3);
	});
	it('pages by the cursor and moves the mark as the first read row arrives', async () => {
		const d = await open([row(5, false), row(4, false)], 'c1');
		expect(d.hasMore).toBe(true);
		expect(d.mark).toBeNull();
		api.listAlerts.mockResolvedValueOnce({ items: [row(3, true)], next_cursor: null });
		await d.loadMore();
		expect(api.listAlerts).toHaveBeenLastCalledWith(1, 'c1', expect.anything());
		expect(d.rows.map((r) => r.id)).toEqual([5, 4, 3]);
		expect(d.mark?.before).toBe(2);
		expect(d.hasMore).toBe(false);
		expect(api.markAlertsRead).toHaveBeenCalledTimes(1); // paging marks nothing
	});
	it('starts over from the first page on a bad cursor', async () => {
		const d = await open([row(5, true)], 'c1');
		api.listAlerts.mockRejectedValueOnce(new ApiError(400, 'bad_cursor', 'x'));
		api.listAlerts.mockResolvedValueOnce({
			items: [row(6, true), row(5, true)],
			next_cursor: null
		});
		await d.loadMore();
		expect(api.listAlerts).toHaveBeenLastCalledWith(1, null, expect.anything());
		expect(d.rows.map((r) => r.id)).toEqual([6, 5]);
	});
	it('stops paging on any other error, keeps the rows and retries from the same cursor', async () => {
		const d = await open([row(5, true)], 'c1');
		api.listAlerts.mockRejectedValueOnce(new ApiError(500, 'internal', 'x'));
		await d.loadMore();
		expect(d.hasMore).toBe(false);
		expect(d.moreError?.status).toBe(500);
		expect(d.rows).toHaveLength(1);
		api.listAlerts.mockResolvedValueOnce({ items: [row(4, true)], next_cursor: null });
		await d.retryMore();
		expect(api.listAlerts).toHaveBeenLastCalledWith(1, 'c1', expect.anything());
		expect(d.moreError).toBeUndefined();
		expect(d.rows).toHaveLength(2);
	});
	it('Show loads the first page again, places the mark anew and marks read again', async () => {
		const d = await open();
		api.getSavedSearch.mockResolvedValue(saved({ unread: 1 }));
		api.listAlerts.mockResolvedValue({ items: [row(6, false), row(5, true)], next_cursor: null });
		await d.show();
		expect(d.mark).toEqual({ before: 1, at: row(5, true).created_at });
		expect(api.markAlertsRead).toHaveBeenLastCalledWith(1, 6);
		expect(d.fresh).toBe(0);
	});
	it('reports not found, with nothing to show', async () => {
		api.getSavedSearch.mockRejectedValue(new ApiError(404, 'saved_search_not_found', 'x'));
		api.listAlerts.mockRejectedValue(new ApiError(404, 'saved_search_not_found', 'x'));
		const d = createAlertDetail(9);
		await d.load();
		expect(d.error?.code).toBe('saved_search_not_found');
		expect(d.search).toBeUndefined();
	});
});
