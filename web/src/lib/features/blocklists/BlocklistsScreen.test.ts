import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { mockFetch } from '../filters/flows/testing/fetch';
import { applied, finished, strengthening, weakening } from '../filters/flows/testing/fixtures';
import BlocklistsScreen from './BlocklistsScreen.svelte';
import { catalog, connected, strictness, subscriptions } from './fixtures';
import { refusalText } from './lists';
import { ApiError } from '$lib/api/client';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
vi.setConfig({ testTimeout: 30_000 });

const SEVERE = strictness({
	level: 'severe',
	text: 'Severe: this list blocks about a third of the public internet.',
	ipv4_public_share: 0.31
});
let net: ReturnType<typeof mockFetch>;
function setup(subs = subscriptions()) {
	net = mockFetch();
	net.on('GET /filters/subscriptions', { body: subs });
	net.on('POST /filters/preview/jobs', { status: 202, body: { job: 7 } });
	net.on('POST /filters/preview/jobs/{id}/cancel', { body: { ok: true } });
	return render(BlocklistsScreen);
}
const wait = (check: () => void) => waitFor(check, { timeout: 10_000 });
const applyBody = () => JSON.parse(net.to('/filters/apply')[0]!.body!);
afterEach(() => vi.unstubAllGlobals());

describe('blocklist subscriptions', () => {
	it('shows the catalog, nothing connected and nothing sent by default', async () => {
		setup();
		await screen.findByTestId('catalog-bt_blocklists');
		expect(screen.getByTestId('blocklists-empty')).toBeTruthy();
		expect(
			screen.getByText(
				'Blocks about 31.0% of public IPv4 addresses · measured by Lodestar, not yet fetched'
			)
		).toBeTruthy();
		expect(net.sent.filter((s) => s.method !== 'GET')).toHaveLength(0);
	});

	it('connects a catalog list: previews first, shows the strictness, then applies without a password', async () => {
		setup();
		const a = strengthening({ strictness: SEVERE });
		net.on('GET /filters/preview/jobs/{id}', { body: finished(a) });
		net.on('POST /filters/apply', { body: applied(a, { id: 9 }) });
		await fireEvent.click(await screen.findByTestId('catalog-bt_blocklists-connect'));
		await wait(() => expect(screen.getByTestId('blocklists-level').textContent).toBe('Severe'));
		expect(screen.getByTestId('blocklists-strictness-text').textContent).toContain('third');
		expect(net.to('/filters/apply')).toHaveLength(0);
		net.on('GET /filters/subscriptions', {
			body: subscriptions({ catalog: [catalog({ subscription: 9 })] })
		});
		await fireEvent.click(await screen.findByTestId('change-apply'));
		await wait(() => expect(net.to('/filters/apply')).toHaveLength(1));
		expect(applyBody().change).toMatchObject({
			op: 'subscribe',
			url: 'https://lists.example.org/bt.p2p.gz'
		});
		expect(applyBody().password).toBeUndefined();
	});

	it('shows a held badge and keeps the previous version without a password', async () => {
		setup(subscriptions({ connected: [connected(4, {}, true)] }));
		await screen.findByTestId('list-4-held');
		const a = strengthening();
		net.on('GET /filters/preview/jobs/{id}', { body: finished(a) });
		net.on('POST /filters/apply', { body: applied(a) });
		await fireEvent.click(screen.getByTestId('list-4-keep'));
		await fireEvent.click(await screen.findByTestId('change-apply'));
		await wait(() => expect(net.to('/filters/apply')).toHaveLength(1));
		expect(applyBody()).toMatchObject({ change: { op: 'resolve_held', id: 4, accept: false } });
		expect(applyBody().password).toBeUndefined();
	});

	it('removing asks for the password and shows the exact counts from the preview', async () => {
		setup(subscriptions({ connected: [connected(4)] }));
		const a = weakening({
			warning: 'Removing Mine lets 3 held and 17 blocked torrents through.',
			preview: {
				quarantine: 3,
				tombstones: 17,
				stored: 0,
				removed_from_floor: 0,
				caught_30d: 0,
				core_caught_30d: 0,
				per_day: 0,
				estimate: false
			},
			strictness: strictness()
		});
		net.on('GET /filters/preview/jobs/{id}', { body: finished(a) });
		net.on('POST /filters/apply', { body: applied(a, { released: 20 }) });
		await fireEvent.click(await screen.findByTestId('list-4-remove'));
		await wait(() =>
			expect(screen.getByTestId('effect-warning-text').textContent).toContain('3 held and 17')
		);
		expect(net.to('/filters/apply')).toHaveLength(0);
		expect(screen.getByLabelText(/password/i)).toBeTruthy();
	});

	it('accepting a held version asks for the password and sends the previewed held_version', async () => {
		setup(subscriptions({ connected: [connected(4, {}, true)] }));
		const held = 'a'.repeat(64);
		const a = weakening({
			held_version: held,
			unblocked: 12,
			warning: 'Accepting lets 12 entries through.'
		});
		net.on('GET /filters/preview/jobs/{id}', { body: finished(a) });
		net.on('POST /filters/apply', { body: applied(a, { released: 9 }) });
		await fireEvent.click(await screen.findByTestId('list-4-accept'));
		const pw = await screen.findByTestId('change-password');
		expect(net.to('/filters/apply')).toHaveLength(0);
		await fireEvent.input(pw, { target: { value: 'hunter2' } });
		await fireEvent.click(await screen.findByTestId('change-apply'));
		await wait(() => expect(net.to('/filters/apply')).toHaveLength(1));
		expect(applyBody()).toMatchObject({
			change: { op: 'resolve_held', id: 4, accept: true, held },
			password: 'hunter2'
		});
		await wait(() => expect(screen.getByTestId('changed-released').textContent).toContain('9'));
	});

	it('pausing an enabled list asks for the password first', async () => {
		setup(subscriptions({ connected: [connected(4)] }));
		net.on('GET /filters/preview/jobs/{id}', { body: finished(weakening()) });
		await fireEvent.click(await screen.findByTestId('list-4-pause'));
		await screen.findByTestId('change-password');
		expect(net.to('/filters/apply')).toHaveLength(0);
	});

	it('shows the server\u2019s sentence when the address is refused, and applies nothing', async () => {
		setup();
		net.on('POST /filters/preview/jobs', {
			status: 400,
			body: {
				error: {
					code: 'list_url_refused',
					message: 'Lists are fetched from public addresses only.'
				}
			}
		});
		await fireEvent.click(await screen.findByTestId('catalog-bt_blocklists-connect'));
		await screen.findByText('Lists are fetched from public addresses only.');
		expect(net.to('/filters/apply')).toHaveLength(0);
	});

	it('shows a failed load with Try again', async () => {
		net = mockFetch();
		net.on('GET /filters/subscriptions', {
			status: 500,
			body: { error: { code: 'internal', message: 'x' } }
		});
		render(BlocklistsScreen);
		await screen.findByTestId('blocklists-failed');
		net.on('GET /filters/subscriptions', { body: subscriptions() });
		await fireEvent.click(screen.getByTestId('blocklists-retry'));
		await screen.findByTestId('catalog-bt_blocklists');
	});
});

describe('refusalText', () => {
	it.each([
		['list_url_refused', /public addresses/],
		['bad_list_url', /http/],
		['bad_list_name', /1 to 200/],
		['changed_since_preview', /changed/]
	])('%s', (code, re) => {
		expect(refusalText(new ApiError(400, code, 'server'))).toMatch(re);
	});
	it('falls back to the server sentence', () => {
		expect(refusalText(new ApiError(400, 'x', 'Because.'))).toBe('Because.');
	});
});
