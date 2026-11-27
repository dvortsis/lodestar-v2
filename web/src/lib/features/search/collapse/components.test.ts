// The collapse pieces on their own: the switch, the "+N" disclosure, and a row
// under an open group.
import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import { hit } from '../results/fixtures';
import type { GroupRow } from './expand.svelte';

vi.setConfig({ testTimeout: 20_000 });

const get = vi.hoisted(() => vi.fn(() => new Promise(() => {})));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));

const { default: GroupToggle } = await import('./GroupToggle.svelte');
const { default: Others } = await import('./Others.svelte');
const { default: VersionRow } = await import('./VersionRow.svelte');

describe('GroupToggle', () => {
	it('is a labelled switch that says what on means, and reports a change', async () => {
		const onchange = vi.fn();
		const { rerender } = render(GroupToggle, { on: true, onchange });
		const sw = screen.getByRole('switch', { name: 'Group versions' });
		expect((sw as HTMLInputElement).checked).toBe(true);
		expect(sw.getAttribute('aria-describedby')).toBeNull();
		await fireEvent.click(sw);
		expect(onchange).toHaveBeenCalledWith(false);
		// It shows `on` only: the press waits for the new value, and a change that never lands
		// leaves it as it was.
		expect((sw as HTMLInputElement).checked).toBe(true);
		await rerender({ on: false });
		expect((sw as HTMLInputElement).checked).toBe(false);
		await fireEvent.click(sw);
		expect(onchange).toHaveBeenLastCalledWith(true);
		expect((sw as HTMLInputElement).checked).toBe(false);
		// It reads the account's choice when it mounts.
		expect(get).toHaveBeenCalledWith('/settings/{section}', {
			path: { section: 'preferences' }
		});
	});

	it('stays usable and points at the line that says why the rows came ungrouped', () => {
		render(GroupToggle, { on: true, describedby: 'why', onchange: () => {} });
		const sw = screen.getByRole('switch', { name: 'Group versions' });
		expect((sw as HTMLInputElement).disabled).toBe(false);
		expect(sw.getAttribute('aria-describedby')).toBe('why');
	});
});

describe('Others', () => {
	it('names the other versions, says whether they are open, and toggles', async () => {
		const ontoggle = vi.fn();
		const { rerender } = render(Others, { others: 3, open: false, testid: 'r', ontoggle });
		const b = screen.getByTestId('r-others');
		expect(b.textContent?.trim()).toBe('+3 other versions');
		expect(b.getAttribute('aria-expanded')).toBe('false');
		expect(b.getAttribute('tabindex')).toBe('-1');
		await fireEvent.click(b);
		expect(ontoggle).toHaveBeenCalledOnce();
		await rerender({ others: 1, open: true, testid: 'r', ontoggle });
		expect(b.textContent?.trim()).toBe('+1 other version');
		expect(b.getAttribute('aria-expanded')).toBe('true');
	});
});

describe('VersionRow', () => {
	const parent = hit(1, { others: 7 });
	const row = (r: Partial<GroupRow> & { row: GroupRow['row'] }) =>
		({ parent, key: 'k', ...r }) as GroupRow;

	it('shows another version compactly, naming its group for screen readers', () => {
		const v = hit(11, { resolution: '1080p', seeders: 1200 });
		render(VersionRow, { row: row({ row: 'version', hit: v }), testid: 'v' });
		expect(screen.getByTestId('v-title').textContent).toBe('Lantern Keeper 11');
		expect(screen.getByTestId('v').textContent).toContain('Other version of Lantern Keeper 1');
		expect(screen.getByTestId('v').textContent).toContain('1080p');
		expect(screen.getByTestId('v-size').textContent?.replace(/\s/g, ' ')).toBe('2.00 GiB');
		expect(screen.getByTestId('v-seeders').textContent).toBe('1,200');
	});

	it('counts the whole group in Show all, or names no number it can’t be sure of', () => {
		const a = render(VersionRow, { row: row({ row: 'more', total: 8 }), testid: 'v' });
		expect(screen.getByTestId('v-more').textContent).toBe('Show all 8');
		a.unmount();
		render(VersionRow, { row: row({ row: 'more', total: null }), testid: 'v' });
		expect(screen.getByTestId('v-more').textContent).toBe('Show all versions');
	});

	it('says when versions are loading, failed or missing', () => {
		const a = render(VersionRow, { row: row({ row: 'loading' }), testid: 'v' });
		expect(screen.getByRole('status').textContent).toBe('Loading other versions…');
		a.unmount();
		const b = render(VersionRow, { row: row({ row: 'failed' }), testid: 'v' });
		expect(screen.getByTestId('v').textContent).toContain('Retry');
		b.unmount();
		render(VersionRow, { row: row({ row: 'none' }), testid: 'v' });
		expect(screen.getByTestId('v').dataset.row).toBe('none');
	});
});
