import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { torrentDetail } from '../fixtures';
import Swarm from './Swarm.svelte';

const DAY = 86_400;
const sample = (i: number, s: number | null, l: number | null) => ({
	at: 1_700_000_000 + i * DAY,
	seeders: s,
	leechers: l
});

describe('Swarm', () => {
	it('draws the line with gaps, axis unit, full-date tooltips and a table alternative', () => {
		render(Swarm, {
			detail: torrentDetail({
				swarm_history: {
					daily: [sample(0, 5, 1), sample(1, 6, 2), sample(2, null, null), sample(3, 7, 2)],
					weekly: []
				}
			})
		});
		const chart = screen.getByTestId('detail-swarm-chart');
		expect(chart.textContent).toContain('Axis: peers');
		expect(chart.textContent).toContain('Daily samples');
		expect(screen.getByTestId('detail-swarm-max').textContent).toBe('7');
		const points = screen.getAllByTestId('detail-swarm-point');
		expect(points).toHaveLength(4);
		// the full date with the year, and a dash (not 0) for the missing sample
		expect(points[2]!.textContent).toMatch(/2023/);
		expect(points[2]!.textContent).toContain('Seeders —');
		const rows = within(screen.getByTestId('detail-swarm-table-alt')).getAllByRole('row');
		expect(rows[3]!.textContent).toContain('—');
	});

	it('falls back to weekly samples, and shows nothing for a one-point history', () => {
		const { unmount } = render(Swarm, {
			detail: torrentDetail({
				swarm_history: { daily: [sample(0, 5, 1)], weekly: [sample(0, 5, 1), sample(7, 6, 1)] }
			})
		});
		expect(screen.getByTestId('detail-swarm-resolution').textContent).toBe('Weekly samples');
		unmount();
		render(Swarm, {
			detail: torrentDetail({ swarm_history: { daily: [sample(0, 5, 1)], weekly: [] } })
		});
		expect(screen.queryByTestId('detail-swarm-chart')).toBeNull();
	});

	it('shows only the host of a tracker, never a passkey, in the text, a title or an aria label', () => {
		const url = 'https://tracker.example.org:8443/PASSKEY123456/announce?pk=SECRETQUERY';
		const { container } = render(Swarm, {
			detail: torrentDetail({
				swarm_trackers: [{ url, at: 1_700_000_000, seeders: 3, leechers: 1, completed: 9 }]
			})
		});
		expect(screen.getByTestId('detail-swarm-host').textContent).toBe('tracker.example.org');
		const html = container.innerHTML;
		for (const secret of ['PASSKEY123456', 'SECRETQUERY', '8443', '/announce'])
			expect(html, secret).not.toContain(secret);
	});

	it('labels the DHT numbers as an estimate and says so when there is nothing', () => {
		const { unmount } = render(Swarm, {
			detail: torrentDetail({ dht_seeds: 40, dht_peers: 90, last_scrape_at: null })
		});
		const dht = screen.getByTestId('detail-swarm-dht').textContent!;
		expect(dht).toContain('estimate');
		expect(dht).toMatch(/≈\s*40/);
		unmount();
		render(Swarm, {
			detail: torrentDetail({
				last_scrape_at: null,
				completed: null,
				seeders: null,
				leechers: null
			})
		});
		expect(screen.getByTestId('detail-swarm-empty').textContent).toContain('Never scraped');
	});

	it('shows "Seen …" with an absolute time for a scraped torrent', () => {
		const now = Date.now() / 1000;
		render(Swarm, { detail: torrentDetail({ last_scrape_at: now - 3 * 3600 }) });
		const age = screen.getByTestId('detail-scrape-age');
		expect(age.textContent).toMatch(/^\s*Seen 3 hours ago/);
		expect(within(age).getByTestId('detail-scrape-at').textContent).toMatch(/\d{4}/);
	});

	it('says "No swarm numbers yet" only when there is truly nothing', () => {
		const bare = {
			completed: null,
			seeders: null,
			leechers: null,
			swarm_history: null,
			swarm_trackers: [],
			dht_peers: null,
			dht_seeds: null
		};
		const { unmount } = render(Swarm, { detail: torrentDetail(bare) });
		expect(screen.getByTestId('detail-swarm-empty').textContent).toContain('No swarm numbers yet');
		unmount();
		// the completed count, a single sample and the summary's own counts are each data
		for (const over of [
			{ completed: 12 },
			{ swarm_history: { daily: [sample(0, 5, 1)], weekly: [] } },
			{ seeders: 120, leechers: 4 }
		]) {
			const view = render(Swarm, { detail: torrentDetail({ ...bare, ...over }) });
			expect(screen.queryByTestId('detail-swarm-empty'), JSON.stringify(over)).toBeNull();
			view.unmount();
		}
	});
});
