// The screen waits for the rules' names only until they arrive or fail (runner, on
// X-safety-toggles): a failed chunk shows the codes. The slow case is names-slow.test.ts.
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

afterEach(() => vi.unstubAllGlobals());

vi.mock('./rule-text', () => {
	throw new Error('chunk failed');
});

describe('the rules’ names chunk fails', () => {
	it('shows the screen, with every rule named by its code', async () => {
		await show();
		const row = await screen.findByTestId('core-CS-4a');
		expect(row.querySelector('code')?.textContent).toBe('CS-4a');
		expect(screen.queryByTestId('rule-name-CS-4a')).toBeNull();
		expect(screen.getByTestId('core-switch-CS-4a').textContent?.trim()).toBe('Turn off CS-4a');
		// Still the danger style: it's what the change does, not what it's called.
		expect(screen.getByTestId('core-switch-CS-4a').className).toContain('btn-error');
	});
});
