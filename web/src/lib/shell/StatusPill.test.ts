import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import StatusPill from './StatusPill.svelte';
import type { PillState } from './status';

const pill = (state: PillState) => render(StatusPill, { state });
const none = { torrents: null, done: null, total: null };

describe('StatusPill', () => {
	it('is a link to Insights with an accessible name that keeps the words', () => {
		pill({ kind: 'crawling', ...none, torrents: 1_234_567 });
		const link = screen.getByTestId('status-pill');
		expect(link.getAttribute('href')).toBe('/insights');
		expect(link.textContent).toContain('Crawling');
		expect(link.textContent).toContain('Crawler status. Open Insights.');
		expect(link.querySelector('svg[aria-hidden="true"]')).toBeTruthy();
	});

	it('formats the server count with the compact formatter', () => {
		pill({ kind: 'crawling', ...none, torrents: 1_234_567 });
		expect(screen.getByTestId('status-pill-count').textContent).toBe('· 1.2M torrents');
	});

	it('says "1 torrent", not "1 torrents"', () => {
		pill({ kind: 'idle', ...none, torrents: 1 });
		expect(screen.getByTestId('status-pill-count').textContent).toBe('· 1 torrent');
	});

	it('shows zero as 0, and unknown as an em dash, never the other way round', () => {
		const { unmount } = pill({ kind: 'idle', ...none, torrents: 0 });
		expect(screen.getByTestId('status-pill-count').textContent).toBe('· 0 torrents');
		unmount();
		pill({ kind: 'idle', ...none, torrents: null });
		expect(screen.getByTestId('status-pill-count').textContent).toBe('· — torrents');
	});

	it('shows rebuild progress from the server numbers', () => {
		pill({ kind: 'rebuilding', torrents: null, done: 1500, total: 20000 });
		expect(screen.getByTestId('status-pill-text').textContent).toBe('Rebuilding index');
		expect(screen.getByTestId('status-pill-count').textContent).toBe('· 1,500 of 20,000');
	});

	it.each([
		['unknown', 'Status unknown'],
		['connecting', 'Connecting'],
		['reconnecting', 'Reconnecting'],
		['busy', 'Server busy'],
		['failed', 'Index rebuild failed']
	] as const)('shows %s as words with no count', (kind, words) => {
		pill({ kind, ...none });
		expect(screen.getByTestId('status-pill-text').textContent).toBe(words);
		expect(screen.queryByTestId('status-pill-count')).toBeNull();
		expect(screen.getByTestId('status-pill').getAttribute('data-state')).toBe(kind);
	});
});
