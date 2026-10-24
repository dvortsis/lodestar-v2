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
		expect(text('r-seeders')).toBe('12');
		expect(text('r-leechers')).toBe('3');
		expect(text('r-trackers')).toContain('4 / 20');
		expect(text('r-dominant')).toBe('82% Video');
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
		expect(
			screen.getByTestId('r-trackers').querySelector('[aria-label="Never scraped"]')
		).toBeTruthy();
		expect(text('r-files')).toBe('1 file');
	});

	it('marks estimated counts with ≈, and a suspect swarm and a backfill in words', () => {
		render(ResultRow, {
			hit: hit(9, { seeders_approx: true, swarm_suspect: true, flags: ['backfill'] }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(text('r-seeders')).toContain('≈');
		expect(text('r-leechers')).toContain('≈');
		expect(text('r-suspect')).toContain('Suspect seeders');
		expect(screen.getByTestId('r-backfill')).toBeTruthy();
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

	it('shows at most five badges: the type (or the adult kind), then the flags', () => {
		render(ResultRow, {
			hit: hit(4, {
				content_type: 'adult',
				adult_kind: 'vr',
				flags: ['private', 'v2', 'hybrid', 'imported', 'backfill']
			}),
			density: 'comfortable',
			testid: 'r'
		});
		const badges = screen.getByTestId('r-badges').querySelectorAll('.rr-badge');
		expect([...badges].map((b) => b.getAttribute('data-testid'))).toEqual([
			'r-badge-type',
			'r-badge-private',
			'r-badge-v2',
			'r-badge-hybrid',
			'r-badge-imported'
		]);
		expect(text('r-badge-type')).toBe('VR');
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

	it('keeps the suspect and backfill words for screen readers', () => {
		render(ResultRow, {
			hit: hit(9, { swarm_suspect: true, flags: ['backfill'] }),
			density: 'compact',
			testid: 'r'
		});
		expect(screen.getByTestId('r-suspect').className).toContain('sr-only');
		expect(screen.getByTestId('r-backfill').textContent).toContain('backfill');
	});
});
