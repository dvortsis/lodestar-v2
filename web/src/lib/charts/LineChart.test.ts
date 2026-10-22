import { render, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const destroy = vi.fn();
const setSize = vi.fn();
const setData = vi.fn();
const ctor = vi.fn();
vi.mock('uplot', () => ({
	default: class {
		constructor(...args: unknown[]) {
			ctor(...args);
		}
		destroy = destroy;
		setSize = setSize;
		setData = setData;
	}
}));
vi.mock('uplot/dist/uPlot.min.css', () => ({}));

let resizeCb: () => void = () => {};
beforeEach(() => {
	destroy.mockClear();
	setSize.mockClear();
	setData.mockClear();
	ctor.mockClear();
	vi.stubGlobal(
		'ResizeObserver',
		class {
			constructor(cb: () => void) {
				resizeCb = cb;
			}
			observe() {}
			disconnect() {}
		}
	);
});

const props = {
	times: [1761000000, 1761086400, 1761172800],
	series: [{ label: 'Seeders', values: [1, null, 3] }],
	label: 'Seeders over three days',
	unit: 'seeders'
};

describe('LineChart', () => {
	it('loads uPlot lazily, passes gaps through and marks itself ready', async () => {
		const { default: LineChart } = await import('./LineChart.svelte');
		const { getByTestId } = render(LineChart, props);
		await waitFor(() => expect(getByTestId('line-chart').dataset.ready).toBe('true'));
		expect(ctor).toHaveBeenCalledTimes(1);
		const [opts, data] = ctor.mock.calls[0]!;
		expect(data[1]).toEqual([1, null, 3]);
		expect(opts.axes[1].label).toBe('seeders');
		expect(opts.series[0].value(null, 1761000000)).toMatch(/2025/);
	});
	it('resizes with its container and destroys on unmount', async () => {
		const { default: LineChart } = await import('./LineChart.svelte');
		const { getByTestId, unmount } = render(LineChart, props);
		await waitFor(() => expect(getByTestId('line-chart').dataset.ready).toBe('true'));
		resizeCb();
		expect(setSize).toHaveBeenCalled();
		unmount();
		expect(destroy).toHaveBeenCalledTimes(1);
	});
	it('updates data in place and rebuilds only when the shape changes', async () => {
		const { default: LineChart } = await import('./LineChart.svelte');
		const { getByTestId, rerender } = render(LineChart, props);
		await waitFor(() => expect(getByTestId('line-chart').dataset.ready).toBe('true'));
		await rerender({ ...props, series: [{ label: 'Seeders', values: [2, 3, 4] }] });
		await waitFor(() => expect(setData).toHaveBeenCalled());
		expect(setData.mock.calls.at(-1)![0][1]).toEqual([2, 3, 4]);
		expect(ctor).toHaveBeenCalledTimes(1);
		await rerender({ ...props, unit: 'peers' });
		await waitFor(() => expect(ctor).toHaveBeenCalledTimes(2));
	});
	it('uses the full date in the tooltip header and the date for day ticks', async () => {
		const { default: LineChart } = await import('./LineChart.svelte');
		const { getByTestId } = render(LineChart, { ...props, timeZone: 'UTC' });
		await waitFor(() => expect(getByTestId('line-chart').dataset.ready).toBe('true'));
		const [opts] = ctor.mock.calls[0]!;
		expect(opts.series[0].value(null, 1761000000)).toBe('Mon, Oct 20, 2025');
		expect(opts.axes[0].values(null, [1761000000])).toEqual(['Oct 20, 2025']);
		expect(opts.axes[1].values(null, [1200])).toEqual(['1.2K']);
		expect(opts.series[1].value(null, 1200)).toBe('1,200');
	});
	it('rebuilds when the theme changes', async () => {
		const { default: LineChart } = await import('./LineChart.svelte');
		const { getByTestId } = render(LineChart, props);
		await waitFor(() => expect(getByTestId('line-chart').dataset.ready).toBe('true'));
		document.documentElement.setAttribute('data-theme', 'nord-dark');
		await waitFor(() => expect(ctor).toHaveBeenCalledTimes(2));
		expect(destroy).toHaveBeenCalledTimes(1);
		document.documentElement.removeAttribute('data-theme');
	});
	it('does not create a chart if unmounted before uPlot loads', async () => {
		const { default: LineChart } = await import('./LineChart.svelte');
		const { unmount } = render(LineChart, props);
		unmount();
		await new Promise((r) => setTimeout(r, 20));
		expect(ctor).not.toHaveBeenCalled();
	});
});
