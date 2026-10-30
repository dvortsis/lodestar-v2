import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';
import { HASH, searchHit, torrentDetail } from './fixtures';
vi.mock('./visibility', () => ({ watchDetailVisibility: vi.fn() }));

vi.mock('$app/paths', () => ({
	resolve: (route: string, params?: Record<string, string>) =>
		route.replace('[hash]', params?.hash ?? '')
}));
const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));

const { default: DetailView } = await import('./DetailView.svelte');
const { DetailModel } = await import('./model.svelte');
const { clearDetailCaches, cacheHit } = await import('./cache');

const model = (state: Partial<InstanceType<typeof DetailModel>>) => {
	const m = new DetailModel();
	Object.assign(m, state);
	return m;
};
// Let queued promise callbacks run (microtasks only; no timers).
const flush = async () => {
	for (let i = 0; i < 20; i++) await Promise.resolve();
};

beforeEach(() => {
	get.mockReset();
	clearDetailCaches();
});

describe('DetailView states', () => {
	it('torrent_not_found: the index message, with Back to Search', async () => {
		render(DetailView, {
			model: model({ error: new ApiError(404, 'torrent_not_found', 'x'), status: 'error' }),
			mode: 'page'
		});
		const box = await screen.findByTestId('detail-not-found');
		expect(box.textContent).toContain("This torrent isn't in the index");
		expect(within(box).getByRole('link', { name: 'Back to Search' }).getAttribute('href')).toBe(
			'/search'
		);
	});

	it('bad_hash reads the same', async () => {
		render(DetailView, {
			model: model({ error: new ApiError(400, 'bad_hash', 'x'), status: 'error' }),
			mode: 'page'
		});
		expect(await screen.findByTestId('detail-not-found')).toBeTruthy();
	});

	it('adult_hidden: says why and links to appearance settings (provisional)', async () => {
		render(DetailView, {
			model: model({ error: new ApiError(404, 'adult_hidden', 'x'), status: 'error' }),
			mode: 'page'
		});
		const box = await screen.findByTestId('detail-adult-hidden');
		expect(box.textContent).toContain('This torrent is Adult, and Show adult content is off');
		expect(
			within(box).getByRole('link', { name: 'Open appearance settings' }).getAttribute('href')
		).toBe('/settings/appearance');
	});

	it('in the drawer, Back to Search calls onBack instead of linking', async () => {
		const onBack = vi.fn();
		render(DetailView, {
			model: model({ error: new ApiError(404, 'torrent_not_found', 'x'), status: 'error' }),
			mode: 'drawer',
			onBack
		});
		const button = await screen.findByRole('button', { name: 'Back to Search' });
		button.click();
		expect(onBack).toHaveBeenCalled();
	});

	it('a network error shows a Retry banner without a cached header', async () => {
		const m = model({
			error: new ApiError(0, 'network', 'offline'),
			status: 'error',
			hash: HASH
		});
		get.mockRejectedValue(new ApiError(0, 'network', 'offline'));
		render(DetailView, { model: m, mode: 'drawer' });
		const alert = await screen.findByRole('alert');
		expect(alert.textContent).toContain("Couldn't load this torrent");
		expect(m.title).toBe('');
		get.mockResolvedValue(torrentDetail());
		within(alert).getByRole('button', { name: 'Retry' }).click();
		await waitFor(() => expect(m.status).toBe('ready'));
		await flush();
		expect(screen.queryByRole('alert')).toBeNull();
		expect(screen.getByTestId('detail-size')).toBeTruthy();
		m.destroy();
	});

	it('a 5xx is the same banner', async () => {
		render(DetailView, {
			model: model({ error: new ApiError(503, 'unavailable', 'x'), status: 'error' }),
			mode: 'page'
		});
		expect((await screen.findByRole('alert')).textContent).toContain("Couldn't load this torrent");
	});

	it('shows the resolved title context: raw name and "Resolved via" when they differ', async () => {
		const m = model({
			detail: torrentDetail({ name: 'a8f3x.zip.part' }),
			hit: searchHit({ name: 'a8f3x.zip.part', title: 'Show', resolved_via: 'files' }),
			status: 'ready'
		});
		render(DetailView, { model: m, mode: 'drawer' });
		expect(screen.getByTestId('detail-raw-name').textContent).toBe('a8f3x.zip.part');
		expect(screen.getByTestId('detail-resolved-via').textContent).toBe('Resolved via files');
	});

	it('hides the raw name when it equals the title', () => {
		const m = model({
			detail: torrentDetail({ name: 'Show' }),
			hit: searchHit({ title: 'Show', name: 'Show' }),
			status: 'ready'
		});
		render(DetailView, { model: m, mode: 'drawer' });
		expect(screen.queryByTestId('detail-raw-name')).toBeNull();
	});

	it('links Open magnet only for a magnet: URL and Open full page only in the drawer', () => {
		const ok = model({ detail: torrentDetail(), hit: null, status: 'ready' });
		const { unmount } = render(DetailView, { model: ok, mode: 'drawer' });
		expect(screen.getByTestId('detail-open-magnet').getAttribute('href')).toMatch(/^magnet:\?/);
		expect(screen.getByTestId('detail-open-full').getAttribute('href')).toBe('/t/' + HASH);
		unmount();
		render(DetailView, { model: ok, mode: 'page' });
		expect(screen.queryByTestId('detail-open-full')).toBeNull();
		document.body.innerHTML = '';
		const bad = model({
			detail: torrentDetail({ magnet: 'javascript:alert(1)' }),
			hit: null,
			status: 'ready'
		});
		render(DetailView, { model: bad, mode: 'drawer' });
		expect(screen.queryByTestId('detail-open-magnet')).toBeNull();
	});
});

describe('DetailModel swaps', () => {
	it('drops the earlier torrent slow response when the next one is cached', async () => {
		const A = 'a'.repeat(40);
		const B = 'b'.repeat(40);
		const { cacheDetail } = await import('./cache');
		let finishA!: () => void;
		get.mockImplementation((path: string, options: { path?: { hash: string } }) =>
			path === '/torrents/{hash}'
				? options.path?.hash === B
					? Promise.resolve(torrentDetail({ hash: B, name: 'B' }))
					: new Promise((resolve) => {
							finishA = () => resolve(torrentDetail({ hash: A, name: 'A' }));
						})
				: Promise.resolve({ hits: [] })
		);
		const m = new DetailModel();
		m.load(A);
		cacheDetail(B, torrentDetail({ hash: B, name: 'B' }));
		m.load(B);
		expect(m.detail).toBeNull();
		finishA();
		await flush();
		expect(m.detail?.name).toBe('B');
		m.destroy();
	});
});

describe('DetailModel', () => {
	it('uses the cached hit and loads detail; a hash lookup fills the hit on a direct load', async () => {
		get.mockImplementation(async (path: string) =>
			path === '/torrents/{hash}' ? torrentDetail() : { hits: [searchHit()] }
		);
		const m = new DetailModel();
		m.load(HASH);
		expect(m.status).toBe('loading');
		await waitFor(() => expect(m.status).toBe('ready'));
		await waitFor(() => expect(m.hit?.title).toBe('Show'));
		const search = get.mock.calls.find((c) => c[0] === '/search')!;
		expect(search[1].query).toEqual({ q: `hash:${HASH}`, limit: 1, hidden: true });
		m.destroy();
	});

	it('ignores a lookup hit for another hash', async () => {
		get.mockImplementation(async (path: string) =>
			path === '/torrents/{hash}'
				? torrentDetail()
				: { hits: [searchHit({ hash: 'f'.repeat(40) })] }
		);
		const m = new DetailModel();
		m.load(HASH);
		await waitFor(() => expect(m.status).toBe('ready'));
		await flush();
		expect(m.hit).toBeNull();
		expect(m.title).toBe('Show.S01.1080p.WEB-DL-GRP');
		m.destroy();
	});

	it('does not look the hit up when Search handed it over', async () => {
		cacheHit(HASH, searchHit());
		get.mockResolvedValue(torrentDetail());
		const m = new DetailModel();
		m.load(HASH);
		expect(m.title).toBe('');
		await waitFor(() => expect(m.status).toBe('ready'));
		expect(m.title).toBe('Show');
		expect(get.mock.calls.map((c) => c[0])).toEqual(['/torrents/{hash}']);
		m.destroy();
	});

	it('a slow response for an earlier hash never overwrites the newer one', async () => {
		const A = 'a'.repeat(40);
		const B = 'b'.repeat(40);
		let finishA!: () => void;
		get.mockImplementation((path: string, o: { path?: { hash: string }; signal?: AbortSignal }) => {
			if (path !== '/torrents/{hash}') return Promise.resolve({ hits: [] });
			if (o.path!.hash === A) {
				return new Promise((resolve) => {
					finishA = () => resolve(torrentDetail({ hash: A, name: 'A' }));
				});
			}
			return Promise.resolve(torrentDetail({ hash: B, name: 'B' }));
		});
		const m = new DetailModel();
		m.load(A);
		m.load(B);
		await waitFor(() => expect(m.detail?.name).toBe('B'));
		finishA();
		await flush();
		expect(m.detail?.name).toBe('B');
		expect(m.hash).toBe(B);
		m.destroy();
	});
});
