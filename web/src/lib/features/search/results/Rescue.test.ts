// The zero-result rescue: asked once per query text, an answer for older text is dropped, and a
// correction the parser rejects is not offered.
import { render, screen, waitFor, within } from '@testing-library/svelte';
import { tick } from 'svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { setQ } from './Rescue.test-double.svelte';

vi.setConfig({ testTimeout: 20_000 });
const WAIT = { timeout: 10_000 };

vi.mock('$lib/state/search.svelte', async () => ({
	search: (await import('./Rescue.test-double.svelte')).rescueSearch
}));
vi.mock('../query/path', () => ({ ensureSearchPath: () => {} }));
const api = vi.hoisted(() => ({ get: vi.fn() }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api
}));

const { default: Rescue, resetRescue } = await import('./Rescue.svelte');

const field = { name: 'seeders', aliases: [], type: 'number', facetable: false, operators: ['gt'] };
const calls = (path: string) => api.get.mock.calls.filter((c) => c[0] === path);

beforeEach(() => {
	resetRescue();
	setQ('');
	api.get.mockReset();
	api.get.mockImplementation(async (path: string, opts: { query?: { q: string } }) => {
		if (path === '/search/fields') return { fields: [field] };
		return { ast: {}, canonical: opts.query?.q };
	});
});

describe('Rescue', () => {
	it('asks nothing again for the same text after a remount, and the fields once for any text', async () => {
		setQ('seeders>5');
		const first = render(Rescue);
		await screen.findByTestId('rescue-didyoumean', {}, WAIT);
		const asked = api.get.mock.calls.length;
		first.unmount();
		render(Rescue); // a retry or a facet change: the block mounts again for the same text
		await screen.findByTestId('rescue-didyoumean', {}, WAIT);
		expect(api.get.mock.calls).toHaveLength(asked);
		expect(calls('/search/fields')).toHaveLength(1);
	});

	it('settles with no offer when the server is slower than the limit', async () => {
		api.get.mockImplementation(
			(_path: string, opts: { signal?: AbortSignal }) =>
				new Promise((_resolve, reject) =>
					opts.signal?.addEventListener('abort', () => reject(new DOMException('', 'AbortError')))
				)
		);
		let settled = false;
		setQ('seeders>5');
		render(Rescue, {
			limit: 50,
			get settled() {
				return settled;
			},
			set settled(v: boolean) {
				settled = v;
			}
		});
		await waitFor(() => expect(settled).toBe(true), WAIT);
		expect(screen.queryByRole('button')).toBeNull();
	});

	it('offers the corrected clause once the parser accepts it, and asks nothing for an empty query', async () => {
		render(Rescue);
		await Promise.resolve();
		expect(api.get).not.toHaveBeenCalled();
		setQ('seeders>5');
		const offer = await screen.findByTestId('rescue-didyoumean', {}, WAIT);
		expect(offer.textContent).toContain('Did you mean seeders:>5?');
		expect(within(offer).getByRole('button', { name: 'Apply' })).toBeTruthy();
		const asked = calls('/parse').map((c) => JSON.stringify(c[1].query));
		expect(asked.sort()).toEqual(
			[
				JSON.stringify({ q: 'seeders:>5' }),
				JSON.stringify({ q: 'seeders>5', natural: true })
			].sort()
		);
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
		let releaseOld!: () => void;
		let releaseNew!: () => void;
		const heldOld = new Promise<void>((r) => (releaseOld = r));
		const heldNew = new Promise<void>((r) => (releaseNew = r));
		let oldReturned = false;
		api.get.mockImplementation(async (path: string, opts: { query?: { q: string } }) => {
			if (path === '/search/fields') return { fields: [field] };
			if (opts.query?.q === 'seeders:>5') {
				await heldOld; // the first text's check is slow
				oldReturned = true;
			}
			if (opts.query?.q === 'size>1gb') await heldNew; // so is the newer text's natural parse
			return { ast: {}, canonical: opts.query?.q };
		});
		let settled = false;
		render(Rescue, {
			get settled() {
				return settled;
			},
			set settled(v: boolean) {
				settled = v;
			}
		});
		setQ('seeders>5');
		await waitFor(() => expect(calls('/parse')).toHaveLength(2), WAIT); // held fix + natural
		setQ('size>1gb'); // newer text; its field is unknown, so no correction
		await waitFor(() => expect(calls('/parse')).toHaveLength(3), WAIT);
		releaseOld();
		await waitFor(() => expect(oldReturned).toBe(true), WAIT);
		await new Promise((r) => setTimeout(r, 0)); // a macrotask: every microtask of the old chain has run
		// The old text's answer arrived while the newer one is still out: it shows nothing and settles nothing.
		expect(screen.queryByRole('button')).toBeNull();
		expect(settled).toBe(false);
		releaseNew();
		await waitFor(() => expect(settled).toBe(true), WAIT);
		expect(screen.queryByRole('button')).toBeNull();
		expect(calls('/search/fields')).toHaveLength(1);
	});

	it('a new text aborting the old one does not poison the field list', async () => {
		let releaseFields!: () => void;
		const heldFields = new Promise<void>((r) => (releaseFields = r));
		api.get.mockImplementation(async (path: string, opts: { query?: { q: string } }) => {
			if (path === '/search/fields') {
				await heldFields;
				return { fields: [field] };
			}
			return { ast: {}, canonical: opts.query?.q };
		});
		render(Rescue);
		setQ('seeders>5');
		await waitFor(() => expect(calls('/search/fields')).toHaveLength(1), WAIT);
		setQ('seeders>6'); // aborts the first text while the field list is still on its way
		releaseFields();
		const offer = await screen.findByTestId('rescue-didyoumean', {}, WAIT);
		expect(offer.textContent).toContain('seeders:>6');
		expect(calls('/search/fields')).toHaveLength(1);
	});

	it('settles even when the answer throws', async () => {
		api.get.mockImplementation(async (path: string, opts: { query?: { q: string } }) => {
			if (path === '/search/fields') return { fields: [field] };
			return { ast: {}, canonical: opts.query?.q, natural: { chips: null } }; // chips.length throws
		});
		let settled = false;
		render(Rescue, {
			get settled() {
				return settled;
			},
			set settled(v: boolean) {
				settled = v;
			}
		});
		setQ('seeders>5');
		await waitFor(() => expect(settled).toBe(true), WAIT);
	});

	it('offers the corrected clause once the parser accepts it, and asks nothing for an empty query', async () => {
		render(Rescue);
		await Promise.resolve();
		expect(api.get).not.toHaveBeenCalled();
		setQ('seeders>5');
		const offer = await screen.findByTestId('rescue-didyoumean', {}, WAIT);
		expect(offer.textContent).toContain('Did you mean seeders:>5?');
		expect(within(offer).getByRole('button', { name: 'Apply' })).toBeTruthy();
		const asked = calls('/parse').map((c) => JSON.stringify(c[1].query));
		expect(asked.sort()).toEqual(
			[
				JSON.stringify({ q: 'seeders:>5' }),
				JSON.stringify({ q: 'seeders>5', natural: true })
			].sort()
		);
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
		await waitFor(() => expect(calls('/parse')).toHaveLength(2), WAIT); // held fix + natural
		setQ('size>1gb'); // newer text; its field is unknown, so no correction
		await waitFor(() => expect(calls('/parse')).toHaveLength(3), WAIT);
		release();
		await tick();
		await tick(); // the old text's answer has arrived
		expect(screen.queryByRole('button')).toBeNull();
		expect(calls('/search/fields')).toHaveLength(1);
	});
});
