import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/svelte';

const handlers = vi.hoisted(() => [] as ((d: Record<string, number | boolean | null>) => void)[]);
vi.mock('$lib/state/events.svelte', () => ({
	events: {
		on: (_topic: string, fn: (typeof handlers)[number]) => {
			handlers.push(fn);
			return () => handlers.splice(handlers.indexOf(fn), 1);
		}
	}
}));
import LiveCounts from './LiveCounts.svelte';

afterEach(cleanup);

describe('wizard live counts', () => {
	it('shows "—" until the crawler reports, then the server’s numbers', async () => {
		render(LiveCounts);
		for (const id of ['nodes', 'harvested', 'fetched', 'stored']) {
			expect(screen.getByTestId(`wizard-live-${id}`).textContent).toBe('—');
		}
		handlers[0]!({ dht_nodes: 812, harvested: 91000, fetched: 3400, stored: 3100, paused: false });
		await waitFor(() => expect(screen.getByTestId('wizard-live-nodes').textContent).toBe('812'));
		expect(screen.getByTestId('wizard-live-harvested').textContent).toBe('91,000');
		expect(screen.getByTestId('wizard-live-stored').textContent).toBe('3,100');
	});

	it('a DHT that is off (null nodes) stays "—", never 0', async () => {
		render(LiveCounts);
		handlers[0]!({ dht_nodes: null, harvested: 0, fetched: 0, stored: 0, paused: false });
		await waitFor(() => expect(screen.getByTestId('wizard-live-stored').textContent).toBe('0'));
		expect(screen.getByTestId('wizard-live-nodes').textContent).toBe('—');
	});

	it('stops listening when it goes away', () => {
		const { unmount } = render(LiveCounts);
		expect(handlers).toHaveLength(1);
		unmount();
		expect(handlers).toHaveLength(0);
	});
});
