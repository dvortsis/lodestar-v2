// The screen waits for the rules' names only until they arrive or the wait is over (runner, on
// X-safety-toggles): a slow chunk shows the codes, with no extra spinner.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, render, screen } from '@testing-library/svelte';
import { mockFetch } from '../flows/testing/fetch';
import FiltersScreen from './FiltersScreen.svelte';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });

const overview = {
	allow: [],
	core: [{ id: 'CS-4a', on: true, floor: false, action: 'quarantine' }],
	cs1_entries: 0,
	exceptions: [],
	ip_ranges: 0,
	overrides: [],
	packs: [],
	pending_floor_removals: [],
	quarantine: 0,
	rules: [],
	subscriptions: [],
	version: 1
};

async function show() {
	const net = mockFetch();
	net.on('GET /filters', { body: overview });
	net.on('GET /filters/queue', { body: { decisions_30d: [], next_cursor: null, quarantine: [] } });
	net.on('GET /settings/content', {
		body: { revision: 1, fields: { show_adult: { value: false } } }
	});
	return render(FiltersScreen);
}

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

// A chunk that never arrives.
vi.mock('./rule-text', () => new Promise(() => {}));

describe('the rules’ names chunk is slow', () => {
	it('shows the screen with the codes once the wait is over, not before', async () => {
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		await show();
		// The overview is in, the names aren't: the loading state stays, with no new spinner.
		await vi.waitFor(() => expect(screen.getByRole('status').textContent).toBe('Loading filters'));
		await vi.advanceTimersByTimeAsync(1_000);
		expect(screen.queryByTestId('core-CS-4a')).toBeNull();
		await vi.advanceTimersByTimeAsync(600);
		vi.useRealTimers();
		const row = await screen.findByTestId('core-CS-4a');
		expect(row.querySelector('code')?.textContent).toBe('CS-4a');
		expect(screen.queryByTestId('rule-name-CS-4a')).toBeNull();
	});
});
