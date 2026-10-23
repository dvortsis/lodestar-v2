import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';
import { keys } from '$lib/keys';
import { stubLayout } from '$lib/ui/lists/testing/layout';
import { toasts } from '$lib/ui/feedback/toasts.svelte';

const api = vi.hoisted(() => ({
	listSavedSearches: vi.fn(),
	getSavedSearch: vi.fn(),
	listAlerts: vi.fn(),
	markAlertsRead: vi.fn(),
	setPaused: vi.fn(),
	deleteSavedSearch: vi.fn(),
	createSavedSearch: vi.fn()
}));
vi.mock('./api', async (orig) => ({ ...(await orig<typeof import('./api')>()), ...api }));

const nav = vi.hoisted(() => ({ goto: vi.fn() }));
vi.mock('$app/navigation', () => nav);

// The event client, replaced by one that hands the test its handlers.
const bus = vi.hoisted(() => ({
	handler: undefined as undefined | ((data: unknown) => void),
	onResync: undefined as undefined | (() => void),
	off: vi.fn(),
	topics: [] as string[]
}));
vi.mock('$lib/state/events.svelte', () => ({
	events: {
		on: (topic: string, handler: (data: unknown) => void, opts: { onResync?: () => void } = {}) => {
			bus.topics.push(topic);
			bus.handler = handler;
			bus.onResync = opts.onResync;
			return bus.off;
		}
	}
}));

import AlertDetailPage from './AlertDetailPage.svelte';
import AlertHitRow from './AlertHitRow.svelte';
import AlertsListPage from './AlertsListPage.svelte';
import ChannelBadges from './ChannelBadges.svelte';
import DeleteAlertDialog from './DeleteAlertDialog.svelte';
import PauseToggle from './PauseToggle.svelte';
import SaveSearchDialog from './SaveSearchDialog.svelte';
import { hit, row, saved } from './fixtures';

// Overlay panels are positioned by Floating UI, which jsdom can't do.
configure({ defaultHidden: true });
beforeAll(() => stubLayout(600, 1000));

let narrow = false;
beforeEach(() => {
	vi.resetAllMocks();
	bus.handler = bus.onResync = undefined;
	bus.topics = [];
	toasts.items = [];
	narrow = false;
	vi.stubGlobal('matchMedia', (query: string) => ({
		matches: !narrow,
		media: query,
		addEventListener: () => {},
		removeEventListener: () => {}
	}));
	api.markAlertsRead.mockResolvedValue({ ok: true });
});
afterEach(() => vi.unstubAllGlobals());

describe('ChannelBadges', () => {
	it('names the channels the API has, and "None" when both are off', () => {
		const { rerender } = render(ChannelBadges, { search: { alert: true, rss: true } });
		expect(screen.getByText('In the app')).toBeTruthy();
		expect(screen.getByText('RSS')).toBeTruthy();
		rerender({ search: { alert: false, rss: false } });
		expect(screen.getByText('None')).toBeTruthy();
	});
});

describe('PauseToggle', () => {
	it('names the search, sends once while pending, and shows the server’s value', async () => {
		let resolve!: (s: ReturnType<typeof saved>) => void;
		api.setPaused.mockReturnValue(new Promise((r) => (resolve = r)));
		const onUpdated = vi.fn();
		render(PauseToggle, { search: saved(), onUpdated });
		const toggle = screen.getByRole('switch', { name: 'Paused: Lantern Keeper releases' });
		await fireEvent.click(toggle);
		expect(toggle.getAttribute('aria-busy')).toBe('true');
		await fireEvent.click(toggle); // a second flip while waiting
		expect(api.setPaused).toHaveBeenCalledTimes(1);
		expect(api.setPaused).toHaveBeenCalledWith(1, true);
		resolve(saved({ paused: true }));
		await waitFor(() => expect(onUpdated).toHaveBeenCalledWith(saved({ paused: true })));
		expect(toggle.getAttribute('aria-busy')).toBeNull();
	});
	it('shows the resume rule once after resuming', async () => {
		api.setPaused.mockResolvedValue(saved({ paused: false }));
		// The parent applies the answer, as both pages do.
		const view = render(PauseToggle, {
			search: saved({ paused: true }),
			onUpdated: (updated: ReturnType<typeof saved>) => view.rerender({ search: updated })
		});
		const toggle = screen.getByRole('switch');
		expect((toggle as HTMLInputElement).checked).toBe(true);
		await fireEvent.click(toggle);
		await screen.findByText('Resuming skips torrents indexed while it was paused.');
	});
	it('goes back and toasts when the server refuses', async () => {
		api.setPaused.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		render(PauseToggle, { search: saved(), onUpdated: vi.fn() });
		const toggle = screen.getByRole('switch') as HTMLInputElement;
		await fireEvent.click(toggle);
		await waitFor(() => expect(toggle.checked).toBe(false));
		expect(toasts.items[0]?.kind).toBe('error');
	});
});

describe('AlertHitRow', () => {
	it('shows the facts with the server’s values', () => {
		render(AlertHitRow, { row: row(7, false, { seeders: 12, leechers: 3 }) });
		expect(screen.getByTestId('alert-title-7').textContent).toBe('Lantern Keeper 7');
		expect(screen.getByTestId('alert-raw-7').textContent).toBe('Lantern.Keeper.7');
		expect(screen.getByTestId('alert-size-7').textContent).toBe('2.00 GiB');
		expect(screen.getByTestId('alert-files-7').textContent?.trim()).toBe('5 files');
		expect(screen.getByTestId('alert-swarm-7').textContent?.trim()).toBe('12 seeders, 3 leechers');
		expect(screen.getByTestId('alert-trackers-7').textContent).toContain('4 / 20');
		expect(screen.getByTestId('alert-comp-7-dominant').textContent).toBe('82% video');
		expect(screen.getByRole('img', { name: /82% video, 11% audio/ })).toBeTruthy();
	});
	it('says so for null values: no title, never scraped, not classified', () => {
		render(AlertHitRow, {
			row: row(8, true, {
				title: null,
				composition: [],
				seeders: null,
				leechers: null,
				trackers_checked: null,
				trackers_reporting: null,
				files: 1
			})
		});
		expect(screen.getByTestId('alert-title-8').textContent).toBe('Lantern.Keeper.8');
		expect(screen.queryByTestId('alert-raw-8')).toBeNull();
		expect(screen.getByTestId('alert-comp-8').textContent).toBe('Not classified yet');
		expect(screen.getByTestId('alert-swarm-8').textContent).toContain('Never scraped');
		expect(screen.getByTestId('alert-trackers-8').textContent).toContain('Never scraped');
		expect(screen.getByTestId('alert-files-8').textContent?.trim()).toBe('1 file');
	});
});

describe('the list page', () => {
	const three = [
		saved({
			id: 1,
			name: 'Lantern Keeper releases',
			unread: 2,
			alerts: 5,
			last_alert_at: 1_700_000_300
		}),
		saved({ id: 2, name: 'New FLAC albums', q: 'flac', rss: true }),
		saved({ id: 3, name: 'Films to check', q: '', paused: true, alert: false })
	];

	it('has its heading before any data, then one row per search in the server’s order', async () => {
		api.listSavedSearches.mockResolvedValue(three);
		render(AlertsListPage);
		expect(screen.getByRole('heading', { level: 1, name: 'Alerts' })).toBeTruthy();
		await screen.findByTestId('alerts-row-1');
		const rows = screen.getAllByTestId(/^alerts-row-/);
		expect(rows.map((r) => r.getAttribute('data-testid'))).toEqual([
			'alerts-row-1',
			'alerts-row-2',
			'alerts-row-3'
		]);
		expect(screen.getByRole('table')).toBeTruthy();
		expect(screen.getByRole('rowheader', { name: /Lantern Keeper releases/ })).toBeTruthy();
	});
	it('shows every column and null state', async () => {
		api.listSavedSearches.mockResolvedValue(three);
		render(AlertsListPage);
		await screen.findByTestId('alerts-row-1');
		expect(screen.getByTestId('alerts-unread-1').textContent).toBe('2');
		expect(screen.getByTestId('alerts-unread-2').textContent).toBe('0');
		expect(screen.getByTestId('alerts-last-2').textContent).toContain('No alerts yet');
		expect(screen.getByTestId('alerts-last-1').textContent).not.toContain('No alerts yet');
		expect(within(screen.getByTestId('alerts-row-3')).getByText('Every new torrent')).toBeTruthy();
		expect(within(screen.getByTestId('alerts-row-3')).getByText('None')).toBeTruthy();
		expect(screen.getByTestId('alerts-paused-3').textContent).toBe('Paused');
		// Status is icon + text: the badge holds an svg.
		expect(screen.getByTestId('alerts-paused-3').parentElement?.querySelector('svg')).toBeTruthy();
		expect(screen.queryByTestId('alerts-paused-1')).toBeNull();
		expect(within(screen.getByTestId('alerts-row-2')).getByText('RSS')).toBeTruthy();
		expect(screen.getByRole('link', { name: 'Lantern Keeper releases' }).getAttribute('href')).toBe(
			'/alerts/1'
		);
		expect(screen.getByRole('button', { name: 'Delete alert “Films to check”' })).toBeTruthy();
	});
	it('is a stacked list on a narrow screen, never both', async () => {
		narrow = true;
		api.listSavedSearches.mockResolvedValue(three);
		render(AlertsListPage);
		await screen.findByTestId('alerts-row-1');
		expect(screen.queryByRole('table')).toBeNull();
		expect(screen.getByTestId('alerts-stack')).toBeTruthy();
	});
	it('offers one next step when there are no saved searches', async () => {
		api.listSavedSearches.mockResolvedValue([]);
		render(AlertsListPage);
		await screen.findByRole('heading', { level: 2, name: 'No saved searches yet' });
		expect(screen.getAllByRole('button', { name: 'New alert' })).toHaveLength(2);
	});
	it('keeps the rows and shows a Retry when a refresh fails', async () => {
		api.listSavedSearches.mockResolvedValueOnce(three);
		render(AlertsListPage);
		await screen.findByTestId('alerts-row-1');
		api.listSavedSearches.mockRejectedValueOnce(new ApiError(0, 'network', 'down'));
		bus.onResync?.();
		await screen.findByRole('alert');
		expect(screen.getByTestId('alerts-row-1')).toBeTruthy();
		api.listSavedSearches.mockResolvedValueOnce(three);
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
	});
	it('subscribes to alerts events, refetches once for several in a frame, and refetches on resync', async () => {
		api.listSavedSearches.mockResolvedValue(three);
		render(AlertsListPage);
		await screen.findByTestId('alerts-row-1');
		expect(bus.topics).toEqual(['alerts']);
		api.listSavedSearches.mockClear();
		api.listSavedSearches.mockResolvedValue([{ ...three[0]!, unread: 9 }, three[1]!, three[2]!]);
		for (const id of [1, 2, 2]) bus.handler?.({ saved_search: id });
		await waitFor(() => expect(screen.getByTestId('alerts-unread-1').textContent).toBe('9'));
		expect(api.listSavedSearches).toHaveBeenCalledTimes(1);
		bus.onResync?.();
		await waitFor(() => expect(api.listSavedSearches).toHaveBeenCalledTimes(2));
	});
	it('unsubscribes when the page goes', async () => {
		api.listSavedSearches.mockResolvedValue(three);
		const { unmount } = render(AlertsListPage);
		await screen.findByTestId('alerts-row-1');
		unmount();
		expect(bus.off).toHaveBeenCalled();
	});
	it('registers "New alert…" in the palette while it is open, and removes it after', async () => {
		api.listSavedSearches.mockResolvedValue(three);
		const { unmount } = render(AlertsListPage);
		await screen.findByTestId('alerts-row-1');
		const info = () => keys.list().find((c) => c.id === 'alerts.new');
		expect(info()).toMatchObject({ title: 'New alert…', scope: 'alerts', palette: true, keys: [] });
		unmount();
		expect(info()).toBeUndefined();
	});
	it('moves focus to the next row’s name after a delete, else the previous, else the heading', async () => {
		api.listSavedSearches.mockResolvedValue(three);
		api.deleteSavedSearch.mockResolvedValue({ ok: true });
		render(AlertsListPage);
		await screen.findByTestId('alerts-row-1');
		api.listSavedSearches.mockResolvedValue([three[0]!, three[2]!]);
		await fireEvent.click(screen.getByTestId('alerts-delete-2'));
		const dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Delete' }));
		await waitFor(() => expect(screen.queryByTestId('alerts-row-2')).toBeNull());
		await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId('alerts-name-3')));
	});
});

describe('SaveSearchDialog', () => {
	const open = async (props: Record<string, unknown> = {}) => {
		render(SaveSearchDialog, { open: true, ...props });
		return screen.findByRole('dialog', { name: 'Save search' });
	};
	const fill = async (dialog: HTMLElement, name: string, q = '') => {
		await fireEvent.input(within(dialog).getByLabelText(/^Name/), { target: { value: name } });
		if (q) await fireEvent.input(within(dialog).getByLabelText('Query'), { target: { value: q } });
	};
	const submit = (dialog: HTMLElement) =>
		fireEvent.click(within(dialog).getByRole('button', { name: 'Save search' }));

	it('has visible labels, the defaults and the help text', async () => {
		const dialog = await open({ q: 'ubuntu' });
		expect(within(dialog).getByLabelText(/^Name/)).toBeTruthy();
		expect((within(dialog).getByLabelText('Query') as HTMLInputElement).value).toBe('ubuntu');
		expect(within(dialog).getByText('Alerts cover torrents indexed from now on')).toBeTruthy();
		expect((within(dialog).getByLabelText('In the app') as HTMLInputElement).checked).toBe(true);
		expect((within(dialog).getByLabelText('RSS feed') as HTMLInputElement).checked).toBe(false);
	});
	it('sends the form, passes sort through, toasts, and opens the new alert', async () => {
		api.createSavedSearch.mockResolvedValue(saved({ id: 9, name: 'Nature' }));
		const dialog = await open({ sort: '-discovered' });
		await fill(dialog, '  Nature  ', 'nature 4k');
		await fireEvent.click(within(dialog).getByLabelText('RSS feed'));
		await submit(dialog);
		await waitFor(() => expect(nav.goto).toHaveBeenCalledWith('/alerts/9'));
		expect(api.createSavedSearch).toHaveBeenCalledWith({
			name: 'Nature',
			q: 'nature 4k',
			sort: '-discovered',
			alert: true,
			rss: true
		});
		expect(toasts.items[0]?.text).toBe('Saved “Nature”');
	});
	it('lets onSaved decide where to go', async () => {
		const created = saved({ id: 9, name: 'Nature' });
		api.createSavedSearch.mockResolvedValue(created);
		const onSaved = vi.fn();
		const dialog = await open({ onSaved });
		await fill(dialog, 'Nature');
		await submit(dialog);
		await waitFor(() => expect(onSaved).toHaveBeenCalledWith(created));
		expect(nav.goto).not.toHaveBeenCalled();
		// Nothing typed, nothing given: neither is sent.
		expect(api.createSavedSearch.mock.calls[0]![0].q).toBeUndefined();
		expect(api.createSavedSearch.mock.calls[0]![0].sort).toBeUndefined();
	});
	it('needs a name and says so without sending', async () => {
		const dialog = await open();
		await submit(dialog);
		expect(within(dialog).getByTestId('field-error').textContent).toContain('1 to 200 characters');
		expect(api.createSavedSearch).not.toHaveBeenCalled();
	});
	it('puts a syntax error on the query with the parser’s message and selects the span', async () => {
		api.createSavedSearch.mockRejectedValue(
			new ApiError(400, 'query_syntax', 'Unmatched ")".', { start: 6, end: 7 })
		);
		const dialog = await open();
		await fill(dialog, 'Broken', 'lantern) x');
		await submit(dialog);
		await waitFor(() =>
			expect(within(dialog).getByTestId('field-error').textContent).toContain('Unmatched ")".')
		);
		const query = within(dialog).getByLabelText('Query') as HTMLInputElement;
		expect(query.getAttribute('aria-invalid')).toBe('true');
		await waitFor(() => expect([query.selectionStart, query.selectionEnd]).toEqual([6, 7]));
		expect(dialog.isConnected).toBe(true);
	});
	it('puts invalid_value on the field the server names', async () => {
		api.createSavedSearch.mockRejectedValue(
			new ApiError(422, 'invalid_value', 'bad', { field: 'name' })
		);
		const dialog = await open();
		await fill(dialog, 'x');
		await submit(dialog);
		await waitFor(() =>
			expect(within(dialog).getByLabelText(/^Name/).getAttribute('aria-invalid')).toBe('true')
		);
	});
	it('shows too_many_saved_searches as a form-level message and keeps the values', async () => {
		api.createSavedSearch.mockRejectedValue(new ApiError(409, 'too_many_saved_searches', 'x'));
		const dialog = await open();
		await fill(dialog, 'One more', 'abc');
		await submit(dialog);
		await within(dialog).findByText(/You have 1,000 saved searches/);
		expect((within(dialog).getByLabelText(/^Name/) as HTMLInputElement).value).toBe('One more');
		expect((within(dialog).getByLabelText('Query') as HTMLInputElement).value).toBe('abc');
	});
	it('keeps the dialog open with a form error on a network failure or a 5xx', async () => {
		for (const err of [new ApiError(0, 'network', 'down'), new ApiError(503, 'unavailable', 'x')]) {
			api.createSavedSearch.mockRejectedValueOnce(err);
			const dialog = await open();
			await fill(dialog, 'Keep me');
			await submit(dialog);
			await within(dialog).findByRole('alert');
			expect((within(dialog).getByLabelText(/^Name/) as HTMLInputElement).value).toBe('Keep me');
			document.body.innerHTML = '';
		}
	});
	it('maps the codes that mean a bug to a generic form error', async () => {
		for (const code of ['q_and_ast', 'invalid_ast', 'unknown_sort']) {
			api.createSavedSearch.mockRejectedValueOnce(new ApiError(400, code, 'x'));
			const dialog = await open();
			await fill(dialog, 'x');
			await submit(dialog);
			await within(dialog).findByRole('alert');
			document.body.innerHTML = '';
		}
	});
});

describe('DeleteAlertDialog', () => {
	const mount = async (rss: boolean, onDeleted = vi.fn()) => {
		render(DeleteAlertDialog, {
			open: true,
			search: { id: 4, name: '4K nature docs', rss },
			onDeleted
		});
		return { dialog: await screen.findByRole('dialog'), onDeleted };
	};
	it('asks the specific question and names the RSS feed only when there is one', async () => {
		const a = await mount(true);
		expect(a.dialog.getAttribute('aria-labelledby')).toBeTruthy();
		expect(
			screen.getByRole('dialog', { name: "Delete '4K nature docs' and its history?" })
		).toBeTruthy();
		expect(screen.getByText(/its RSS feed stops working/)).toBeTruthy();
		document.body.innerHTML = '';
		await mount(false);
		expect(screen.queryByText(/RSS feed/)).toBeNull();
		expect(screen.getByText("Its alerts are deleted. This can't be undone.")).toBeTruthy();
	});
	it('deletes, then reports', async () => {
		api.deleteSavedSearch.mockResolvedValue({ ok: true });
		const { onDeleted } = await mount(false);
		await fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
		await waitFor(() =>
			expect(onDeleted).toHaveBeenCalledWith({ id: 4, name: '4K nature docs', rss: false })
		);
		expect(api.deleteSavedSearch).toHaveBeenCalledWith(4);
	});
	it('counts a 404 as already deleted', async () => {
		api.deleteSavedSearch.mockRejectedValue(new ApiError(404, 'saved_search_not_found', 'x'));
		const { onDeleted } = await mount(false);
		await fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
		await waitFor(() => expect(onDeleted).toHaveBeenCalled());
	});
	it('stays open with an error on any other failure', async () => {
		api.deleteSavedSearch.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		const { onDeleted } = await mount(false);
		await fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
		await screen.findByRole('alert');
		expect(onDeleted).not.toHaveBeenCalled();
	});
});

describe('the detail page', () => {
	const rows = [row(5, false), row(4, false), row(3, true), row(2, true)];
	const load = (search = saved({ unread: 2 }), items = rows) => {
		api.getSavedSearch.mockResolvedValue(search);
		api.listAlerts.mockResolvedValue({ items, next_cursor: null });
	};

	it('shows the header, the rows and the mark labelled by the first read row', async () => {
		load(saved({ unread: 2, rss: true, q: 'lantern keeper', sort: 'size' }));
		render(AlertDetailPage, { id: 1 });
		await screen.findByTestId('alert-row-5');
		expect(screen.getByRole('heading', { level: 1, name: 'Lantern Keeper releases' })).toBeTruthy();
		expect(screen.getByTestId('alert-opened-unread').textContent).toBe('2 new');
		expect(screen.getByTestId('alert-open-search').getAttribute('href')).toBe(
			'/search?q=lantern+keeper&sort=size'
		);
		expect((screen.getByTestId('alert-feed') as HTMLInputElement).value).toBe('/api/v1/feeds/1');
		expect(screen.getByRole('link', { name: 'Settings → Security' }).getAttribute('href')).toBe(
			'/settings/security'
		);
		expect(screen.getByRole('heading', { level: 2, name: /^New since / })).toBeTruthy();
		await waitFor(() => expect(api.markAlertsRead).toHaveBeenCalledWith(1, 5));
	});
	it('has no mark when nothing is unread, and sends no read mark', async () => {
		load(saved(), [row(3, true), row(2, true)]);
		render(AlertDetailPage, { id: 1 });
		await screen.findByTestId('alert-row-3');
		expect(screen.queryByTestId('alert-divider')).toBeNull();
		expect(api.markAlertsRead).not.toHaveBeenCalled();
	});
	it('says what to expect when there are no alerts, and when paused', async () => {
		load(saved(), []);
		const { unmount } = render(AlertDetailPage, { id: 1 });
		expect((await screen.findByTestId('alert-empty')).textContent).toBe(
			'No matches yet. New torrents that match appear here.'
		);
		unmount();
		load(saved({ paused: true }), []);
		render(AlertDetailPage, { id: 1 });
		await waitFor(() =>
			expect(screen.getByTestId('alert-empty').textContent).toBe(
				'Paused: no new alerts are recorded.'
			)
		);
	});
	it('says the alert is gone, with a way back', async () => {
		api.getSavedSearch.mockRejectedValue(new ApiError(404, 'saved_search_not_found', 'x'));
		api.listAlerts.mockRejectedValue(new ApiError(404, 'saved_search_not_found', 'x'));
		render(AlertDetailPage, { id: 7 });
		await screen.findByRole('heading', { level: 1, name: "This alert doesn't exist any more" });
		expect(screen.getByRole('link', { name: 'Back to Alerts' }).getAttribute('href')).toBe(
			'/alerts'
		);
	});
	it('shows an error with Retry when it cannot load, then recovers', async () => {
		api.getSavedSearch.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		api.listAlerts.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		render(AlertDetailPage, { id: 1 });
		await screen.findByRole('alert');
		load();
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		await screen.findByTestId('alert-row-5');
	});

	describe('live updates', () => {
		beforeEach(() => vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] }));
		afterEach(() => vi.useRealTimers());

		const settle = async () => {
			await vi.advanceTimersByTimeAsync(0);
			await vi.advanceTimersByTimeAsync(0);
		};

		it('raises a banner for this search only, from the server’s count, and leaves the list alone', async () => {
			load();
			render(AlertDetailPage, { id: 1 });
			await settle();
			expect(bus.topics).toEqual(['alerts']);
			bus.handler?.({ saved_search: 2 });
			await settle();
			expect(api.getSavedSearch).toHaveBeenCalledTimes(1);
			expect(screen.queryByTestId('alert-banner')).toBeNull();

			api.getSavedSearch.mockResolvedValue(saved({ unread: 3 }));
			bus.handler?.({ saved_search: 1, matched: 99 });
			await settle();
			expect(screen.getByTestId('alert-banner').textContent).toBe('3 new alerts');
			expect(screen.queryByTestId('alert-row-6')).toBeNull();
			// The header still says how many were new when the page opened.
			expect(screen.getByTestId('alert-opened-unread').textContent).toBe('2 new');
		});
		it('refetches once for events in one frame', async () => {
			load();
			render(AlertDetailPage, { id: 1 });
			await settle();
			api.getSavedSearch.mockClear();
			api.getSavedSearch.mockResolvedValue(saved({ unread: 1 }));
			for (let i = 0; i < 3; i++) bus.handler?.({ saved_search: 1 });
			await settle();
			expect(api.getSavedSearch).toHaveBeenCalledTimes(1);
		});
		it('refetches on resync', async () => {
			load();
			render(AlertDetailPage, { id: 1 });
			await settle();
			api.getSavedSearch.mockClear();
			api.getSavedSearch.mockResolvedValue(saved({ unread: 1 }));
			bus.onResync?.();
			await settle();
			expect(api.getSavedSearch).toHaveBeenCalledTimes(1);
			expect(screen.getByTestId('alert-banner').textContent).toBe('1 new alert');
		});
		it('Show reloads the first page, re-places the mark and marks read through the new top', async () => {
			load();
			render(AlertDetailPage, { id: 1 });
			await settle();
			api.getSavedSearch.mockResolvedValue(saved({ unread: 1 }));
			bus.handler?.({ saved_search: 1 });
			await settle();
			api.listAlerts.mockResolvedValue({
				items: [row(6, false), row(5, true), row(4, true)],
				next_cursor: null
			});
			await fireEvent.click(screen.getByTestId('alert-show'));
			await settle();
			expect(screen.getByTestId('alert-row-6')).toBeTruthy();
			expect(screen.queryByTestId('alert-banner')).toBeNull();
			expect(api.markAlertsRead).toHaveBeenLastCalledWith(1, 6);
			expect(api.markAlertsRead).toHaveBeenCalledTimes(2);
		});
		it('announces politely, at most once every 10 seconds', async () => {
			load();
			render(AlertDetailPage, { id: 1 });
			await settle();
			const live = screen.getByTestId('alert-live');
			expect(live.getAttribute('aria-live')).toBe('polite');
			const event = async (unread: number) => {
				api.getSavedSearch.mockResolvedValue(saved({ unread }));
				bus.handler?.({ saved_search: 1 });
				await settle();
			};
			await event(1);
			expect(live.textContent).toBe('1 new alert');
			await vi.advanceTimersByTimeAsync(3_000);
			await event(2);
			await event(3);
			expect(live.textContent).toBe('1 new alert'); // still inside the window
			await vi.advanceTimersByTimeAsync(7_000);
			expect(live.textContent).toBe('3 new alerts');
		});
		it('unsubscribes when the page goes', async () => {
			load();
			const { unmount } = render(AlertDetailPage, { id: 1 });
			await settle();
			unmount();
			expect(bus.off).toHaveBeenCalled();
		});
	});

	it('registers j and k for its rows while it is open, and removes them after', async () => {
		load();
		const { unmount } = render(AlertDetailPage, { id: 1 });
		await screen.findByTestId('alert-row-5');
		const ids = () => keys.list().filter((c) => c.scope === 'alerts' && c.id.startsWith('list.'));
		expect(ids().map((c) => [c.id, c.keys])).toEqual([
			['list.next', ['j']],
			['list.previous', ['k']]
		]);
		unmount();
		expect(ids()).toEqual([]);
	});
	it('links nothing from a hash that is not 40 hex digits', async () => {
		load(saved(), [{ ...row(3, true), torrent: hit(3, { hash: '../evil' }) }]);
		render(AlertDetailPage, { id: 1 });
		await screen.findByTestId('alert-row-3');
		await fireEvent.click(screen.getByTestId('alert-row-3'));
		expect(nav.goto).not.toHaveBeenCalled();
	});
	it('opens a row at /t/<hash>', async () => {
		load(saved(), [row(3, true)]);
		render(AlertDetailPage, { id: 1 });
		await screen.findByTestId('alert-row-3');
		await fireEvent.click(screen.getByTestId('alert-row-3'));
		expect(nav.goto).toHaveBeenCalledWith(`/t/${'3'.padStart(40, '0')}`);
	});
});
