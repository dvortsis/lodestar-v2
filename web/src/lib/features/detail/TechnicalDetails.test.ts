import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { searchHit, torrentDetail } from './fixtures';
import TechnicalDetails from './TechnicalDetails.svelte';

const text = (id: string) => screen.getByTestId(id).textContent!.replace(/\s+/g, ' ').trim();

describe('TechnicalDetails', () => {
	it('is a closed disclosure holding piece size, hash, metadata time and the junk score', () => {
		render(TechnicalDetails, { detail: torrentDetail(), hit: searchHit() });
		expect((screen.getByTestId('detail-technical') as HTMLDetailsElement).open).toBe(false);
		expect(text('detail-piece-size')).toBe('4.00 MiB');
		expect(text('detail-hash')).toBe('0123456789abcdef0123456789abcdef01234567');
		expect(text('detail-junk-score')).toBe('0');
		expect(screen.queryByTestId('detail-hash-v2')).toBeNull();
	});

	it('shows the flags as badges with text, the v2 hash, and junk reasons', () => {
		render(TechnicalDetails, {
			detail: torrentDetail({
				flags: ['private', 'v2', 'hybrid', 'files_partial'],
				hash_v2: 'f'.repeat(64)
			}),
			hit: searchHit({ junk_score: 70, junk_reasons: ['tiny-video'] })
		});
		const flags = screen.getByTestId('detail-flags');
		for (const label of ['Private', 'v2', 'Hybrid', 'Partial file list'])
			expect(within(flags).getByText(label)).toBeTruthy();
		expect(text('detail-hash-v2')).toBe('f'.repeat(64));
		expect(text('detail-junk-score')).toBe('70');
		expect(text('detail-junk-reasons')).toContain('too small');
	});

	it('without the hit the junk score is a dash, not 0', () => {
		render(TechnicalDetails, { detail: torrentDetail(), hit: null });
		expect(text('detail-junk-score')).toBe('—');
	});
});
