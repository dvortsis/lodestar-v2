import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { addRecent, loadRecent } from '../assist/recent';

vi.mock('$app/state', async () => ({
	page: (await import('../query/testing.svelte')).testPage
}));
vi.mock('$lib/shell/status.svelte', async () => ({
	shellStatus: (await import('../results/testing.svelte')).testStatus
}));

const { default: Home } = await import('./Home.svelte');
const { visit } = await import('../query/testing.svelte');
const { setPill } = await import('../results/testing.svelte');

beforeEach(() => {
	visit('/search');
	setPill('unknown');
});
afterEach(() => localStorage.clear());

const props = { focusBar: () => {}, count: 2_400_000, saved: [] };

describe('Search home', () => {
	it('says what it is and what to type, with the count', () => {
		render(Home, props);
		expect(screen.getByTestId('results-empty-title').textContent).toBe('Search 2,400,000 torrents');
		expect(screen.getByTestId('results-empty').querySelectorAll('.rp-examples a')).toHaveLength(3);
	});

	it('does without the count', () => {
		render(Home, { ...props, count: null });
		expect(screen.getByTestId('results-empty-title').textContent).toBe('Search torrents');
	});

	it('lists recent searches as links, newest first, and shows none when there are none', () => {
		render(Home, props);
		expect(screen.queryByTestId('home-recent')).toBeNull();
		addRecent('one');
		addRecent('two');
		render(Home, props);
		const links = screen.getByTestId('home-recent').querySelectorAll('a');
		expect([...links].map((a) => a.textContent)).toEqual(['two', 'one']);
		expect(links[0]!.getAttribute('href')).toContain('q=two');
	});

	it('clears recent searches from the list and from storage', async () => {
		addRecent('one');
		render(Home, props);
		await fireEvent.click(screen.getByTestId('home-recent-clear'));
		await waitFor(() => expect(screen.queryByTestId('home-recent')).toBeNull());
		expect(loadRecent()).toEqual([]);
	});

	it('keeps a line for the health sentence in every state, and fills it by status', async () => {
		render(Home, props);
		const line = screen.getByTestId('home-health');
		expect(line.textContent).toBe('');
		setPill('crawling');
		await waitFor(() => expect(line.textContent).toContain('working'));
		setPill('stalled');
		await waitFor(() => expect(line.textContent).toContain('needs attention'));
	});
});
