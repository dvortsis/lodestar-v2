import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { mockFetch } from '../filters/flows/testing/fetch';
import SourcesScreen from './SourcesScreen.svelte';
import { dht, nyaa, prowlarr, source, sourceList } from './fixtures';

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
		expect(screen.getByTestId('source-0-found').textContent).toBe('1,840,221 found in 24 h');
		expect(screen.getByTestId('source-0-stored').textContent).toBe('30,115 stored');
		const bar = screen.getByTestId('source-0-yield');
		expect(bar.getAttribute('aria-label')).toContain('30,115');
		expect(screen.getByTestId('source-2-restart')).toBeTruthy();
		expect(screen.getByTestId('source-2-last-error').textContent).toContain('timeout');
		expect(writes()).toHaveLength(0);
	});

	it('shows needs-setup and never sends an enable for a source the gate would refuse', async () => {
		setup();
		await screen.findByTestId('source-1');
		expect(screen.getByTestId('source-1-status').textContent).toContain('Needs setup');
		expect(screen.getByTestId('source-1-need').textContent).toContain('sites it contacts');
		const toggle = within(screen.getByTestId('source-1')).getByRole('switch') as HTMLInputElement;
		expect(toggle.disabled).toBe(true);
		await fireEvent.click(toggle);
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
});
