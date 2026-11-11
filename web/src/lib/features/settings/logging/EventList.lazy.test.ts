// The event list fetches names only for the groups that are open: closed groups render no rows, so
// opening one group loads that component's chunk and no other.
import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { describe, expect, it, vi } from 'vitest';

const loaded: string[] = [];
vi.mock('./event-names', async (original) => {
	const real = await original<typeof import('./event-names')>();
	return {
		...real,
		CHUNKS: Object.fromEntries(
			Object.entries(real.CHUNKS).map(([c, load]) => [
				c,
				() => {
					loaded.push(c);
					return load();
				}
			])
		)
	};
});

const { default: EventList } = await import('./EventList.svelte');
const ev = (id: string, component: string) => ({
	id,
	component,
	level: 'info',
	kinds: [],
	rate_per_hour: 0,
	required: false,
	opt_in: null,
	default_on: true,
	current: true
});
const events = [ev('app.start', 'system'), ev('cluster.listening', 'workers')];
const form = {
	specs: [],
	field: () => ({ value: {}, locked: false }),
	set: () => {}
};
const context = new Map([
	['logging-catalog', { catalog: { catalog: { events, presets: { temporary: null } }, now: 0 } }]
]);

describe('event list names', () => {
	it('loads nothing while every group is closed, then one chunk per opened group', async () => {
		const { getByTestId, queryByTestId } = render(EventList, {
			props: { form, key: 'logging.events', controlProps: {} } as never,
			context
		});
		expect(getByTestId('log-group-system')).toBeTruthy();
		expect(queryByTestId('log-event-app.start')).toBeNull();
		await tick();
		expect(loaded).toEqual([]);

		await fireEvent.click(getByTestId('log-group-workers'));
		await waitFor(() => expect(loaded).toEqual(['workers']), { timeout: 10_000 });
		expect(queryByTestId('log-event-cluster.listening')).not.toBeNull();
		expect(queryByTestId('log-event-app.start')).toBeNull();
	});
});
