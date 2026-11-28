import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render, screen } from '@testing-library/svelte';
import States from './States.svelte';
import type { CrawlerEvent } from './state.svelte';

afterEach(cleanup);

const live = (over: Partial<CrawlerEvent> = {}): CrawlerEvent => ({
	at: 1,
	dht_nodes: 812,
	fetch_active: 4,
	fetch_pending: 10,
	fetched: 3400,
	harvested: 91000,
	paused: false,
	stored: 3100,
	torrents: 48211,
	...over
});
const text = (id: string) => screen.getByTestId(`insights-starting-${id}`).textContent?.trim();

describe('the first-hour pipeline (starting scene)', () => {
	it('shows nodes found, harvested, metadata fetched and stored from the live feed', () => {
		render(States, { kind: 'starting', live: live() });
		expect(text('nodes')).toBe('812');
		expect(text('harvested')).toBe('91,000');
		expect(text('fetched')).toBe('3,400');
		expect(text('stored')).toBe('3,100');
	});

	it('shows "—" before the first report and for a DHT that is off, never 0', () => {
		render(States, { kind: 'starting', live: undefined });
		for (const id of ['nodes', 'harvested', 'fetched', 'stored']) expect(text(id)).toBe('—');
		cleanup();
		render(States, { kind: 'starting', live: live({ dht_nodes: null }) });
		expect(text('nodes')).toBe('—');
	});

	it('is honest about speed: days and weeks, no minutes or hours promised', () => {
		render(States, { kind: 'starting', live: live() });
		const scene = screen.getByTestId('insights-starting').textContent ?? '';
		expect(scene).toContain('days and weeks');
		expect(scene).not.toMatch(/within minutes|\d\s*(h|hours?)\b/i);
	});

	it('the counters are a labelled list, not colour alone', () => {
		render(States, { kind: 'starting', live: live() });
		const group = screen.getByRole('group', { name: 'Counts since this start' });
		expect(group.querySelector('dl')).toBeTruthy();
	});
});
