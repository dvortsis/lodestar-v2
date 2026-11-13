// Settings → Sources' layout (X-sources-restructure, owner §2.66): the filter box and its empty
// state, the groups' summary lines and collapse, rows that expand, and the consent legend.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { mockFetch } from '../filters/flows/testing/fetch';
import SourcesScreen from './SourcesScreen.svelte';
import { darknet, dht, lime, nyaa, prowlarr, sourceList } from './fixtures';

configure({ asyncUtilTimeout: 10_000 });
vi.setConfig({ testTimeout: 30_000 });

let net: ReturnType<typeof mockFetch>;
function setup(list = sourceList(dht, nyaa, prowlarr, lime, darknet)) {
	net = mockFetch();
	net.on('GET /sources', { body: list });
	return render(SourcesScreen);
}
afterEach(() => vi.unstubAllGlobals());

const filter = () => screen.getByRole('searchbox', { name: 'Filter sources' });
const rowNames = () =>
	[...document.querySelectorAll<HTMLElement>('[data-testid$="-expand"]')]
		.filter((b) => !b.closest('[hidden]'))
		.map((b) => b.textContent?.trim());
const writes = () => net.sent.filter((s) => s.method !== 'GET');

describe('Sources layout', () => {
	it('filters rows by name or kind, says how many show, and leaves out empty groups', async () => {
		setup();
		await screen.findByTestId('source-0');
		expect(rowNames()).toHaveLength(5);
		expect(screen.getByTestId('sources-filter-count').textContent?.trim()).toBe('');

		await fireEvent.input(filter(), { target: { value: 'nyaa' } });
		expect(rowNames()).toEqual(['Nyaa']);
		expect(screen.getByTestId('sources-filter-count').textContent).toBe('Showing 1 of 5 sources');
		expect(screen.queryByTestId('sources-group-importer')).toBeNull();

		await fireEvent.input(filter(), { target: { value: 'IMPORTERS' } });
		expect(rowNames()).toEqual(['Prowlarr']);
		expect(writes()).toHaveLength(0);
	});

	it('shows an empty state when nothing matches, and Clear filter brings every row back', async () => {
		setup();
		await screen.findByTestId('source-0');
		await fireEvent.input(filter(), { target: { value: 'zzz' } });
		expect(rowNames()).toEqual([]);
		const empty = screen.getByTestId('sources-filter-empty');
		expect(within(empty).getByRole('heading', { level: 2 }).textContent).toBe('No sources match');
		await fireEvent.click(within(empty).getByRole('button', { name: 'Clear filter' }));
		expect(rowNames()).toHaveLength(5);
		expect((filter() as HTMLInputElement).value).toBe('');
		// The button left with the empty state: the keyboard is back in the filter.
		await waitFor(() => expect(document.activeElement).toBe(filter()));
		expect(screen.queryByTestId('sources-filter-empty')).toBeNull();
	});

	it('gives each group a summary line: how many are on and how many need attention', async () => {
		setup();
		await screen.findByTestId('source-0');
		expect(screen.getByTestId('sources-group-discovery-summary').textContent?.trim()).toBe(
			'1 of 3 on'
		);
		expect(screen.getByTestId('sources-group-importer-summary').textContent?.trim()).toBe(
			'1 of 1 on'
		);
		expect(screen.getByTestId('sources-group-importer-attention').textContent?.trim()).toBe(
			'Needs attention: 1'
		);
		expect(screen.queryByTestId('sources-group-discovery-attention')).toBeNull();
	});

	it('collapses and opens a group by its button, and a new filter opens it again', async () => {
		setup();
		await screen.findByTestId('source-0');
		const toggle = screen.getByTestId('sources-group-discovery-toggle');
		const list = screen.getByTestId('sources-group-discovery');
		expect(toggle.getAttribute('aria-expanded')).toBe('true');
		expect(toggle.getAttribute('aria-controls')).toBe(list.id);
		await fireEvent.click(toggle);
		expect(toggle.getAttribute('aria-expanded')).toBe('false');
		expect(list.hidden).toBe(true);
		expect(rowNames()).not.toContain('Nyaa');
		await fireEvent.input(filter(), { target: { value: 'a' } });
		expect(screen.getByTestId('sources-group-discovery-toggle').getAttribute('aria-expanded')).toBe(
			'true'
		);
		expect(rowNames()).toContain('Nyaa');
	});

	it('shows each row as one line that expands to its detail', async () => {
		setup();
		await screen.findByTestId('source-0');
		const expand = screen.getByRole('button', { name: 'Mainline DHT' });
		const detail = screen.getByTestId('source-0-detail');
		expect(expand.getAttribute('aria-expanded')).toBe('false');
		expect(expand.getAttribute('aria-controls')).toBe(detail.id);
		expect(detail.hidden).toBe(true);
		// The line keeps the switch; priority is in the detail.
		expect(screen.getByRole('switch', { name: 'Mainline DHT: on' })).toBeTruthy();
		expect(screen.queryByRole('combobox', { name: 'Mainline DHT: priority' })).toBeNull();
		await fireEvent.click(expand);
		expect(expand.getAttribute('aria-expanded')).toBe('true');
		expect(detail.hidden).toBe(false);
		expect(screen.getByRole('combobox', { name: 'Mainline DHT: priority' })).toBeTruthy();
		await fireEvent.click(expand);
		expect(detail.hidden).toBe(true);
	});

	it('keeps the setup note the switch points at in the page while the row is closed', async () => {
		setup();
		await screen.findByTestId('source-1');
		const toggle = screen.getByRole('switch', { name: 'Nyaa: on' });
		const note = document.getElementById(toggle.getAttribute('aria-describedby')!);
		expect(note?.textContent).toContain('sites it contacts');
	});

	it('names the consent classes the rows show in a legend', async () => {
		setup();
		await screen.findByTestId('source-0');
		const legend = screen.getByTestId('sources-legend');
		expect(within(legend).getByRole('heading', { level: 2 }).textContent).toBe('Consent classes');
		const terms = within(legend)
			.getAllByRole('term')
			.map((t) => t.textContent?.trim());
		expect(terms).toEqual(['Open', 'External', 'Account', 'Restricted', 'Overlay']);
		// Each badge a row shows is named in the legend.
		for (const id of [1, 2, 3, 4]) {
			const badge = screen.getByTestId(`source-${id}-consent`).textContent;
			expect(terms).toContain(badge);
		}
	});
});
