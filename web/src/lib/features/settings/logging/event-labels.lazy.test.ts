// Each component's names are their own chunk, loaded when an event of that component is shown, and
// only then; a chunk that fails to load is tried again on the next use.
import { waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';

const loaded: string[] = [];
let failWorkers = true;
vi.mock('./event-names', async (original) => {
	const real = await original<typeof import('./event-names')>();
	const CHUNKS = Object.fromEntries(
		Object.entries(real.CHUNKS).map(([component, load]) => [
			component,
			() => {
				loaded.push(component);
				if (component === 'workers' && failWorkers) return Promise.reject(new Error('offline'));
				return load();
			}
		])
	);
	return { ...real, CHUNKS };
});

const { eventHelp, eventLabel } = await import('./event-labels.svelte');
const search = { id: 'alerts.search_unreadable', component: 'search' } as const;
const workers = { id: 'cluster.listening', component: 'workers' } as const;

describe('event names, per component', { timeout: 20_000 }, () => {
	it('loads only the chunk of a shown event', async () => {
		expect(eventLabel(search)).toBe('alerts.search_unreadable');
		await waitFor(() => expect(eventLabel(search)).not.toBe('alerts.search_unreadable'), {
			timeout: 10_000
		});
		expect(eventHelp(search)).toBeTruthy();
		expect(loaded).toEqual(['search']);
		eventLabel(search);
		expect(loaded).toEqual(['search']);
	});

	it('keeps the id when a chunk fails, and tries again on the next use', async () => {
		expect(eventLabel(workers)).toBe('cluster.listening');
		await waitFor(() => expect(loaded).toEqual(['search', 'workers']), { timeout: 10_000 });
		await Promise.resolve();
		expect(eventLabel(workers)).toBe('cluster.listening');
		failWorkers = false;
		await waitFor(
			() => {
				expect(eventLabel(workers)).not.toBe('cluster.listening');
			},
			{ timeout: 10_000 }
		);
		expect(loaded.filter((c) => c === 'workers').length).toBeGreaterThanOrEqual(2);
	});
});
