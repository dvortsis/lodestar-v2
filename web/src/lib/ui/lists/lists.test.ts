import { beforeAll, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { stubLayout } from './testing/layout';
import ListHarness from './testing/ListHarness.svelte';
import TableHarness from './testing/TableHarness.svelte';
import TreeHarness from './testing/TreeHarness.svelte';
import type { FlatNode } from './tree-model';

beforeAll(() => stubLayout());

const key = (el: Element, k: string, init: KeyboardEventInit = {}) =>
	fireEvent.keyDown(el, { key: k, ...init });
const active = () => document.activeElement as HTMLElement;
const names = (n: number, prefix = 'item') =>
	Array.from({ length: n }, (_, i) => ({ id: `${prefix}${i}`, name: `${prefix} ${i}` }));

describe('VirtualList', () => {
	it('exposes listbox semantics, windows the rows and sets size/position', () => {
		render(ListHarness, { items: names(1000) });
		const list = screen.getByRole('listbox', { name: 'Results' });
		const options = within(list).getAllByRole('option');
		expect(options.length).toBeGreaterThan(5);
		expect(options.length).toBeLessThan(40);
		expect(options[0]!.getAttribute('aria-setsize')).toBe('1000');
		expect(options[0]!.getAttribute('aria-posinset')).toBe('1');
	});

	it('uses a roving tabindex: exactly one row is tabbable', () => {
		render(ListHarness, { items: names(50) });
		const tabbable = screen.getAllByRole('option').filter((o) => o.tabIndex === 0);
		expect(tabbable).toHaveLength(1);
		expect(tabbable[0]!.textContent).toContain('item 0');
	});

	it('moves with arrows, Home and End, focusing the new row', async () => {
		const onActiveChange = vi.fn();
		render(ListHarness, { items: names(50), onActiveChange });
		const first = screen.getAllByRole('option')[0]!;
		first.focus();
		await key(first, 'ArrowDown');
		await waitFor(() => expect(active().textContent).toContain('item 1'));
		expect(onActiveChange).toHaveBeenLastCalledWith(1, expect.objectContaining({ id: 'item1' }));
		await key(active(), 'ArrowUp');
		await waitFor(() => expect(active().textContent).toContain('item 0'));
		await key(active(), 'End');
		await waitFor(() => expect(active().textContent).toContain('item 49'));
		await key(active(), 'Home');
		await waitFor(() => expect(active().textContent).toContain('item 0'));
		expect(screen.getByTestId('active').textContent).toBe('0');
	});

	it('does not move past either end', async () => {
		render(ListHarness, { items: names(3) });
		const first = screen.getAllByRole('option')[0]!;
		first.focus();
		await key(first, 'ArrowUp');
		expect(screen.getByTestId('active').textContent).toBe('0');
		await key(first, 'End');
		await waitFor(() => expect(screen.getByTestId('active').textContent).toBe('2'));
		await key(active(), 'ArrowDown');
		expect(screen.getByTestId('active').textContent).toBe('2');
	});

	it('activates on Enter and on click', async () => {
		const onActivate = vi.fn();
		render(ListHarness, { items: names(5), onActivate });
		const first = screen.getAllByRole('option')[0]!;
		first.focus();
		await key(first, 'Enter');
		expect(onActivate).toHaveBeenLastCalledWith(0, expect.objectContaining({ id: 'item0' }));
		await fireEvent.click(screen.getAllByRole('option')[2]!);
		expect(onActivate).toHaveBeenLastCalledWith(2, expect.objectContaining({ id: 'item2' }));
		expect(screen.getByTestId('active').textContent).toBe('2');
	});

	it('hands keys to the caller first, which can claim them (j/k-ready)', async () => {
		const onKey = vi.fn((e: KeyboardEvent) => {
			if (e.key === 'j') e.preventDefault();
		});
		render(ListHarness, { items: names(5), onKey });
		const first = screen.getAllByRole('option')[0]!;
		first.focus();
		await key(first, 'j');
		expect(onKey).toHaveBeenCalledWith(
			expect.anything(),
			expect.objectContaining({ index: 0, item: expect.objectContaining({ id: 'item0' }) })
		);
		expect(screen.getByTestId('active').textContent).toBe('0');
	});

	it('typeahead jumps to the next row starting with the typed text', async () => {
		const items = [
			{ id: '1', name: 'Alpha' },
			{ id: '2', name: 'Beta' },
			{ id: '3', name: 'Gamma' }
		];
		render(ListHarness, { items });
		const first = screen.getAllByRole('option')[0]!;
		first.focus();
		await key(first, 'g');
		await waitFor(() => expect(active().textContent).toContain('Gamma'));
	});

	it('keeps the same row active when items are prepended (live inserts)', async () => {
		const items = names(30);
		const { rerender } = render(ListHarness, { items });
		const third = screen.getAllByRole('option')[2]!;
		await fireEvent.click(third);
		third.focus();
		await rerender({ items: [...names(3, 'new'), ...items] });
		await waitFor(() => expect(screen.getByTestId('active').textContent).toBe('5'));
		const stillActive = screen.getAllByRole('option').find((o) => o.tabIndex === 0)!;
		expect(stillActive.textContent).toContain('item 2');
		expect(active()).toBe(stillActive);
	});

	it('asks for more once, when the sentinel row renders, and announces loading', async () => {
		const onLoadMore = vi.fn();
		const { rerender } = render(ListHarness, { items: names(3), hasMore: true, onLoadMore });
		await waitFor(() => expect(onLoadMore).toHaveBeenCalledTimes(1));
		await rerender({ items: names(3), hasMore: true, onLoadMore, loadingMore: true });
		expect(screen.getAllByText('Loading more').length).toBeGreaterThan(0);
		expect(screen.getByRole('status').textContent).toBe('Loading more');
		expect(onLoadMore).toHaveBeenCalledTimes(1);
		await rerender({ items: names(6), hasMore: true, onLoadMore, loadingMore: false });
		await waitFor(() => expect(onLoadMore).toHaveBeenCalledTimes(2));
	});

	it('does not render the sentinel or ask when there is nothing more', () => {
		const onLoadMore = vi.fn();
		render(ListHarness, { items: names(3), hasMore: false, onLoadMore });
		expect(onLoadMore).not.toHaveBeenCalled();
	});

	it('renders an empty list without options', () => {
		render(ListHarness, { items: [] });
		expect(screen.getByRole('listbox').querySelectorAll('[role="option"]')).toHaveLength(0);
	});
});

const torrents = Array.from({ length: 20 }, (_, i) => ({
	id: `t${i}`,
	name: `Torrent ${i}`,
	size: `${i} GiB`,
	seeders: String(i * 3)
}));

describe('Table', () => {
	it('has grid semantics with a header row and row counts', () => {
		render(TableHarness, { rows: torrents });
		const grid = screen.getByRole('grid', { name: 'Torrents' });
		expect(grid.getAttribute('aria-rowcount')).toBe('21');
		const headers = within(grid).getAllByRole('columnheader');
		expect(headers.map((h) => h.textContent?.trim())).toEqual(['', 'Name', 'Size', 'Seeders']);
		const rows = within(grid).getAllByRole('row');
		expect(rows[0]!.getAttribute('aria-rowindex')).toBe('1');
		expect(rows[1]!.getAttribute('aria-rowindex')).toBe('2');
	});

	it('sorts from the header: none → ascending → descending → none, with aria-sort', async () => {
		render(TableHarness, { rows: torrents });
		const header = () => screen.getByRole('columnheader', { name: /^Size/ });
		const button = () => within(header()).getByRole('button');
		expect(header().getAttribute('aria-sort')).toBe('none');
		await fireEvent.click(button());
		expect(header().getAttribute('aria-sort')).toBe('ascending');
		expect(screen.getByTestId('sort').textContent).toBe('size:asc');
		await fireEvent.click(button());
		expect(header().getAttribute('aria-sort')).toBe('descending');
		await fireEvent.click(button());
		expect(header().getAttribute('aria-sort')).toBe('none');
		// An unsortable column carries no aria-sort at all.
		expect(screen.getByRole('columnheader', { name: 'Seeders' }).hasAttribute('aria-sort')).toBe(
			false
		);
	});

	it('sorting one column resets the other', async () => {
		render(TableHarness, { rows: torrents });
		await fireEvent.click(
			within(screen.getByRole('columnheader', { name: /^Size/ })).getByRole('button')
		);
		await fireEvent.click(
			within(screen.getByRole('columnheader', { name: /^Name/ })).getByRole('button')
		);
		expect(screen.getByRole('columnheader', { name: /^Size/ }).getAttribute('aria-sort')).toBe(
			'none'
		);
		expect(screen.getByTestId('sort').textContent).toBe('name:asc');
	});

	it('selects rows with the checkbox, with shift for a range, and select-all', async () => {
		const onSelected = vi.fn();
		render(TableHarness, { rows: torrents, onSelected });
		const box = (n: number) => screen.getByLabelText(`Select Torrent ${n}`);
		await fireEvent.click(box(1));
		expect(screen.getByTestId('selected').textContent).toBe('t1');
		await fireEvent.click(box(4), { shiftKey: true });
		expect(screen.getByTestId('selected').textContent).toBe('t1,t2,t3,t4');
		// Shift from the anchor (row 4) to a selected row clears the range between them.
		await fireEvent.click(box(2), { shiftKey: true });
		expect(screen.getByTestId('selected').textContent).toBe('t1');
		await fireEvent.click(box(3));
		expect(screen.getByTestId('selected').textContent).toBe('t1,t3');
		const all = screen.getByLabelText('Select all') as HTMLInputElement;
		expect(all.indeterminate).toBe(true);
		await fireEvent.click(all);
		expect(screen.getByTestId('selected').textContent?.split(',')).toHaveLength(20);
		await fireEvent.click(all);
		expect(screen.getByTestId('selected').textContent).toBe('');
	});

	it('marks selected rows with aria-selected', async () => {
		render(TableHarness, { rows: torrents });
		await fireEvent.click(screen.getByLabelText('Select Torrent 0'));
		expect(screen.getAllByRole('row')[1]!.getAttribute('aria-selected')).toBe('true');
		expect(screen.getAllByRole('row')[2]!.getAttribute('aria-selected')).toBe('false');
	});

	it('Space on a focused row toggles its selection', async () => {
		render(TableHarness, { rows: torrents });
		const row = screen.getAllByRole('row')[1]!;
		row.focus();
		await key(row, ' ');
		expect(screen.getByTestId('selected').textContent).toBe('t0');
	});

	it('column chooser hides and restores columns, but not required ones', async () => {
		render(TableHarness, { rows: torrents });
		await fireEvent.click(screen.getByRole('button', { name: 'Columns' }));
		const seeders = (await screen.findByLabelText('Seeders', {
			selector: 'input'
		})) as HTMLInputElement;
		expect(seeders.checked).toBe(true);
		expect(
			(screen.getByLabelText('Name', { selector: 'input' }) as HTMLInputElement).disabled
		).toBe(true);
		await fireEvent.click(seeders);
		await waitFor(() => expect(screen.queryByRole('columnheader', { name: 'Seeders' })).toBeNull());
		await fireEvent.click(seeders);
		await waitFor(() => expect(screen.getByRole('columnheader', { name: 'Seeders' })).toBeTruthy());
	});

	it('virtualises large sets', () => {
		const rows = Array.from({ length: 5000 }, (_, i) => ({
			id: `r${i}`,
			name: `R ${i}`,
			size: '1 B',
			seeders: '1'
		}));
		render(TableHarness, { rows });
		expect(screen.getAllByRole('row').length).toBeLessThan(60);
	});
});

const sample: FlatNode[] = [
	{ id: 'a', depth: 0, name: 'Alpha', dir: true, size: '3 MiB' },
	{ id: 'a/b', depth: 1, name: 'Bravo', dir: true, size: '2 MiB' },
	{ id: 'a/b/x', depth: 2, name: 'x.mkv', dir: false, size: '1 MiB' },
	{ id: 'a/b/y', depth: 2, name: 'y.mkv', dir: false, size: '1 MiB' },
	{ id: 'a/z', depth: 1, name: 'z.nfo', dir: false, size: '1 MiB' },
	{ id: 'c', depth: 0, name: 'Charlie.txt', dir: false, size: '1 KiB' },
	{ id: 'd', depth: 0, name: 'Delta', dir: true, size: '1 KiB' },
	{ id: 'd/e', depth: 1, name: 'e.bin', dir: false, size: '1 KiB' }
];
const items = () => screen.getAllByRole('treeitem');
const label = (el: Element) => el.textContent?.replace(/\s+/g, ' ').trim() ?? '';

describe('Tree', () => {
	it('shows only top-level rows collapsed, with tree semantics and sizes', () => {
		render(TreeHarness, { nodes: sample });
		const tree = screen.getByRole('tree', { name: 'Files' });
		expect(within(tree).getAllByRole('treeitem')).toHaveLength(3);
		const [alpha, charlie] = items();
		expect(alpha!.getAttribute('aria-level')).toBe('1');
		expect(alpha!.getAttribute('aria-expanded')).toBe('false');
		expect(alpha!.getAttribute('aria-posinset')).toBe('1');
		expect(alpha!.getAttribute('aria-setsize')).toBe('3');
		expect(label(alpha!)).toContain('3 MiB');
		expect(charlie!.hasAttribute('aria-expanded')).toBe(false);
	});

	it('Right expands, then moves to the first child; Left collapses, then goes to the parent', async () => {
		render(TreeHarness, { nodes: sample });
		items()[0]!.focus();
		await key(active(), 'ArrowRight');
		await waitFor(() => expect(items()).toHaveLength(5));
		expect(items()[0]!.getAttribute('aria-expanded')).toBe('true');
		await key(active(), 'ArrowRight');
		await waitFor(() => expect(label(active())).toContain('Bravo'));
		expect(active().getAttribute('aria-level')).toBe('2');
		await key(active(), 'ArrowLeft'); // Bravo is closed: go to the parent
		await waitFor(() => expect(label(active())).toContain('Alpha'));
		await key(active(), 'ArrowLeft'); // Alpha is open: collapse
		await waitFor(() => expect(items()).toHaveLength(3));
	});

	it('Down/Up/Home/End walk the visible rows only', async () => {
		render(TreeHarness, { nodes: sample });
		items()[0]!.focus();
		await key(active(), 'ArrowDown');
		await waitFor(() => expect(label(active())).toContain('Charlie'));
		await key(active(), 'End');
		await waitFor(() => expect(label(active())).toContain('Delta'));
		await key(active(), 'Home');
		await waitFor(() => expect(label(active())).toContain('Alpha'));
	});

	it('* expands every sibling directory', async () => {
		render(TreeHarness, { nodes: sample });
		items()[0]!.focus();
		await key(active(), '*');
		await waitFor(() =>
			expect(items().map((i) => i.getAttribute('aria-expanded'))).toContain('true')
		);
		const open = items().filter((i) => i.getAttribute('aria-expanded') === 'true');
		expect(open.map((i) => label(i).replace(/ \d.*/, ''))).toEqual(['Alpha', 'Delta']);
		expect(items()).toHaveLength(3 + 2 + 1);
	});

	it('typeahead focuses the next matching row', async () => {
		render(TreeHarness, { nodes: sample });
		items()[0]!.focus();
		await key(active(), 'd');
		await waitFor(() => expect(label(active())).toContain('Delta'));
	});

	it('Space checks a file and parents become mixed, then on', async () => {
		const onChecked = vi.fn();
		render(TreeHarness, { nodes: sample, onChecked });
		items()[0]!.focus();
		await key(active(), '*');
		await waitFor(() => expect(items().length).toBeGreaterThan(3));
		// Check Bravo's first file by clicking its checkbox.
		await key(active(), 'ArrowDown'); // Bravo
		await waitFor(() => expect(label(active())).toContain('Bravo'));
		await key(active(), 'ArrowRight'); // expand Bravo
		await waitFor(() => expect(items().some((i) => label(i).includes('x.mkv'))).toBe(true));
		await key(active(), 'ArrowRight'); // first child: x.mkv
		await waitFor(() => expect(label(active())).toContain('x.mkv'));
		await key(active(), ' ');
		expect(onChecked).toHaveBeenLastCalledWith(new Set(['a/b/x']));
		const byName = (n: string) => items().find((i) => label(i).includes(n))!;
		expect(byName('x.mkv').getAttribute('aria-checked')).toBe('true');
		expect(byName('Bravo').getAttribute('aria-checked')).toBe('mixed');
		expect(byName('Alpha').getAttribute('aria-checked')).toBe('mixed');
		expect(byName('Delta').getAttribute('aria-checked')).toBe('false');
		await key(byName('y.mkv'), ' ');
		expect(byName('Bravo').getAttribute('aria-checked')).toBe('true');
	});

	it('checking a directory selects every file beneath it, collapsed or not', async () => {
		const onChecked = vi.fn();
		render(TreeHarness, { nodes: sample, onChecked });
		items()[0]!.focus();
		await key(active(), ' ');
		expect(onChecked).toHaveBeenLastCalledWith(new Set(['a/b/x', 'a/b/y', 'a/z']));
		expect(items()[0]!.getAttribute('aria-checked')).toBe('true');
		expect(screen.getByTestId('checked').textContent).toBe('3');
		await key(active(), ' ');
		expect(screen.getByTestId('checked').textContent).toBe('0');
	});

	it('the checkbox is labelled and the indeterminate flag follows the mixed state', async () => {
		render(TreeHarness, { nodes: sample });
		items()[0]!.focus();
		await key(active(), '*');
		await waitFor(() => expect(items().length).toBeGreaterThan(3));
		const bravoBox = screen.getByLabelText('Select Bravo') as HTMLInputElement;
		expect(bravoBox.indeterminate).toBe(false);
		await fireEvent.click(screen.getByLabelText('Select z.nfo'));
		await waitFor(() =>
			expect((screen.getByLabelText('Select Alpha') as HTMLInputElement).indeterminate).toBe(true)
		);
	});

	it('renders an empty tree', () => {
		render(TreeHarness, { nodes: [] });
		expect(screen.getByRole('tree').querySelectorAll('[role="treeitem"]')).toHaveLength(0);
	});

	it('mounts 100k nodes and renders only a window of rows', () => {
		const big: FlatNode[] = [];
		for (let a = 0; a < 100; a++) {
			big.push({ id: `d${a}`, depth: 0, name: `dir ${a}`, dir: true });
			for (let b = 0; b < 999; b++)
				big.push({ id: `d${a}/f${b}`, depth: 1, name: `file ${b}`, dir: false });
		}
		const t0 = performance.now();
		render(TreeHarness, { nodes: big });
		expect(performance.now() - t0).toBeLessThan(3000);
		expect(items().length).toBeGreaterThan(5);
		expect(items().length).toBeLessThan(40);
	});
});
