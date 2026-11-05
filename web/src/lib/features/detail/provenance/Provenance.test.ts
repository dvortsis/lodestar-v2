import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import Provenance from './Provenance.svelte';
import type { Provenance as P, ProvenanceSource } from './types';

const src = (over: Partial<ProvenanceSource> = {}): ProvenanceSource => ({
	id: 0,
	plugin: 'dht',
	name: 'Mainline DHT',
	first: true,
	first_seen_at: 1,
	private: false,
	claim: null,
	...over
});
const base: P = { network: 'bt', sources: [], verified: true, withheld: false };
const T = { timeout: 10_000 };

describe('Provenance', () => {
	it('lists the sources joined by a dot, with DHT shortened', () => {
		render(Provenance, {
			provenance: {
				...base,
				sources: [src(), src({ id: 2, plugin: 'nyaa', name: 'Nyaa', first: false })]
			}
		});
		expect(screen.getByTestId('detail-found-by').textContent).toMatch(/Found by:\s*DHT.*·.*Nyaa/);
	});
	it('gives screen readers a comma between sources', () => {
		render(Provenance, {
			provenance: {
				...base,
				sources: [src(), src({ id: 2, plugin: 'nyaa', name: 'Nyaa', first: false })]
			}
		});
		const sep = screen.getByTestId('detail-found-by').querySelector('.sr-only');
		expect(sep?.textContent?.trim()).toBe(',');
	});
	it('shows nothing for no sources and nothing withheld', () => {
		const { container } = render(Provenance, { provenance: base });
		expect(container.textContent!.trim()).toBe('');
	});
	it('notes withheld details without adding a source, even with no sources', () => {
		const { container } = render(Provenance, { provenance: { ...base, withheld: true } });
		expect(screen.queryByTestId('detail-found-by')).toBeNull();
		expect(screen.getByTestId('detail-sources-withheld').textContent).toContain('private sources');
		expect(container.textContent).not.toContain('Found by');
	});
	it('keeps a public source and adds the note when some were withheld', () => {
		render(Provenance, { provenance: { ...base, sources: [src()], withheld: true } });
		expect(screen.getByTestId('detail-found-by').textContent).toMatch(/DHT/);
		expect(screen.getByTestId('detail-sources-withheld')).toBeTruthy();
	});
	it('marks a private source for the admin in words', () => {
		render(Provenance, {
			provenance: {
				...base,
				sources: [src({ plugin: 'prowlarr', name: 'Prowlarr', private: true })]
			}
		});
		expect(screen.getByTestId('detail-source-prowlarr').textContent).toContain('(private)');
	});
	it('loads nothing about differences when claims match', () => {
		render(Provenance, {
			provenance: {
				...base,
				sources: [src({ claim: { matches: true, name: null, size: null, file_count: null } })]
			}
		});
		expect(screen.queryByTestId('detail-claim-differences')).toBeNull();
	});
	it('lists the claim differences, with formatted numbers', async () => {
		render(Provenance, {
			provenance: {
				...base,
				sources: [
					src({
						id: 2,
						plugin: 'nyaa',
						name: 'Nyaa',
						claim: {
							matches: false,
							name: { claimed: 'Show S01', verified: 'Show.S01.1080p' },
							size: { claimed: 1024, verified: 2048 },
							file_count: { claimed: 3, verified: 4 }
						}
					})
				]
			}
		});
		expect((await screen.findByTestId('detail-claim-name-nyaa', {}, T)).textContent).toContain(
			'the name was "Show S01"; the metadata says "Show.S01.1080p"'
		);
		expect(screen.getByTestId('detail-claim-size-nyaa').textContent).toContain('size was 1.00 KiB');
		expect(screen.getByTestId('detail-claim-files-nyaa').textContent).toContain(
			'file count was 3; the metadata says 4'
		);
	});
	it('lists differences from several sources and uses the given heading level', async () => {
		const claim = (n: number) => ({
			matches: false,
			name: null,
			size: null,
			file_count: { claimed: n, verified: 9 }
		});
		render(Provenance, {
			level: 2,
			provenance: {
				...base,
				sources: [
					src({ id: 2, plugin: 'nyaa', name: 'Nyaa', claim: claim(1) }),
					src({ id: 3, plugin: 'rss', name: 'RSS', claim: claim(2) })
				]
			}
		});
		expect(await screen.findByTestId('detail-claim-files-nyaa', {}, T)).toBeTruthy();
		expect(screen.getByTestId('detail-claim-files-rss')).toBeTruthy();
		expect(screen.getByRole('heading', { level: 2 }).textContent).toContain('Claims that differ');
	});
});
