// The zero-result rescue: asked once per query text, an answer for older text is dropped, and a
// correction the parser rejects is not offered.
import { render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { setQ } from './rescue.svelte';

vi.setConfig({ testTimeout: 20_000 });
const WAIT = { timeout: 10_000 };

vi.mock('$lib/state/search.svelte', async () => ({
	search: (await import('./rescue.svelte')).rescueSearch
}));
vi.mock('../query/path', () => ({ ensureSearchPath: () => {} }));
const api = vi.hoisted(() => ({ get: vi.fn() }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api
}));

const { default: Rescue } = await import('./Rescue.svelte');

const field = { name: 'seeders', aliases: [], type: 'number', facetable: false, operators: ['gt'] };
const calls = (path: string) => api.get.mock.calls.filter((c) => c[0] === path);

beforeEach(() => {
	setQ('');
	api.get.mockReset();
	api.get.mockImplementation(async (path: string, opts: { query?: { q: string } }) => {
		if (path === '/search/fields') return { fields: [field] };
		return { ast: {}, canonical: opts.query?.q };
	});
});

describe('Rescue', () => {
	it('offers the corrected clause once the parser accepts it, and asks nothing for an empty query', async () => {
		render(Rescue);
		await Promise.resolve();
		expect(api.get).not.toHaveBeenCalled();
		setQ('seeders>5');
		await screen.findByRole('button', { name: 'Did you mean seeders:>5?' }, WAIT);
		expect(calls('/parse').map((c) => c[1].query)).toEqual([
			{ q: 'seeders:>5' },
			{ q: 'seeders>5', natural: true }
		]);
	});

	it('does not offer a correction the parser rejects', async () => {
		api.get.mockImplementation(async (path: string, opts: { query?: { q: string } }) => {
			if (path === '/search/fields') return { fields: [field] };
			if (opts.query?.q === 'seeders:>5') throw new Error('rejected');
			return { ast: {}, canonical: opts.query?.q };
		});
		render(Rescue);
		setQ('seeders>5');
		await waitFor(() => expect(calls('/parse')).toHaveLength(2), WAIT);
		expect(screen.queryByRole('button')).toBeNull();
	});

	it('drops an answer for older text, and asks the fields once', async () => {
		let release!: () => void;
		const held = new Promise<void>((r) => (release = r));
		api.get.mockImplementation(async (path: string, opts: { query?: { q: string } }) => {
			if (path === '/search/fields') return { fields: [field] };
			if (opts.query?.q === 'seeders:>5') await held; // the first text's check is slow
			return { ast: {}, canonical: opts.query?.q };
		});
		render(Rescue);
		setQ('seeders>5');
		await waitFor(() => expect(calls('/parse')).toHaveLength(1), WAIT);
		setQ('size>1gb'); // newer text; its field is unknown, so no correction
		await waitFor(() => expect(calls('/parse').length).toBeGreaterThan(1), WAIT);
		release();
		await waitFor(() => expect(calls('/parse')).toHaveLength(3), WAIT); // the old text ran on
		expect(screen.queryByRole('button')).toBeNull();
		expect(calls('/search/fields')).toHaveLength(1);
	});
});
