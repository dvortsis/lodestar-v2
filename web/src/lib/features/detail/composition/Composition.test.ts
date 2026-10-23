import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import Composition from './Composition.svelte';

const share = (group: string, bytes: number, pct_bytes: number, files = 1) => ({
	group,
	files,
	bytes,
	pct_bytes,
	pct_files: 0
});

describe('Composition', () => {
	const mix = [
		share('documents', 2048, 1, 1),
		share('video', 2_900_000_000, 82, 2),
		share('audio', 600_000_000, 17, 9)
	];

	it('draws the bar in the fixed order with a text summary and the dominant share', () => {
		const { container } = render(Composition, { composition: mix });
		const segments = [...container.querySelectorAll<HTMLElement>('[data-group]')];
		expect(segments.map((s) => s.dataset.group)).toEqual(['video', 'audio', 'documents']);
		expect(screen.getByTestId('detail-compbar-bar').getAttribute('aria-label')).toBe(
			'82% video, 17% audio, 1% documents'
		);
		expect(screen.getByTestId('detail-compbar-dominant').textContent).toBe('82% video');
	});

	it('lists the groups largest first with file counts, sizes, server shares and the bar colour', () => {
		const { container } = render(Composition, { composition: mix });
		const items = screen.getAllByTestId('detail-groups-item');
		expect(items.map((i) => within(i).getByTestId('detail-groups-value').textContent)).toEqual([
			'2.70 GiB',
			'572 MiB',
			'2.00 KiB'
		]);
		expect(items[0]!.textContent).toContain('Video · 2');
		expect(items[1]!.textContent).toContain('Audio · 9');
		expect(within(items[0]!).getByTestId('detail-groups-share').textContent).toBe('82%');
		const fills = [...container.querySelectorAll<HTMLElement>('ul li [style*="width"]')];
		expect(fills.map((f) => f.style.backgroundColor)).toEqual([
			'var(--data-1)',
			'var(--data-2)',
			'var(--data-4)'
		]);
	});

	it('maps an unknown group to Other rather than dropping it', () => {
		render(Composition, { composition: [share('holograms', 10, 100)] });
		expect(screen.getByTestId('detail-groups-item').textContent).toContain('Other · 1');
	});

	it('says so in one line when there is nothing to show (padding only)', () => {
		render(Composition, { composition: [] });
		expect(screen.getByTestId('detail-composition-empty').textContent).toContain('No file types');
		expect(screen.queryByTestId('detail-compbar')).toBeNull();
		expect(screen.queryByTestId('detail-groups')).toBeNull();
	});

	it('says "not classified" while the hit is unknown', () => {
		render(Composition, { composition: undefined });
		expect(screen.getByTestId('detail-composition-unknown')).toBeTruthy();
	});
});
