import { render, waitFor } from '@testing-library/svelte';
import { expect, it, vi } from 'vitest';
import LineChart from './LineChart.svelte';

vi.mock('uplot', () => {
	throw new Error('offline');
});
vi.mock('uplot/dist/uPlot.min.css', () => ({}));

it('reports a load failure through onerror and leaves the container empty', async () => {
	vi.stubGlobal(
		'ResizeObserver',
		class {
			observe() {}
			disconnect() {}
		}
	);
	const onerror = vi.fn();
	const { getByTestId } = render(LineChart, {
		times: [1],
		series: [{ label: 'a', values: [1] }],
		label: 'x',
		unit: 'u',
		onerror
	});
	await waitFor(() => expect(onerror).toHaveBeenCalled());
	expect(getByTestId('line-chart').dataset.failed).toBe('true');
	expect(getByTestId('line-chart').dataset.ready).toBe('false');
});
