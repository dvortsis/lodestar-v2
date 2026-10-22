import { fireEvent, render, screen, within } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import CategoryBar from './CategoryBar.svelte';
import DataTableAlternative from './DataTableAlternative.svelte';
import HBarList from './HBarList.svelte';
import Histogram from './Histogram.svelte';
import Kpi from './Kpi.svelte';
import Sparkline from './Sparkline.svelte';

describe('Kpi', () => {
	it('formats the value and shows null as a dash', () => {
		const { rerender } = render(Kpi, { label: 'Indexed', value: 1234567 });
		expect(screen.getByTestId('kpi-value').textContent).toBe('1,234,567');
		rerender({ label: 'Indexed', value: null });
		expect(screen.getByTestId('kpi-value').textContent).toBe('—');
	});
	it('marks estimates with ≈ and keeps a real zero', () => {
		const { rerender } = render(Kpi, { label: 'DHT', value: { value: 2400, approx: true } });
		expect(screen.getByTestId('kpi-value').textContent).toMatch(/^≈\s2,400$/);
		rerender({ label: 'DHT', value: 0 });
		expect(screen.getByTestId('kpi-value').textContent).toBe('0');
	});
	it('shows the delta as icon + signed text + period, never colour alone', () => {
		const { container } = render(Kpi, {
			label: 'Indexed',
			value: 10,
			delta: { value: -3, text: 'vs last week' }
		});
		const delta = screen.getByTestId('kpi-delta');
		expect(delta.textContent).toContain('−3');
		expect(delta.textContent).toContain('vs last week');
		expect(container.querySelector('[data-trend="down"]')).not.toBeNull();
	});
	it('renders flat for a zero or unknown delta and an optional sparkline', () => {
		const { container } = render(Kpi, {
			label: 'x',
			value: 1,
			delta: { value: null, text: 'vs last week' },
			sparkline: { values: [1, null, 2], label: 'Trend' }
		});
		expect(container.querySelector('[data-trend="flat"]')).not.toBeNull();
		expect(screen.getByTestId('kpi-delta').textContent).toContain('—');
		expect(screen.getByRole('img', { name: 'Trend' })).toBeTruthy();
	});
});

describe('Sparkline', () => {
	it('exposes a label and the number of known points', () => {
		render(Sparkline, { values: [1, null, 3], label: 'Seeders' });
		expect(screen.getByTestId('sparkline').getAttribute('data-points')).toBe('2');
		expect(screen.getByRole('img', { name: 'Seeders' })).toBeTruthy();
	});
	it('renders no path when everything is unknown', () => {
		const { container } = render(Sparkline, { values: [null, null], label: 'None' });
		expect(container.querySelector('path')).toBeNull();
	});
});

describe('CategoryBar', () => {
	const segments = [
		{ group: 'software' as const, value: 5, label: 'software' },
		{ group: 'video' as const, value: 80, label: 'video' },
		{ group: 'audio' as const, value: 0, label: 'audio' },
		{ group: 'images' as const, value: null, label: 'images' }
	];
	it('uses the fixed order, skips empty groups and uses data tokens', () => {
		const { container } = render(CategoryBar, {
			segments,
			summary: '94% video',
			dominant: '94% video'
		});
		const spans = [...container.querySelectorAll<HTMLElement>('[data-group]')];
		expect(spans.map((s) => s.dataset.group)).toEqual(['video', 'software']);
		expect(spans[0]!.style.backgroundColor).toBe('var(--data-1)');
		expect(spans[1]!.style.backgroundColor).toBe('var(--data-3)');
	});
	it('has a text summary and visible dominant share', () => {
		render(CategoryBar, { segments, summary: '94% video, 6% software', dominant: '94% video' });
		expect(screen.getByRole('img', { name: '94% video, 6% software' })).toBeTruthy();
		expect(screen.getByTestId('category-bar-dominant').textContent).toBe('94% video');
	});
	it('scales to a shared max', () => {
		render(CategoryBar, { segments, summary: 's', max: 170 });
		expect(screen.getByTestId('category-bar-bar').style.width).toBe('50%');
	});
	it('fills the width without max', () => {
		render(CategoryBar, { segments, summary: 's' });
		expect(screen.getByTestId('category-bar-bar').style.width).toBe('100%');
	});
});

describe('HBarList', () => {
	const items = [
		{ key: 'b', label: 'Beta', value: 10, share: 0.25 },
		{ key: 'n', label: 'Nada', value: null },
		{ key: 'a', label: 'Alpha', value: 40, share: 0.75 },
		{ key: 'z', label: 'Zero', value: 0 }
	];
	it('sorts by value, unknowns last, with values and percentages', () => {
		render(HBarList, { items, label: 'Top' });
		const rows = screen.getAllByTestId('hbar-list-item');
		expect(rows.map((r) => within(r).getByText(/Alpha|Beta|Nada|Zero/).textContent)).toEqual([
			'Alpha',
			'Beta',
			'Zero',
			'Nada'
		]);
		expect(within(rows[0]!).getByTestId('hbar-list-value').textContent).toBe('40');
		expect(within(rows[0]!).getByTestId('hbar-list-share').textContent).toBe('75%');
		expect(within(rows[3]!).getByTestId('hbar-list-value').textContent).toBe('—');
	});
	it('starts bars at zero and scales to the largest or a shared max', () => {
		const { container, rerender } = render(HBarList, { items, label: 'Top' });
		const widths = () =>
			[...container.querySelectorAll<HTMLElement>('li [style*="width"]')].map((e) => e.style.width);
		expect(widths()).toEqual(['100%', '25%', '0%', '0%']);
		rerender({ items, label: 'Top', max: 80 });
		expect(widths()[0]).toBe('50%');
	});
});

describe('DataTableAlternative', () => {
	const props = {
		caption: 'Seeders per day',
		columns: [
			{ key: 'day', label: 'Day' },
			{ key: 'n', label: 'Seeders', numeric: true }
		],
		rows: [{ day: 'Oct 21, 2026', n: '12' }, { day: 'Oct 22, 2026' }] as Record<string, string>[]
	};
	it('is an accessible table, visually hidden by default, with a dash for missing cells', () => {
		render(DataTableAlternative, props);
		const wrap = screen.getByTestId('data-table');
		expect(wrap.className).toContain('sr-only');
		expect(screen.getByRole('table', { name: 'Seeders per day' })).toBeTruthy();
		expect(screen.getAllByRole('columnheader')).toHaveLength(2);
		expect(screen.getAllByRole('cell')[3]!.textContent).toBe('—');
	});
	it('can be shown', () => {
		render(DataTableAlternative, { ...props, visible: true });
		expect(screen.getByTestId('data-table').className).not.toContain('sr-only');
	});
});

describe('Histogram', () => {
	const base = {
		counts: [0, 4, null, 8],
		edges: ['1 KiB', '2 KiB', '4 KiB', '8 KiB', '16 KiB'],
		label: 'Sizes',
		handleLabels: { from: 'Lower', to: 'Upper' }
	};
	it('draws empty and unknown bins empty and others from a zero baseline', () => {
		const { container } = render(Histogram, base);
		const bins = screen.getAllByTestId('histogram-bin');
		expect(bins.map((b) => b.dataset.empty)).toEqual(['true', 'false', 'true', 'false']);
		const heights = [...container.querySelectorAll<HTMLElement>('[style*="height"]')].map(
			(e) => e.style.height
		);
		expect(heights).toEqual(['50%', '100%']);
	});
	it('labels the first and last edges', () => {
		render(Histogram, base);
		const edges = screen.getAllByTestId('histogram-edge').map((e) => e.textContent);
		expect(edges[0]).toBe('1 KiB');
		expect(edges.at(-1)).toBe('16 KiB');
	});
	it('has no brush without handle labels', () => {
		render(Histogram, { ...base, handleLabels: undefined });
		expect(screen.queryByRole('slider')).toBeNull();
	});
	it('starts a brush from the keyboard and moves edges with the arrow keys', async () => {
		const onbrush = vi.fn();
		render(Histogram, { ...base, onbrush });
		const plot = screen.getByRole('group', { name: 'Sizes' });
		plot.focus();
		await fireEvent.keyDown(plot, { key: 'Enter' });
		expect(onbrush).toHaveBeenLastCalledWith({ from: 0, to: 4 });
		const to = screen.getByRole('slider', { name: 'Upper' });
		await fireEvent.keyDown(to, { key: 'ArrowLeft' });
		expect(onbrush).toHaveBeenLastCalledWith({ from: 0, to: 3 });
		expect(to.getAttribute('aria-valuenow')).toBe('3');
		expect(to.getAttribute('aria-valuetext')).toBe('8 KiB');
		const from = screen.getByRole('slider', { name: 'Lower' });
		await fireEvent.keyDown(from, { key: 'ArrowRight' });
		expect(onbrush).toHaveBeenLastCalledWith({ from: 1, to: 3 });
		await fireEvent.keyDown(from, { key: 'End' });
		expect(onbrush).toHaveBeenLastCalledWith({ from: 2, to: 3 });
	});
	it('clears with Escape and returns focus to the plot', async () => {
		const onbrush = vi.fn();
		render(Histogram, { ...base, selection: { from: 1, to: 3 }, onbrush });
		await fireEvent.keyDown(screen.getByRole('slider', { name: 'Lower' }), { key: 'Escape' });
		expect(onbrush).toHaveBeenLastCalledWith(null);
		expect(screen.queryByRole('slider')).toBeNull();
		expect(document.activeElement).toBe(screen.getByRole('group', { name: 'Sizes' }));
	});
	it('selects a bin range with the pointer', async () => {
		const onbrush = vi.fn();
		render(Histogram, { ...base, onbrush });
		const plot = screen.getByRole('group', { name: 'Sizes' });
		plot.getBoundingClientRect = () => ({ left: 0, width: 400 }) as DOMRect;
		await fireEvent.pointerDown(plot, { clientX: 100 });
		expect(onbrush).toHaveBeenLastCalledWith({ from: 1, to: 2 });
		await fireEvent.pointerMove(plot, { clientX: 300 });
		expect(onbrush).toHaveBeenLastCalledWith({ from: 1, to: 3 });
		await fireEvent.pointerUp(plot);
		await fireEvent.pointerMove(plot, { clientX: 400 });
		expect(onbrush).toHaveBeenCalledTimes(2);
	});
});
