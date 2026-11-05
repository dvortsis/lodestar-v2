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

describe('Provenance', () => {
	it('lists the sources joined by a dot, with DHT shortened', () => {
		render(Provenance, {
			provenance: {
				...base,
				sources: [src(), src({ id: 2, plugin: 'nyaa', name: 'Nyaa', first: false })]
			}
		});
		expect(screen.getByTestId('detail-found-by').textContent).toMatch(/Found by:\s*DHT\s*·\s*Nyaa/);
	});
	it('shows nothing for no sources and nothing withheld', () => {
		const { container } = render(Provenance, { provenance: base });
		expect(container.textContent!.trim()).toBe('');
	});
	it('says only "private source" when sources were withheld', () => {
		render(Provenance, { provenance: { ...base, sources: [src()], withheld: true } });
		const t = screen.getByTestId('detail-sources-withheld');
		expect(t.textContent).toContain('private source');
		expect(t.querySelector('svg')).toBeTruthy();
		expect(screen.getByTestId('detail-found-by').textContent).toMatch(/DHT/);
	});
	it('marks a private source for the admin with an icon and text', () => {
		render(Provenance, {
			provenance: {
				...base,
				sources: [src({ plugin: 'prowlarr', name: 'Prowlarr', private: true })]
			}
		});
		const el = screen.getByTestId('detail-source-prowlarr');
		expect(el.textContent).toContain('private');
		expect(el.querySelector('svg')).toBeTruthy();
	});
	it('shows the Unverified badge only when not verified', () => {
		const { unmount } = render(Provenance, { provenance: base });
		expect(screen.queryByTestId('unverified-badge')).toBeNull();
		unmount();
		render(Provenance, { provenance: { ...base, verified: false } });
		expect(screen.getByTestId('unverified-badge')).toBeTruthy();
	});
	it('lists the claim differences, with formatted numbers', () => {
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
		expect(screen.getByTestId('detail-claim-name-nyaa').textContent).toContain(
			'the name was "Show S01"; the metadata says "Show.S01.1080p"'
		);
		expect(screen.getByTestId('detail-claim-size-nyaa').textContent).toContain(
			'size was 1.00\u00a0KiB'
		);
		expect(screen.getByTestId('detail-claim-files-nyaa').textContent).toContain(
			'file count was 3; the metadata says 4'
		);
	});
	it('hides the section when claims match', () => {
		render(Provenance, {
			provenance: {
				...base,
				sources: [src({ claim: { matches: true, name: null, size: null, file_count: null } })]
			}
		});
		expect(screen.queryByTestId('detail-claim-differences')).toBeNull();
	});
});
