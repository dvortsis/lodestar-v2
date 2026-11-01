// "Block this…" from a result's More menu and from the detail drawer's actions (S-filters-2c),
// over a scripted fetch: the safety rules of the entry points (only an added rule, Cancel sends
// nothing, a refused apply keeps the server's answer, nothing beyond what the view shows).
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { clearDetailCaches } from '$lib/features/detail/cache';
import { torrentDetail } from '$lib/features/detail/fixtures';
import DetailActions from '$lib/features/detail/DetailActions.svelte';
import { hit } from '$lib/features/search/results/fixtures';
import RowActions from '$lib/features/search/results/RowActions.svelte';
import { mockFetch } from './flows/testing/fetch';
import { HOSTILE, applied, finished, strengthening } from './flows/testing/fixtures';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });

const HASH = 'ab'.repeat(20);
const V2 = 'cd'.repeat(32);
let net: ReturnType<typeof mockFetch>;

beforeEach(() => {
	net = mockFetch();
	net.on('GET /search', { body: { total: 1, total_approx: false, hits: [] } });
	net.on('POST /filters/preview/jobs', { status: 202, body: { job: 1 } });
	net.on('GET /filters/preview/jobs/{id}', { body: finished(strengthening()) });
	net.on('POST /filters/preview/jobs/{id}/cancel', { body: { ok: true } });
	net.on('POST /filters/apply', { body: applied(strengthening(), { id: 9 }) });
});
// The flows and Bits UI transform slowly on a cold cache (CI): load them outside any test's timeout.
beforeAll(() => import('./flows-root'), 60_000);
vi.setConfig({ testTimeout: 30_000 });
afterEach(async () => {
	await vi.dynamicImportSettled();
	vi.unstubAllGlobals();
});

/** Each entry point: render it for a torrent with this name, and press "Block this…". */
const entries = {
	'a result': {
		mount: (name: string) =>
			render(RowActions, {
				hit: hit(1, { hash: HASH, hash_v2: V2, name, title: 'Parsed Title', files: 3 }),
				density: 'comfortable',
				testid: 'r'
			}),
		press: async () => {
			await fireEvent.click(screen.getByTestId('r-more'));
			await fireEvent.click(await screen.findByRole('menuitem', { name: 'Block this…' }));
		}
	},
	'the drawer': {
		mount: (name: string) =>
			render(DetailActions, { detail: torrentDetail({ hash: HASH, hash_v2: V2, name }) }),
		press: () => fireEvent.click(screen.getByTestId('detail-block'))
	}
};

const dialogState = () => screen.getByTestId('change-dialog').dataset.state;
const opened = () => screen.findByRole('dialog', { name: 'Block this torrent' });
const changes = () => net.sent.filter((r) => r.method !== 'GET');

describe.each(Object.entries(entries))('Block this… from %s', (_, entry) => {
	const open = async (name: string) => {
		entry.mount(name);
		await entry.press();
	};
	it('pre-fills only this hash, previews first and only ever adds a rule', async () => {
		await open('Lantern.Keeper.1');
		await opened();
		expect(screen.getByTestId('block-query').textContent).toBe(
			`hash:${HASH} | hash:${V2.slice(0, 40)}`
		);
		expect(changes()).toEqual([]);
		await fireEvent.click(screen.getByTestId('block-preview'));
		await waitFor(() => expect(dialogState()).toBe('review'));
		await fireEvent.click(screen.getByTestId('change-apply'));
		await waitFor(() => expect(dialogState()).toBe('done'));
		const rule = {
			op: 'add_rule',
			query: `hash:${HASH} | hash:${V2.slice(0, 40)}`,
			action: 'hide'
		};
		expect(changes().map((r) => [r.method, r.path])).toEqual([
			['POST', '/filters/preview/jobs'],
			['POST', '/filters/apply']
		]);
		for (const r of changes()) expect(JSON.parse(r.body!).change).toEqual(rule);
		expect(JSON.parse(changes()[1]!.body!)).not.toHaveProperty('password');
	});

	it('the phrase offers only the words of the name the view shows, as text', async () => {
		await open(`Lantern.Keeper ${HOSTILE}`);
		await opened();
		await fireEvent.click(screen.getByRole('radio', { name: 'A phrase from the name' }));
		const words = screen.getAllByTestId('block-word').map((w) => w.textContent?.trim());
		expect(words.slice(0, 2)).toEqual(['Lantern', 'Keeper']);
		const dialog = screen.getByRole('dialog');
		expect(dialog.textContent).not.toContain('Parsed Title');
		expect(dialog.querySelector('img, script')).toBeNull();
	});

	it('Cancel before or after the preview sends no change', async () => {
		await open('Lantern.Keeper.1');
		await opened();
		await fireEvent.click(screen.getByTestId('change-cancel'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(changes()).toEqual([]);

		await entry.press();
		await opened();
		await fireEvent.click(screen.getByTestId('block-preview'));
		await waitFor(() => expect(dialogState()).toBe('review'));
		await fireEvent.click(screen.getByTestId('change-cancel'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(net.to('/filters/apply')).toEqual([]);
	});

	it('a refused apply shows the server’s message and sends nothing more', async () => {
		net.error('POST /filters/apply', 500, 'internal', { message: 'The rule store is locked.' });
		await open('Lantern.Keeper.1');
		await opened();
		await fireEvent.click(screen.getByTestId('block-preview'));
		await waitFor(() => expect(dialogState()).toBe('review'));
		await fireEvent.click(screen.getByTestId('change-apply'));
		await waitFor(() => expect(dialogState()).toBe('failed'));
		expect(screen.getByRole('dialog').textContent).toContain('The rule store is locked.');
		expect(net.to('/filters/apply')).toHaveLength(1);
	});
});

it('the drawer drops a press whose torrent changed before the flows loaded', async () => {
	const first = torrentDetail({ hash: HASH, name: 'First' });
	const view = render(DetailActions, { detail: first });
	await fireEvent.click(screen.getByTestId('detail-block'));
	await view.rerender({ detail: torrentDetail({ hash: 'ef'.repeat(20), name: 'Second' }) });
	await vi.dynamicImportSettled();
	expect(screen.queryByRole('dialog')).toBeNull();
	// Back to the first torrent (A → B → A): the dropped press must not open its dialog now.
	await view.rerender({ detail: first });
	await vi.dynamicImportSettled();
	expect(screen.queryByRole('dialog')).toBeNull();
	// No filters request of any kind, and no change at all; the header's one GET reads the client.
	expect(net.sent.filter((r) => r.path.startsWith('/filters'))).toEqual([]);
	expect(changes()).toEqual([]);
});

it('the drawer closes the dialog when its torrent stops being the one shown', async () => {
	const detail = torrentDetail({ hash: HASH, name: 'Lantern.Keeper.1' });
	const view = render(DetailActions, { detail });
	await fireEvent.click(screen.getByTestId('detail-block'));
	await opened();
	await view.rerender({ detail: null });
	await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
	await view.rerender({ detail });
	await fireEvent.click(screen.getByTestId('detail-block'));
	await opened();
	clearDetailCaches(false); // visibility changed, e.g. "Show adult content" turned off
	await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
	expect(changes()).toEqual([]);
});
