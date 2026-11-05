import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import Provenance from './Provenance.svelte';
import type { Provenance as P } from './types';

const base: P = { sources: [], verified: true, differences: [] };

describe('Provenance', () => {
	it('lists the sources joined by a dot', () => {
		render(Provenance, {
			provenance: {
				...base,
				sources: [
					{ name: 'DHT', first_seen_at: 1 },
					{ name: 'Nyaa', first_seen_at: 2 }
				]
			}
		});
		expect(screen.getByTestId('detail-found-by').textContent).toMatch(/Found by:\s*DHT · Nyaa/);
	});
	it('names nothing for a private source without a name', () => {
		render(Provenance, {
			provenance: { ...base, sources: [{ name: null, first_seen_at: 1 }] }
		});
		expect(screen.queryByTestId('detail-found-by')).toBeNull();
	});
	it('shows the Unverified badge only when not verified', () => {
		const { unmount } = render(Provenance, { provenance: base });
		expect(screen.queryByTestId('unverified-badge')).toBeNull();
		unmount();
		render(Provenance, { provenance: { ...base, verified: false } });
		expect(screen.getByTestId('unverified-badge')).toBeTruthy();
	});
	it('lists claim differences', () => {
		render(Provenance, {
			provenance: {
				...base,
				differences: [{ field: 'size', claimed: '1.0 GiB', verified: '2.7 GiB' }]
			}
		});
		expect(screen.getByTestId('detail-claim-differences').textContent).toContain(
			'Size: a source said 1.0 GiB; the metadata says 2.7 GiB'
		);
	});
	it('shows nothing for a verified item with one nameless source and no differences', () => {
		const { container } = render(Provenance, {
			provenance: { ...base, sources: [{ name: null, first_seen_at: 1 }] }
		});
		expect(container.textContent!.trim()).toBe('');
	});
});
