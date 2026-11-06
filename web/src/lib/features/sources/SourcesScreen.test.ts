import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { mockFetch } from '../filters/flows/testing/fetch';
import SourcesScreen from './SourcesScreen.svelte';
import { dht, nyaa, nyaaAcked, prowlarr, source, sourceList } from './fixtures';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
vi.setConfig({ testTimeout: 30_000 });

let net: ReturnType<typeof mockFetch>;
function setup(list = sourceList(dht, nyaa, prowlarr)) {
	net = mockFetch();
	net.on('GET /sources', { body: list });
	return render(SourcesScreen);
}
const writes = () => net.sent.filter((s) => s.method !== 'GET');
afterEach(() => vi.unstubAllGlobals());

describe('Sources screen', () => {
	it('groups rows by kind and shows status as icon plus text with the 24 h yield', async () => {
		setup();
		await screen.findByTestId('source-0');
		expect(screen.getByTestId('sources-group-discovery').children).toHaveLength(2);
		expect(screen.getByTestId('sources-group-importer').children).toHaveLength(1);
		const st = screen.getByTestId('source-0-status');
		expect(st.textContent).toContain('Running');
		expect(st.querySelector('svg')).toBeTruthy();
		expect(screen.getByTestId('source-0-found').textContent).toBe('1,840,221 unique finds in 24 h');
		expect(screen.getByTestId('source-0-stored').textContent).toBe('30,115 of them stored');
		expect(screen.getByTestId('source-0-yield').getAttribute('aria-hidden')).toBe('true');
		expect(screen.getByTestId('source-0-risk').textContent).toBe('Low risk');
		expect(screen.getByTestId('source-1-consent').textContent).toBe('External');
		expect(screen.getByTestId('source-2-restart')).toBeTruthy();
		expect(screen.getByTestId('source-2-last-error').textContent).toContain('timeout');
		expect(writes()).toHaveLength(0);
	});

	it('shows needs-setup and opens the consent dialog instead of sending an enable', async () => {
		setup();
		await screen.findByTestId('source-1');
		expect(screen.getByTestId('source-1-status').textContent).toContain('Needs setup');
		expect(screen.getByTestId('source-1-need').textContent).toContain('sites it contacts');
		const toggle = within(screen.getByTestId('source-1')).getByRole('switch') as HTMLInputElement;
		expect(toggle.disabled).toBe(false);
		await fireEvent.click(toggle);
		await screen.findByTestId('consent-text');
		expect(toggle.checked).toBe(false);
		expect(writes()).toHaveLength(0);
	});

	it('lists the full addresses an acknowledgement covers, paths included', async () => {
		const cache = source({
			id: 9,
			name: 'Torrent cache',
			kinds: ['enrichment'],
			consent: 'external',
			consent_required: true,
			needs: 'endpoints_ack',
			endpoints: ['https://a.example/~me/{hash}.torrent', 'https://b.example/get?h={HASH}']
		});
		const empty = source({ ...cache, id: 10, endpoints: [] });
		setup(sourceList(dht, cache, empty));
		const list = await screen.findByTestId('source-9-addresses');
		const items = within(list)
			.getAllByRole('listitem')
			.map((li) => li.textContent);
		expect(items).toEqual([
			'https://a.example/~me/{hash}.torrent',
			'https://b.example/get?h={HASH}'
		]);
		expect(screen.getByTestId('source-10-addresses').textContent).toContain('No address');
		// A source that needs nothing to be acknowledged shows no list.
		expect(screen.queryByTestId('source-0-addresses')).toBeNull();
		expect(writes()).toHaveLength(0);
	});

	it('turns a source off with a PATCH and replaces the row with the reply', async () => {
		setup();
		net.on('PATCH /sources/{id}', {
			body: { ...dht, enabled: false, state: 'disabled', restart_required: true }
		});
		const toggle = within(await screen.findByTestId('source-0')).getByRole('switch');
		await fireEvent.click(toggle);
		await waitFor(() => expect(screen.getByTestId('source-0-restart')).toBeTruthy());
		expect(writes()).toHaveLength(1);
		expect(JSON.parse(writes()[0]!.body!)).toEqual({ enabled: false });
	});

	it('changes priority with a PATCH', async () => {
		setup();
		net.on('PATCH /sources/{id}', { body: { ...dht, priority: 'high' } });
		const select = (await screen.findByTestId('source-0-priority')) as HTMLSelectElement;
		await fireEvent.change(select, { target: { value: 'high' } });
		await waitFor(() => expect(writes()).toHaveLength(1));
		expect(JSON.parse(writes()[0]!.body!)).toEqual({ priority: 'high' });
		await waitFor(() =>
			expect((screen.getByTestId('source-0-priority') as HTMLSelectElement).value).toBe('high')
		);
	});

	it("shows the server's refusal and reloads the list", async () => {
		setup(sourceList(source({ id: 7, name: 'Plain', kinds: ['enrichment'] })));
		net.on('PATCH /sources/{id}', {
			status: 409,
			body: { error: { code: 'consent_required', message: 'consent needed' } }
		});
		const toggle = within(await screen.findByTestId('source-7')).getByRole('switch');
		await fireEvent.click(toggle);
		const alert = await screen.findByTestId('sources-problem');
		expect(alert.textContent).toContain('needs setup');
		await waitFor(() => expect(net.sent.filter((s) => s.method === 'GET')).toHaveLength(2));
	});

	it('says so when the list cannot load', async () => {
		net = mockFetch();
		net.on('GET /sources', { status: 500, body: { error: { code: 'x', message: 'x' } } });
		render(SourcesScreen);
		expect(await screen.findByTestId('sources-failed')).toBeTruthy();
	});

	it('names every control with its source', async () => {
		setup();
		await screen.findByTestId('source-0');
		expect(screen.getByRole('switch', { name: 'Mainline DHT: on' })).toBeTruthy();
		expect(screen.getByRole('switch', { name: 'Nyaa: on' })).toBeTruthy();
		expect(screen.getByRole('combobox', { name: 'Nyaa: priority' })).toBeTruthy();
	});

	it('turns an acknowledged source back on with a plain enable', async () => {
		setup(sourceList(nyaaAcked));
		net.on('PATCH /sources/{id}', { body: { ...nyaaAcked, enabled: true, state: 'enabled' } });
		const toggle = within(await screen.findByTestId('source-1')).getByRole('switch');
		expect((toggle as HTMLInputElement).disabled).toBe(false);
		await fireEvent.click(toggle);
		await waitFor(() => expect(writes()).toHaveLength(1));
		expect(JSON.parse(writes()[0]!.body!)).toEqual({ enabled: true });
		expect(await screen.findByTestId('sources-announce')).toBeTruthy();
		await waitFor(() =>
			expect(screen.getByTestId('sources-announce').textContent).toBe('Nyaa updated.')
		);
	});

	it('puts the switch back when the change is refused, even if the reload fails too', async () => {
		setup(sourceList(dht));
		net.on('PATCH /sources/{id}', {
			status: 400,
			body: { error: { code: 'bad', message: 'no' } }
		});
		const toggle = within(await screen.findByTestId('source-0')).getByRole(
			'switch'
		) as HTMLInputElement;
		expect(toggle.checked).toBe(true);
		net.on('GET /sources', { status: 500, body: { error: { code: 'x', message: 'x' } } });
		await fireEvent.click(toggle);
		await screen.findByTestId('sources-problem');
		await waitFor(() => expect(toggle.checked).toBe(true));
		// The reload failed after a first good load: the old rows stay, marked as possibly stale.
		expect(await screen.findByTestId('sources-stale')).toBeTruthy();
		expect(screen.getByTestId('source-0')).toBeTruthy();
	});

	it('says plainly when Lodestar cannot be reached', async () => {
		setup(sourceList(dht));
		net.on('PATCH /sources/{id}', () => {
			throw new TypeError('network down');
		});
		await fireEvent.click(within(await screen.findByTestId('source-0')).getByRole('switch'));
		expect((await screen.findByTestId('sources-problem')).textContent).toContain(
			"Couldn't reach Lodestar"
		);
	});

	it('ignores a second change while one is in flight', async () => {
		setup(sourceList(dht));
		let release: () => void = () => {};
		const held = new Promise<void>((r) => (release = r));
		net.on('PATCH /sources/{id}', async () => {
			await held;
			return { body: { ...dht, priority: 'high' } };
		});
		const select = (await screen.findByTestId('source-0-priority')) as HTMLSelectElement;
		await fireEvent.change(select, { target: { value: 'high' } });
		await waitFor(() => expect(writes()).toHaveLength(1));
		await fireEvent.change(select, { target: { value: 'max' } });
		expect(writes()).toHaveLength(1);
		release();
		await waitFor(() => expect(select.value).toBe('high'));
		expect(select.disabled).toBe(false);
	});

	it('drops a list read that started before another change was answered', async () => {
		const other = source({ id: 8, name: 'Other', kinds: ['enrichment'] });
		setup(sourceList(dht, other));
		await screen.findByTestId('source-8');
		let release: () => void = () => {};
		const held = new Promise<void>((r) => (release = r));
		net.on('GET /sources', async () => {
			await held;
			return { body: sourceList(dht, other) };
		});
		// Row 0 is refused: its reload starts and waits. Row 8's change lands meanwhile.
		net.on('PATCH /sources/{id}', (req) =>
			req.path.endsWith('/0')
				? { status: 400, body: { error: { code: 'bad', message: 'no' } } }
				: { body: { ...other, priority: 'high' } }
		);
		await fireEvent.change(screen.getByTestId('source-0-priority'), { target: { value: 'low' } });
		await waitFor(() => expect(net.sent.filter((s) => s.method === 'GET')).toHaveLength(2));
		await fireEvent.change(screen.getByTestId('source-8-priority'), { target: { value: 'high' } });
		await waitFor(() =>
			expect(screen.getByTestId('sources-announce').textContent).toBe('Other updated.')
		);
		release();
		await waitFor(() => expect(screen.queryByTestId('sources-stale')).toBeNull());
		expect((screen.getByTestId('source-8-priority') as HTMLSelectElement).value).toBe('high');
	});

	it('keeps the toggle off for a purged row, a purge in progress and a row not editable', async () => {
		setup(
			sourceList(
				source({ id: 20, name: 'Purged', state: 'purged' }),
				source({ id: 21, name: 'Half', purge_pending: true }),
				source({ id: 22, name: 'Fixed', editable: [] })
			)
		);
		await screen.findByTestId('source-20');
		for (const id of [20, 21, 22]) {
			const t = within(screen.getByTestId(`source-${id}`)).getByRole('switch') as HTMLInputElement;
			expect(t.disabled).toBe(true);
		}
		expect(screen.getByTestId('source-21-status').textContent).toContain('Purge unfinished');
		expect(writes()).toHaveLength(0);
	});
});
