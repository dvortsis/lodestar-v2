import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { searchHit, torrentDetail } from '../fixtures';
import Summary from './Summary.svelte';

const ids = [
	'detail-size',
	'detail-files',
	'detail-discovered',
	'detail-seeders',
	'detail-leechers',
	'detail-trackers'
];
const text = (id: string) => screen.getByTestId(id).textContent!.replace(/\s+/g, ' ').trim();

describe('Summary (swarm fields)', () => {
	it('shows Last seen as an absolute time', () => {
		render(Summary, { detail: torrentDetail({ last_seen_at: 1_700_200_000 }), hit: null });
		expect(text('detail-last-seen')).toMatch(/2023/);
	});
	it('puts ≈ on seeders only when the server says approximate', () => {
		const { unmount } = render(Summary, {
			detail: torrentDetail({ seeders_approx: true }),
			hit: null
		});
		expect(text('detail-seeders')).toMatch(/^≈\s*120$/);
		expect(text('detail-leechers')).toMatch(/^≈\s*4$/);
		unmount();
		render(Summary, { detail: torrentDetail({ seeders_approx: false }), hit: null });
		expect(text('detail-seeders')).toBe('120');
		expect(text('detail-leechers')).toBe('4');
	});
	it('labels suspect seeders with text and an icon', () => {
		render(Summary, { detail: torrentDetail({ swarm_suspect: true }), hit: null });
		const badge = screen.getByText('Suspect seeders');
		expect(badge.closest('span')!.querySelector('svg')).toBeTruthy();
	});
});

describe('Summary', () => {
	it('shows every number with a test id', () => {
		render(Summary, { detail: torrentDetail(), hit: searchHit() });
		for (const id of ids) expect(screen.getByTestId(id), id).toBeTruthy();
		expect(text('detail-size')).toBe('2.70 GiB');
		expect(text('detail-files')).toBe('3');
		expect(text('detail-seeders')).toBe('120');
		expect(text('detail-leechers')).toBe('4');
	});

	it('says "Reported by 4 of 20 trackers checked"', () => {
		render(Summary, { detail: torrentDetail(), hit: null });
		expect(text('detail-trackers')).toBe('Reported by 4 of 20 trackers checked');
	});

	it('never-scraped swarm: "—" with the words "Never scraped"', () => {
		render(Summary, {
			detail: torrentDetail({
				seeders: null,
				leechers: null,
				last_scrape_at: null,
				trackers_checked: null,
				trackers_reporting: null,
				completed: null
			}),
			hit: null
		});
		for (const id of ['detail-seeders', 'detail-leechers', 'detail-trackers']) {
			const el = screen.getByTestId(id);
			expect(el.querySelector('[aria-hidden="true"]')!.textContent).toBe('—');
			expect(el.textContent).toContain('Never scraped');
		}
	});

	it('keeps a scraped zero as 0, not a dash', () => {
		render(Summary, {
			detail: torrentDetail({
				seeders: 0,
				leechers: 0,
				trackers_reporting: 0,
				trackers_checked: 20
			}),
			hit: null
		});
		expect(text('detail-seeders')).toBe('0');
		expect(text('detail-leechers')).toBe('0');
		expect(text('detail-trackers')).toBe('Reported by 0 of 20 trackers checked');
	});

	it('marks a backfill discovery in words', () => {
		render(Summary, { detail: torrentDetail({ flags: ['backfill'] }), hit: null });
		expect(screen.getByText('First seen during backfill')).toBeTruthy();
	});
});

describe('Summary junk reasons', () => {
	it('lists reasons as text only when there are some, unknown keys as themselves', () => {
		const { unmount } = render(Summary, { detail: torrentDetail(), hit: searchHit() });
		expect(screen.queryByTestId('detail-junk-reasons')).toBeNull();
		unmount();
		render(Summary, {
			detail: torrentDetail(),
			hit: searchHit({ junk_score: 70, junk_reasons: ['double-extension', 'future-reason'] })
		});
		const items = within(screen.getByTestId('detail-junk-reasons')).getAllByRole('listitem');
		expect(items.map((li) => li.textContent)).toEqual([
			'A media file name hides a program (like movie.mp4.exe)',
			'future-reason'
		]);
	});
});
