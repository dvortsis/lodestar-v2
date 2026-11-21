// A names chunk that arrives after the 1.5 s wait (runner, on X-safety-toggles): the screen shows
// the codes when the wait is over, then names the rows once the chunk is in.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, render, screen, waitFor } from '@testing-library/svelte';
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

// The chunk resolves only when the test opens the gate.
const gate = vi.hoisted(() => {
	let open!: () => void;
	const arrived = new Promise<void>((resolve) => (open = resolve));
	return { arrived, open };
});
vi.mock('./rule-text', async (importOriginal) => {
	await gate.arrived;
	return importOriginal();
});

describe('the rules’ names chunk arrives late', () => {
	it(
		'shows the codes when the wait is over, then the names once it arrives',
		{ timeout: 20_000 },
		async () => {
			vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
			await show();
			await vi.advanceTimersByTimeAsync(1_600);
			vi.useRealTimers();
			const row = await screen.findByTestId('core-CS-4a');
			expect(screen.queryByTestId('rule-name-CS-4a')).toBeNull();
			expect(row.querySelector('code')?.textContent).toBe('CS-4a');
			gate.open();
			await waitFor(() =>
				expect(screen.getByTestId('rule-name-CS-4a').textContent).toBe(
					'Child terms near sexual terms'
				)
			);
			expect(row.textContent).toContain('A word for a child near a sexual term');
			expect(screen.getByTestId('core-switch-CS-4a').textContent?.trim()).toBe(
				'Turn off Child terms near sexual terms'
			);
		}
	);
});
