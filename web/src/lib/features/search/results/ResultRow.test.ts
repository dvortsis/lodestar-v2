import { describe, expect, it } from 'vitest';
import { createRawSnippet } from 'svelte';
import { render, screen } from '@testing-library/svelte';
import ResultRow from './ResultRow.svelte';
import type { SearchHit } from './ResultRow.svelte';
import { hit } from './fixtures';

const text = (id: string) => screen.getByTestId(id).textContent?.replace(/\s+/g, ' ').trim();

describe('ResultRow, Comfortable', () => {
	it('shows the facts with the server’s values', () => {
		render(ResultRow, {
			hit: hit(7, { seeders: 12, leechers: 3 }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(text('r-title')).toBe('Lantern Keeper 7');
		expect(text('r-raw')).toBe('Lantern.Keeper.7');
		expect(text('r-size')).toBe('2.00 GiB');
		expect(text('r-files')).toBe('5 files');
		expect(text('r-seeders')).toBe('12 seeders');
		expect(text('r-dominant')).toBe('82% video');
		expect(screen.getByRole('img', { name: /82% video, 11% audio/i })).toBeTruthy();
		expect(screen.getByTestId('r').dataset.density).toBe('comfortable');
	});

	it('says how the title was resolved, and hides the raw name when it is the title', () => {
		const { unmount } = render(ResultRow, {
			hit: hit(1, { resolved_via: 'folder' }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(screen.getByTestId('r-via')).toBeTruthy();
		unmount();
		render(ResultRow, {
			hit: hit(2, { title: 'Lantern.Keeper.2' }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(screen.queryByTestId('r-raw')).toBeNull();
	});

	it('says so for null values: no title, never scraped, not classified', () => {
		render(ResultRow, {
			hit: hit(8, {
				title: null,
				composition: [],
				seeders: null,
				leechers: null,
				trackers_checked: null,
				trackers_reporting: null,
				files: 1
			}),
			density: 'comfortable',
			testid: 'r'
		});
		expect(text('r-title')).toBe('Lantern.Keeper.8');
		expect(screen.queryByTestId('r-raw')).toBeNull();
		expect(text('r-composition')).toBe('Not classified yet');
		expect(screen.getByTestId('r-never-scraped').getAttribute('aria-label')).toBe('Never scraped');
		expect(text('r-files')).toBe('1 file');
	});

	it('says one seeder in the singular and marks an estimate with ≈ (also for one)', () => {
		const { unmount } = render(ResultRow, {
			hit: hit(9, { seeders: 1 }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(text('r-seeders')).toBe('1 seeder');
		unmount();
		const est = render(ResultRow, {
			hit: hit(9, { seeders: 1, seeders_approx: true }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(text('r-seeders')).toContain('≈');
		est.unmount();
		render(ResultRow, {
			hit: hit(9, { seeders_approx: true, swarm_suspect: true }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(text('r-seeders')).toContain('≈');
		expect(text('r-suspect')).toContain('Suspect seeders');
	});

	it('leaves the technical facts to the drawer: leechers, trackers, backfill', () => {
		render(ResultRow, {
			hit: hit(9, { flags: ['backfill', 'v2', 'hybrid'] }),
			density: 'comfortable',
			testid: 'r'
		});
		for (const gone of ['r-leechers', 'r-trackers', 'r-backfill'])
			expect(screen.queryByTestId(gone)).toBeNull();
	});

	it('shows bidirectional controls in a name as markers', () => {
		render(ResultRow, {
			hit: hit(3, { title: 'evil‮txt.exe', name: 'raw‮name' }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(text('r-title')).not.toContain('‮');
		expect(text('r-raw')).not.toContain('‮');
	});

	it('shows up to five badges and collapses the rest into a +N chip', () => {
		render(ResultRow, {
			hit: hit(4, {
				content_type: 'movie',
				resolution: '1080p',
				flags: ['private', 'imported', 'v2', 'hybrid']
			}),
			density: 'comfortable',
			testid: 'r'
		});
		// Six facts (type, res, private, imported, v2, hybrid): four show, the chip counts two.
		const chip = screen.getByTestId('r-badges-more');
		expect(chip.tagName).toBe('BUTTON');
		expect(chip.getAttribute('aria-label')).toMatch(/more facts/);
		expect(chip.textContent).toBe('+2');
		expect(screen.getByTestId('r-badge-type')).toBeTruthy();
		expect(screen.queryByTestId('r-badge-hybrid')).toBeNull();
	});

	it('never collapses safety or verification into the chip', () => {
		render(ResultRow, {
			hit: hit(4, {
				content_type: 'movie',
				resolution: '1080p',
				flags: ['private', 'v2', 'hybrid'],
				verified: false,
				swarm_suspect: true
			}),
			density: 'comfortable',
			testid: 'r'
		});
		expect(screen.getByTestId('unverified-badge')).toBeTruthy();
		expect(screen.getByTestId('r-suspect')).toBeTruthy();
		expect(screen.getByTestId('r-badges-more').textContent).toBe('+3');
	});

	it('shows the adult kind as the type badge', () => {
		render(ResultRow, {
			hit: hit(4, { content_type: 'adult', adult_kind: 'vr' }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(text('r-badge-type')).toBe('VR');
	});

	it('makes badges buttons only where the row can filter (it has actions)', () => {
		const actions = createRawSnippet<[SearchHit]>(() => ({ render: () => '<span></span>' }));
		const plain = render(ResultRow, {
			hit: hit(1, { content_type: 'movie' }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(screen.getByTestId('r-badge-type').tagName).toBe('SPAN');
		plain.unmount();
		render(ResultRow, {
			hit: hit(1, { content_type: 'movie' }),
			density: 'comfortable',
			testid: 'r',
			actions
		});
		const b = screen.getByTestId('r-badge-type');
		expect(b.tagName).toBe('BUTTON');
		expect(b.getAttribute('aria-label')).toBe('Filter by Movie');
	});

	it('has no badges for an unclassified torrent without flags', () => {
		render(ResultRow, {
			hit: hit(5, { content_type: null }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(screen.queryByTestId('r-badges')).toBeNull();
	});

	it('shows the matched files line only for a file query', () => {
		const { unmount } = render(ResultRow, {
			hit: hit(6, { matched_files: [0, 2, 3] }),
			density: 'comfortable',
			testid: 'r',
			matched: true
		});
		expect(text('r-matched')).toBe('Matched in 3 files');
		unmount();
		render(ResultRow, { hit: hit(6), density: 'comfortable', testid: 'r' });
		expect(screen.queryByTestId('r-matched')).toBeNull();
	});

	it('renders a feature’s extra fact and the actions', () => {
		render(ResultRow, {
			hit: hit(1),
			density: 'comfortable',
			testid: 'r',
			extra: createRawSnippet(() => ({ render: () => '<span data-testid="extra">Matched</span>' })),
			actions: createRawSnippet<[SearchHit]>((h) => ({
				render: () => `<button data-testid="act">${h().hash.slice(-1)}</button>`
			}))
		});
		expect(text('extra')).toBe('Matched');
		expect(text('act')).toBe('1');
	});
});

describe('ResultRow, Compact', () => {
	it('is one line: title, size, seeders and discovered only', () => {
		render(ResultRow, { hit: hit(7, { resolved_via: 'files' }), density: 'compact', testid: 'r' });
		expect(text('r-title')).toBe('Lantern Keeper 7');
		expect(screen.getByTestId('r-size')).toBeTruthy();
		expect(screen.getByTestId('r-seeders')).toBeTruthy();
		expect(screen.getByTestId('r-discovered')).toBeTruthy();
		for (const gone of [
			'r-raw',
			'r-via',
			'r-badges',
			'r-composition',
			'r-files',
			'r-leechers',
			'r-trackers'
		])
			expect(screen.queryByTestId(gone)).toBeNull();
	});

	it('keeps the suspect words for screen readers', () => {
		render(ResultRow, { hit: hit(9, { swarm_suspect: true }), density: 'compact', testid: 'r' });
		expect(screen.getByTestId('r-suspect').className).toContain('sr-only');
	});
});

describe('ResultRow, backfill (Discover only)', () => {
	it('shows the marker only when asked, and only for a backfill discovery', () => {
		const flagged = hit(1, { flags: ['backfill'] });
		const { unmount } = render(ResultRow, { hit: flagged, density: 'comfortable', testid: 'r' });
		expect(screen.queryByTestId('r-backfill')).toBeNull();
		unmount();
		const on = render(ResultRow, {
			hit: flagged,
			density: 'comfortable',
			testid: 'r',
			backfill: true
		});
		expect(text('r-backfill')).toContain('backfill');
		on.unmount();
		render(ResultRow, { hit: hit(2), density: 'comfortable', testid: 'r', backfill: true });
		expect(screen.queryByTestId('r-backfill')).toBeNull();
	});
});
